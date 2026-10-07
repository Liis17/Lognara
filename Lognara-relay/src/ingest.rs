//! Приём пачек от агентов: `POST /v1/batches`, MessagePack + zstd.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, FromRequest, Request, State};
use axum::http::header::{AUTHORIZATION, CONTENT_ENCODING, CONTENT_TYPE, HeaderName};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
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
    mut request: Request,
) -> Result<StatusCode, IngestError> {
    if !header_is(request.headers(), CONTENT_TYPE, "application/msgpack")
        || !header_is(request.headers(), CONTENT_ENCODING, "zstd")
    {
        return Err(IngestError::UnsupportedType);
    }

    if !state.resources.ready() {
        return Err(IngestError::Unavailable);
    }
    let permit = state
        .resources
        .slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| IngestError::Unavailable)?;
    DefaultBodyLimit::max(MAX_BODY).apply(&mut request);
    let body = tokio::time::timeout(Duration::from_secs(30), Bytes::from_request(request, &()))
        .await
        .map_err(|_| IngestError::Timeout)?
        .map_err(|error| {
            if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
                IngestError::TooLarge
            } else {
                IngestError::InvalidBody
            }
        })?;
    crate::memory::run_blocking(permit, move || accept(&state.buffer, &body))
        .await
        .map_err(|error| {
            tracing::error!(%error, "ingest worker failed");
            IngestError::Unavailable
        })?
}

fn accept(buffer: &Buffer, body: &[u8]) -> Result<StatusCode, IngestError> {
    let batch = agent_wire::decode(body, MAX_DECODED as u64)?;
    let source = Source {
        environment: batch.environment,
        server: batch.server,
        backend: batch.backend,
        service: batch.service,
        service_instance: batch.service_instance,
    };
    let events = batch.records.into_iter().map(normalize::event).collect();
    buffer
        .push(source, batch.dropped, events)
        .map_err(|_| IngestError::BufferFull)?;
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
    use std::pin::Pin;
    use std::task::{Context, Poll};

    use axum::body::Body;
    use axum::http::HeaderValue;
    use http_body::Frame;
    use tower::ServiceExt;

    use super::*;

    fn resources() -> Arc<Resources> {
        let config = crate::config::Config::from_lookup(|name| match name {
            "LOGNARA_CORE_URL" => Some("http://localhost/v1/batches".into()),
            "LOGNARA_CORE_TOKEN" | "LOGNARA_RELAY_TOKEN" => Some("secret".into()),
            _ => None,
        })
        .unwrap();
        Resources::new(&config)
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
