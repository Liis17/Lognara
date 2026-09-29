//! Приём пачек от агентов: `POST /v1/batches`, MessagePack + zstd.

use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::header::{CONTENT_ENCODING, CONTENT_TYPE, HeaderName};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;

use crate::agent_wire::{self, DecodeError};
use crate::buffer::Buffer;
use crate::core_wire::Source;
use crate::normalize;

/// Предел сжатого тела запроса.
const MAX_BODY: usize = 64 * 1024 * 1024;
/// Предел распакованной пачки, защита от zstd-бомбы.
const MAX_DECODED: u64 = 256 * 1024 * 1024;

pub fn router(buffer: Arc<Buffer>) -> Router {
    Router::new()
        .route("/v1/batches", post(ingest))
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .with_state(buffer)
}

async fn ingest(
    State(buffer): State<Arc<Buffer>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, IngestError> {
    if !header_is(&headers, CONTENT_TYPE, "application/msgpack")
        || !header_is(&headers, CONTENT_ENCODING, "zstd")
    {
        return Err(IngestError::UnsupportedType);
    }

    let batch = agent_wire::decode(&body, MAX_DECODED)?;
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
        };
        (status, message).into_response()
    }
}
