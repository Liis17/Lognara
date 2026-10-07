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
use crate::buffer::Buffer;
use crate::core_wire::Source;
use crate::memory::{MAX_BODY, MAX_DECODED, Resources};
use crate::normalize;

#[derive(Clone)]
struct IngestState {
    buffer: Arc<Buffer>,
    resources: Arc<Resources>,
}

pub fn router(buffer: Arc<Buffer>, token: &str, resources: Arc<Resources>) -> Router {
    Router::new()
        .route("/v1/batches", post(ingest))
        .route_layer(middleware::from_fn_with_state(token_hash(token), authorize))
        .with_state(IngestState { buffer, resources })
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
    state.buffer.check_admission().map_err(IngestError::from)?;
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
        accept(&state.buffer, &body, state.resources.model_limit)
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

fn accept(buffer: &Buffer, body: &[u8], model_limit: usize) -> Result<StatusCode, IngestError> {
    let mut budget = crate::model_budget::ModelBudget::new(model_limit);
    let batch = agent_wire::decode_with_budget(body, MAX_DECODED, &mut budget)?;
    if batch.records.is_empty() {
        return Err(IngestError::EmptyBatch);
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
                .checked_mul(std::mem::size_of::<crate::core_wire::Event>())
                .ok_or(IngestError::TooLarge)?,
        )
        .map_err(|_| IngestError::TooLarge)?;
    let mut events = Vec::with_capacity(batch.records.len());
    for record in batch.records {
        events.push(
            normalize::event_with_budget(record, &mut budget).map_err(|_| IngestError::TooLarge)?,
        );
    }
    buffer
        .push(source, batch.dropped, events)
        .map_err(IngestError::from)?;
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

impl From<crate::buffer::Full> for IngestError {
    fn from(error: crate::buffer::Full) -> Self {
        match error {
            crate::buffer::Full::Busy => Self::BufferFull,
            crate::buffer::Full::TooLarge => Self::TooLarge,
        }
    }
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

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::{Context, Poll};

    use axum::body::Bytes;
    use axum::http::HeaderValue;
    use http_body::Frame;
    use tower::ServiceExt;

    use super::*;

    fn resources() -> Arc<Resources> {
        resources_with_model(crate::memory::DEFAULT_MODEL)
    }

    fn resources_with_model(model: usize) -> Arc<Resources> {
        let config = crate::config::Config::from_lookup(|name| match name {
            "LOGNARA_CORE_URL" => Some("http://localhost/v1/batches".into()),
            "LOGNARA_CORE_TOKEN" | "LOGNARA_RELAY_TOKEN" => Some("secret".into()),
            "LOGNARA_RELAY_MAX_MODEL_BYTES" => Some(model.to_string()),
            _ => None,
        })
        .unwrap();
        Resources::new(&config)
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

    #[test]
    fn rejects_empty_batches_without_changing_buffer_or_dropped() {
        let memory = crate::memory::Pool::new(crate::memory::DEFAULT_BUFFER);
        let buffer = Buffer::with_memory(1, 1, memory.clone());
        let mut batch =
            agent_wire::decode(include_bytes!("../tests/fixtures/agent-batch.bin"), 1024).unwrap();
        let record = batch.records[0].clone();
        batch.records.clear();
        for index in 0..100 {
            batch.service = format!("empty-{index}");
            batch.dropped = if index % 2 == 0 { 0 } else { 7 };
            let packed = rmp_serde::to_vec_named(&batch).unwrap();
            let body = zstd::encode_all(&packed[..], 3).unwrap();
            let error = accept(&buffer, &body, crate::memory::DEFAULT_MODEL).unwrap_err();
            assert_eq!(error.into_response().status(), StatusCode::BAD_REQUEST);
            assert_eq!(memory.used(), 0);
            assert!(buffer.take(true).is_none());
        }

        batch.records = vec![record];
        batch.dropped = 3;
        let packed = rmp_serde::to_vec_named(&batch).unwrap();
        let body = zstd::encode_all(&packed[..], 3).unwrap();
        assert_eq!(
            accept(&buffer, &body, crate::memory::DEFAULT_MODEL).unwrap(),
            StatusCode::ACCEPTED
        );
        let used = memory.used();

        batch.records.clear();
        batch.dropped = 7;
        let packed = rmp_serde::to_vec_named(&batch).unwrap();
        let body = zstd::encode_all(&packed[..], 3).unwrap();
        let error = accept(&buffer, &body, crate::memory::DEFAULT_MODEL).unwrap_err();
        assert_eq!(error.into_response().status(), StatusCode::BAD_REQUEST);
        assert_eq!(memory.used(), used);
        let taken = buffer.take(true).unwrap();
        assert_eq!(taken.len, 1);
        assert_eq!(taken.batch.groups.len(), 1);
        assert_eq!(taken.batch.groups[0].dropped, 3);
    }

    #[tokio::test]
    async fn minimum_configured_model_and_buffer_accept_a_text_record() {
        let resources = resources_with_model(8192);
        let buffer = Arc::new(Buffer::with_memory(
            100,
            100,
            crate::memory::Pool::new(8192),
        ));
        let mut batch =
            agent_wire::decode(include_bytes!("../tests/fixtures/agent-batch.bin"), 1024).unwrap();
        batch.records = vec![agent_wire::Record {
            received_at: 1,
            payload: agent_wire::Payload::Text("x".into()),
        }];
        let packed = rmp_serde::to_vec_named(&batch).unwrap();
        let body = zstd::encode_all(&packed[..], 3).unwrap();
        let app = router(buffer.clone(), "relay-secret", resources.clone());
        let request = Request::builder()
            .method("POST")
            .uri("/v1/batches")
            .header(AUTHORIZATION, "Bearer relay-secret")
            .header(CONTENT_TYPE, "application/msgpack")
            .header(CONTENT_ENCODING, "zstd")
            .body(Body::from(body))
            .unwrap();
        assert_eq!(
            app.oneshot(request).await.unwrap().status(),
            StatusCode::ACCEPTED
        );
        assert_eq!(buffer.take(true).unwrap().len, 1);
        assert_eq!(resources.slots.available_permits(), 1);
    }

    #[tokio::test]
    async fn rejects_model_amplification_atomically_and_releases_admission() {
        let resources = resources_with_model(32 << 10);
        let buffer = Arc::new(Buffer::new(100, 100));
        let app = router(buffer.clone(), "relay-secret", resources.clone());
        let mut batch =
            agent_wire::decode(include_bytes!("../tests/fixtures/agent-batch.bin"), 1024).unwrap();
        batch.records = vec![
            agent_wire::Record {
                received_at: 1,
                payload: agent_wire::Payload::Text("first".into()),
            },
            agent_wire::Record {
                received_at: 2,
                payload: agent_wire::Payload::Json(format!(
                    "{{\"a\":[{}null]}}",
                    "null,".repeat(1000)
                )),
            },
        ];
        let packed = rmp_serde::to_vec_named(&batch).unwrap();
        let body = zstd::encode_all(&packed[..], 3).unwrap();
        let request = Request::builder()
            .method("POST")
            .uri("/v1/batches")
            .header(AUTHORIZATION, "Bearer relay-secret")
            .header(CONTENT_TYPE, "application/msgpack")
            .header(CONTENT_ENCODING, "zstd")
            .body(Body::from(body))
            .unwrap();
        assert_eq!(
            app.oneshot(request).await.unwrap().status(),
            StatusCode::PAYLOAD_TOO_LARGE
        );
        assert!(buffer.take(true).is_none());
        assert_eq!(resources.slots.available_permits(), 1);
    }

    #[tokio::test]
    async fn rejects_saturated_admission_without_reading_body() {
        let resources = resources();
        let held = resources.slots.clone().try_acquire_owned().unwrap();
        let app = router(
            Arc::new(Buffer::new(1, 1)),
            "relay-secret",
            resources.clone(),
        );
        let request = Request::builder()
            .method("POST")
            .uri("/v1/batches")
            .header(AUTHORIZATION, "Bearer relay-secret")
            .header(CONTENT_TYPE, "application/msgpack")
            .header(CONTENT_ENCODING, "zstd")
            .body(Body::new(UnreadableBody))
            .unwrap();
        assert_eq!(
            app.oneshot(request).await.unwrap().status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        drop(held);
        assert_eq!(resources.slots.available_permits(), 1);
    }

    struct UnreadableBody;

    #[tokio::test]
    async fn full_buffer_and_unready_relay_reject_without_reading_body() {
        for full in [true, false] {
            let resources = resources();
            let buffer = Arc::new(Buffer::new(1, 1));
            if full {
                let batch =
                    agent_wire::decode(include_bytes!("../tests/fixtures/agent-batch.bin"), 1024)
                        .unwrap();
                buffer
                    .push(
                        Source {
                            environment: batch.environment,
                            server: batch.server,
                            backend: batch.backend,
                            service: batch.service,
                            service_instance: batch.service_instance,
                        },
                        0,
                        vec![normalize::event(batch.records[0].clone())],
                    )
                    .unwrap();
            } else {
                resources.set_ready(false);
            }
            let app = router(buffer, "relay-secret", resources.clone());
            let request = Request::builder()
                .method("POST")
                .uri("/v1/batches")
                .header(AUTHORIZATION, "Bearer relay-secret")
                .header(CONTENT_TYPE, "application/msgpack")
                .header(CONTENT_ENCODING, "zstd")
                .body(Body::new(UnreadableBody))
                .unwrap();
            assert_eq!(
                app.oneshot(request).await.unwrap().status(),
                StatusCode::SERVICE_UNAVAILABLE
            );
            assert_eq!(resources.slots.available_permits(), 1);
        }
    }

    #[tokio::test]
    async fn compressed_body_limit_is_explicit_and_releases_admission() {
        let resources = resources();
        let app = router(
            Arc::new(Buffer::new(1, 1)),
            "relay-secret",
            resources.clone(),
        );
        let request = Request::builder()
            .method("POST")
            .uri("/v1/batches")
            .header(AUTHORIZATION, "Bearer relay-secret")
            .header(CONTENT_TYPE, "application/msgpack")
            .header(CONTENT_ENCODING, "zstd")
            .body(Body::from(vec![0; MAX_BODY + 1]))
            .unwrap();
        assert_eq!(
            app.oneshot(request).await.unwrap().status(),
            StatusCode::PAYLOAD_TOO_LARGE
        );
        assert_eq!(resources.slots.available_permits(), 1);
    }

    impl http_body::Body for UnreadableBody {
        type Data = Bytes;
        type Error = Infallible;

        fn poll_frame(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
            panic!("unauthorized request body must not be read");
        }
    }

    #[tokio::test]
    async fn rejects_invalid_authorization_before_reading_body() {
        let buffer = Arc::new(Buffer::new(1, 1));
        let app = router(buffer.clone(), "relay-secret", resources());
        let invalid_headers: &[&[&str]] = &[
            &[],
            &["Bearer wrong"],
            &["Basic relay-secret"],
            &["Bearer"],
            &["Bearer "],
            &["Bearer  relay-secret"],
            &["Bearer relay-secret "],
            &["Bearer relay-secret, Bearer relay-secret"],
            &["Bearer relay-secret", "Bearer relay-secret"],
            &["Bearer wrong", "Bearer relay-secret"],
            &["Bearer relay-secret", "Bearer wrong"],
        ];
        for values in invalid_headers {
            let mut request = Request::builder()
                .method("POST")
                .uri("/v1/batches")
                .header(CONTENT_TYPE, "application/msgpack")
                .header(CONTENT_ENCODING, "zstd")
                .header("content-length", MAX_BODY + 1)
                .body(Body::new(UnreadableBody))
                .unwrap();
            for value in *values {
                request
                    .headers_mut()
                    .append(AUTHORIZATION, HeaderValue::from_str(value).unwrap());
            }
            let response = app.clone().oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{values:?}");
            assert_eq!(response.headers()["www-authenticate"], "Bearer");
            assert!(buffer.take(true).is_none());
        }
    }

    #[tokio::test]
    async fn accepts_case_insensitive_bearer_scheme_and_preserves_body_validation() {
        let app = router(Arc::new(Buffer::new(1, 1)), "relay-secret", resources());
        for (content_type, status) in [
            ("text/plain", StatusCode::UNSUPPORTED_MEDIA_TYPE),
            ("application/msgpack", StatusCode::BAD_REQUEST),
        ] {
            let request = Request::builder()
                .method("POST")
                .uri("/v1/batches")
                .header(AUTHORIZATION, "bEaReR relay-secret")
                .header(CONTENT_TYPE, content_type)
                .header(CONTENT_ENCODING, "zstd")
                .body(Body::from("not zstd"))
                .unwrap();
            assert_eq!(app.clone().oneshot(request).await.unwrap().status(), status);
        }
    }
}
