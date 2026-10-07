//! Приём пачек от агентов: `POST /v1/batches`, MessagePack + zstd.

use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::header::{AUTHORIZATION, CONTENT_ENCODING, CONTENT_TYPE, HeaderName};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use http_body::Body as _;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::agent_wire::{self, DecodeError};
use crate::core_wire::Source;
use crate::core_wire::{BatchEncoder, CoreBatch, Event, Group};
use crate::memory::{MAX_BODY, MAX_DECODED, Resources};
use crate::normalize;
use crate::spool::Spool;

#[derive(Clone)]
struct IngestState {
    spool: Spool,
    resources: Arc<Resources>,
}

pub fn router(spool: Spool, token: &str, resources: Arc<Resources>) -> Router {
    Router::new()
        .route("/v1/batches", post(ingest))
        .route_layer(middleware::from_fn_with_state(token_hash(token), authorize))
        .with_state(IngestState { spool, resources })
}

fn token_hash(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

async fn authorize(State(expected): State<[u8; 32]>, request: Request, next: Next) -> Response {
    let mut values = request.headers().get_all(AUTHORIZATION).iter();
    let token = values
        .next()
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("Bearer"))
        .map(|(_, token)| token);
    if values.next().is_some()
        || !token.is_some_and(|token| bool::from(token_hash(token).ct_eq(&expected)))
    {
        return (
            StatusCode::UNAUTHORIZED,
            [("www-authenticate", "Bearer")],
            "unauthorized",
        )
            .into_response();
    }
    next.run(request).await
}

async fn ingest(
    State(state): State<IngestState>,
    request: Request,
) -> Result<StatusCode, IngestError> {
    if !header_is(request.headers(), CONTENT_TYPE, "application/msgpack")
        || !header_is(request.headers(), CONTENT_ENCODING, "zstd")
    {
        return Err(IngestError::UnsupportedType);
    }

    if !state.resources.ready() {
        return Err(IngestError::Unavailable);
    }
    if !state.spool.available() {
        return Err(IngestError::BufferFull);
    }
    let permit = state
        .resources
        .slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| IngestError::Unavailable)?;
    let body = tokio::time::timeout(Duration::from_secs(30), read_body(request.into_body()))
        .await
        .map_err(|_| IngestError::Timeout)??;
    crate::memory::run_blocking(permit, move || {
        accept(&state.spool, &body, &state.resources)
    })
    .await
    .map_err(|error| {
        tracing::error!(%error, "ingest worker failed");
        IngestError::Unavailable
    })?
}

// Не collect(): число DATA frames тоже контролирует клиент. После копирования
// каждого фрагмента он освобождается, capacity тела никогда не растёт.
async fn read_body(mut body: Body) -> Result<Vec<u8>, IngestError> {
    let mut bytes = Vec::with_capacity(MAX_BODY);
    while let Some(frame) = std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)).await {
        let frame = frame.map_err(|_| IngestError::InvalidBody)?;
        if let Ok(data) = frame.into_data() {
            if data.len() > MAX_BODY - bytes.len() {
                return Err(IngestError::TooLarge);
            }
            bytes.extend_from_slice(&data);
        }
    }
    Ok(bytes)
}

fn accept(spool: &Spool, body: &[u8], resources: &Resources) -> Result<StatusCode, IngestError> {
    let mut budget = crate::model_budget::ModelBudget::new(resources.model_limit);
    let batch = agent_wire::decode_with_budget(body, MAX_DECODED, &mut budget)?;
    if batch.records.is_empty() {
        return Err(IngestError::EmptyBatch);
    }
    for record in &batch.records {
        let size = match &record.payload {
            agent_wire::Payload::Text(s) | agent_wire::Payload::Json(s) => s.len(),
            agent_wire::Payload::Binary(bytes) => bytes.len(),
        };
        if size > 2 << 20 {
            return Err(IngestError::TooLarge);
        }
    }
    let source = Source {
        environment: batch.environment,
        server: batch.server,
        backend: batch.backend,
        service: batch.service,
        service_instance: batch.service_instance,
    };
    budget
        .charge(
            batch
                .records
                .len()
                .checked_mul(std::mem::size_of::<Event>())
                .ok_or(IngestError::TooLarge)?,
        )
        .map_err(|_| IngestError::TooLarge)?;
    let mut events = Vec::with_capacity(batch.records.len());
    for record in batch.records {
        events.push(
            normalize::event_with_budget(record, &mut budget).map_err(|_| IngestError::TooLarge)?,
        );
    }
    let bytes = buffered_bytes(&source, &events, events.capacity());
    let _reservation = resources
        .buffered
        .reserve(bytes)
        .map_err(|error| match error {
            crate::memory::ReserveError::TooLarge => IngestError::TooLarge,
            crate::memory::ReserveError::Full => IngestError::BufferFull,
        })?;
    let batch = CoreBatch {
        dropped: 0,
        groups: vec![Group {
            source,
            dropped: batch.dropped,
            events,
        }],
    };
    let mut encoder = BatchEncoder::new(
        batch,
        resources.body_limit,
        resources.decoded_limit,
        resources.core_model_limit,
    );
    let parts = std::iter::from_fn(move || {
        let part = encoder.next();
        if encoder.take_dropped() != 0 {
            return Some(Err(crate::spool::Error::TooLarge));
        }
        part.map(|part| Ok((part.body, part.events as u64)))
    });
    spool.append_group(parts).map_err(|error| match error {
        crate::spool::Error::TooLarge => IngestError::TooLarge,
        crate::spool::Error::Full => IngestError::BufferFull,
        crate::spool::Error::Io(error) => {
            resources.set_ready(false);
            tracing::error!(%error, "durable commit failed; request not acknowledged");
            IngestError::Unavailable
        }
    })?;
    Ok(StatusCode::ACCEPTED)
}

fn header_is(headers: &HeaderMap, name: HeaderName, expected: &str) -> bool {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case(expected))
}

#[derive(Debug)]
enum IngestError {
    UnsupportedType,
    TooLarge,
    InvalidBody,
    EmptyBatch,
    BufferFull,
    Unavailable,
    Timeout,
}

impl From<DecodeError> for IngestError {
    fn from(err: DecodeError) -> Self {
        match err {
            DecodeError::TooLarge => Self::TooLarge,
            DecodeError::Invalid => Self::InvalidBody,
        }
    }
}

impl IntoResponse for IngestError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::UnsupportedType => (
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "expected Content-Type application/msgpack and Content-Encoding zstd",
            ),
            Self::TooLarge => (StatusCode::PAYLOAD_TOO_LARGE, "decoded batch is too large"),
            Self::InvalidBody => (
                StatusCode::BAD_REQUEST,
                "body is not a zstd-compressed MessagePack batch",
            ),
            Self::EmptyBatch => (
                StatusCode::BAD_REQUEST,
                "batch must contain at least one record",
            ),
            // Агент повторит пачку с backoff и пока продержит записи у себя.
            Self::BufferFull => (StatusCode::SERVICE_UNAVAILABLE, "relay buffer is full"),
            Self::Unavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                "relay ingest capacity is unavailable",
            ),
            Self::Timeout => (StatusCode::REQUEST_TIMEOUT, "request body timed out"),
        };
        (status, message).into_response()
    }
}

fn buffered_bytes(source: &Source, events: &[Event], capacity: usize) -> usize {
    let source_bytes = [&source.server, &source.backend, &source.service]
        .into_iter()
        .chain(source.environment.iter())
        .chain(source.service_instance.iter())
        .fold(0usize, |sum, text| sum.saturating_add(text.capacity()));
    events.iter().fold(
        4096usize
            .saturating_add(source_bytes)
            .saturating_add(capacity.saturating_mul(4 * std::mem::size_of::<Event>())),
        |sum, event| {
            let strings = [&event.message]
                .into_iter()
                .chain(event.action.iter())
                .chain(event.request_id.iter())
                .fold(0usize, |n, text| {
                    n.saturating_add(text.capacity().saturating_mul(2))
                });
            event.attributes.iter().fold(
                sum.saturating_add(strings)
                    .saturating_add(event.attributes.capacity().saturating_mul(256)),
                |n, (key, value)| {
                    n.saturating_add(key.capacity().saturating_mul(2))
                        .saturating_add(value_bytes(value))
                },
            )
        },
    )
}

fn value_bytes(value: &serde_json::Value) -> usize {
    use serde_json::Value;
    let heap = match value {
        Value::String(text) => text.capacity().saturating_mul(2),
        Value::Array(values) => values
            .iter()
            .fold(values.capacity().saturating_mul(256), |n, v| {
                n.saturating_add(value_bytes(v))
            }),
        Value::Object(values) => values.iter().fold(0usize, |n, (key, v)| {
            n.saturating_add(256)
                .saturating_add(key.capacity().saturating_mul(2))
                .saturating_add(value_bytes(v))
        }),
        _ => 0,
    };
    256usize.saturating_add(heap)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Bytes;
    use http_body::Frame;
    use std::convert::Infallible;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::{Context, Poll};
    use tower::ServiceExt;

    async fn setup() -> (tempfile::TempDir, Spool, Arc<Resources>) {
        let dir = tempfile::tempdir().unwrap();
        let spool = Spool::open(dir.path(), 1 << 20).await.unwrap();
        let config = crate::config::Config::from_lookup(|name| match name {
            "LOGNARA_CORE_URL" => Some("http://localhost/v1/batches".into()),
            "LOGNARA_CORE_TOKEN" | "LOGNARA_RELAY_TOKEN" => Some("secret".into()),
            _ => None,
        })
        .unwrap();
        (dir, spool, Resources::new(&config))
    }
    fn encoded(batch: &agent_wire::Batch) -> Vec<u8> {
        zstd::encode_all(&rmp_serde::to_vec_named(batch).unwrap()[..], 3).unwrap()
    }
    fn fixture() -> agent_wire::Batch {
        agent_wire::decode(include_bytes!("../tests/fixtures/agent-batch.bin"), 1024).unwrap()
    }
    struct Unreadable;
    impl http_body::Body for Unreadable {
        type Data = Bytes;
        type Error = Infallible;
        fn poll_frame(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
            panic!("rejected request body was read");
        }
    }
    fn request(body: Body, token: &str) -> Request {
        Request::builder()
            .method("POST")
            .uri("/v1/batches")
            .header(AUTHORIZATION, token)
            .header(CONTENT_TYPE, "application/msgpack")
            .header(CONTENT_ENCODING, "zstd")
            .body(body)
            .unwrap()
    }

    #[tokio::test]
    async fn auth_and_busy_admission_reject_before_reading_body() {
        let (_dir, spool, resources) = setup().await;
        let app = router(spool.clone(), "relay-secret", resources.clone());
        for token in [
            "Bearer wrong",
            "Basic relay-secret",
            "Bearer",
            "Bearer  relay-secret",
        ] {
            assert_eq!(
                app.clone()
                    .oneshot(request(Body::new(Unreadable), token))
                    .await
                    .unwrap()
                    .status(),
                StatusCode::UNAUTHORIZED
            );
        }
        let permit = resources.slots.clone().try_acquire_owned().unwrap();
        assert_eq!(
            app.clone()
                .oneshot(request(Body::new(Unreadable), "Bearer relay-secret"))
                .await
                .unwrap()
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        drop(permit);
        resources.set_ready(false);
        assert_eq!(
            app.oneshot(request(Body::new(Unreadable), "Bearer relay-secret"))
                .await
                .unwrap()
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert!(spool.is_empty());
    }

    #[tokio::test]
    async fn invalid_or_empty_batches_never_publish_any_part() {
        let (_dir, spool, resources) = setup().await;
        let mut batch = fixture();
        batch.records.clear();
        batch.dropped = 7;
        assert!(matches!(
            accept(&spool, &encoded(&batch), &resources),
            Err(IngestError::EmptyBatch)
        ));
        assert!(matches!(
            accept(&spool, b"invalid", &resources),
            Err(IngestError::InvalidBody)
        ));
        assert!(spool.is_empty());
        assert_eq!(resources.buffered.used(), 0);
    }

    #[tokio::test]
    async fn unencodable_last_record_rolls_back_entire_request() {
        let (_dir, spool, mut resources) = setup().await;
        Arc::get_mut(&mut resources).unwrap().decoded_limit = 400;
        let mut batch = fixture();
        batch.records = vec![
            agent_wire::Record {
                received_at: 1,
                payload: agent_wire::Payload::Text("small".into()),
            },
            agent_wire::Record {
                received_at: 2,
                payload: agent_wire::Payload::Text("x".repeat(1024)),
            },
        ];
        assert!(matches!(
            accept(&spool, &encoded(&batch), &resources),
            Err(IngestError::TooLarge)
        ));
        assert!(spool.is_empty());
    }

    #[tokio::test]
    async fn per_record_limit_and_disk_quota_are_checked_before_ack() {
        let (dir, spool, resources) = setup().await;
        let mut batch = fixture();
        batch.records = vec![agent_wire::Record {
            received_at: 1,
            payload: agent_wire::Payload::Text("x".repeat((2 << 20) + 1)),
        }];
        assert!(matches!(
            accept(&spool, &encoded(&batch), &resources),
            Err(IngestError::TooLarge)
        ));
        assert!(spool.is_empty());
        drop(spool);
        let spool = Spool::open(dir.path(), 1).await.unwrap();
        assert!(matches!(
            accept(&spool, &encoded(&fixture()), &resources),
            Err(IngestError::BufferFull)
        ));
        assert!(spool.is_empty());
    }

    #[tokio::test]
    async fn compressed_body_limit_releases_slot() {
        let (_dir, spool, resources) = setup().await;
        let app = router(spool, "relay-secret", resources.clone());
        assert_eq!(
            app.oneshot(request(
                Body::from(vec![0; MAX_BODY + 1]),
                "Bearer relay-secret"
            ))
            .await
            .unwrap()
            .status(),
            StatusCode::PAYLOAD_TOO_LARGE
        );
        assert_eq!(resources.slots.available_permits(), 1);
    }
    struct Fragment {
        live: Arc<AtomicUsize>,
        byte: u8,
    }

    impl AsRef<[u8]> for Fragment {
        fn as_ref(&self) -> &[u8] {
            std::slice::from_ref(&self.byte)
        }
    }

    impl Drop for Fragment {
        fn drop(&mut self) {
            self.live.fetch_sub(1, Ordering::SeqCst);
        }
    }

    struct FragmentedBody {
        left: usize,
        live: Arc<AtomicUsize>,
    }

    impl http_body::Body for FragmentedBody {
        type Data = Bytes;
        type Error = Infallible;

        fn poll_frame(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
            assert_eq!(
                self.live.load(Ordering::SeqCst),
                0,
                "previous frame retained"
            );
            if self.left == 0 {
                return Poll::Ready(None);
            }
            self.left -= 1;
            self.live.fetch_add(1, Ordering::SeqCst);
            Poll::Ready(Some(Ok(Frame::data(Bytes::from_owner(Fragment {
                live: self.live.clone(),
                byte: b'x',
            })))))
        }
    }

    #[tokio::test]
    async fn fragmented_body_releases_each_frame_and_keeps_fixed_capacity() {
        let live = Arc::new(AtomicUsize::new(0));
        let body = read_body(Body::new(FragmentedBody {
            left: 1024,
            live: live.clone(),
        }))
        .await
        .unwrap();
        assert_eq!(body, vec![b'x'; 1024]);
        assert_eq!(body.capacity(), MAX_BODY);
        assert_eq!(live.load(Ordering::SeqCst), 0);
    }
}
