//! Приём логов от приложения: `POST /v1/logs`, формат по `Content-Type`.

use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde_json::value::RawValue;

use crate::buffer::Buffer;
use crate::wire::{self, Payload, Record};

pub fn router(buffer: Arc<Buffer>) -> Router {
    Router::new()
        .route("/v1/logs", post(ingest))
        .with_state(buffer)
}

async fn ingest(
    State(buffer): State<Arc<Buffer>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, IngestError> {
    let received_at = wire::unix_nanos();
    let content_type = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok());

    let payloads = parse_body(content_type, &body)?;
    buffer.push(payloads.into_iter().map(|payload| Record {
        received_at,
        payload,
    }));
    Ok(StatusCode::ACCEPTED)
}

#[derive(Debug, PartialEq)]
pub enum IngestError {
    UnsupportedType,
    EmptyBody,
    InvalidUtf8,
    InvalidJson,
}

impl IntoResponse for IngestError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::UnsupportedType => (
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "expected Content-Type text/plain, application/json or application/octet-stream",
            ),
            Self::EmptyBody => (StatusCode::BAD_REQUEST, "empty body"),
            Self::InvalidUtf8 => (StatusCode::BAD_REQUEST, "text body is not valid UTF-8"),
            Self::InvalidJson => (StatusCode::BAD_REQUEST, "body is not valid JSON"),
        };
        (status, message).into_response()
    }
}

/// Превращает тело запроса в записи: text и binary дают одну запись,
/// JSON-массив даёт по записи на элемент.
pub fn parse_body(content_type: Option<&str>, body: &[u8]) -> Result<Vec<Payload>, IngestError> {
    let mime = content_type
        .and_then(|value| value.split(';').next())
        .map(|essence| essence.trim().to_ascii_lowercase())
        .ok_or(IngestError::UnsupportedType)?;
    if !matches!(
        mime.as_str(),
        "text/plain" | "application/json" | "application/octet-stream"
    ) {
        return Err(IngestError::UnsupportedType);
    }
    if body.is_empty() {
        return Err(IngestError::EmptyBody);
    }

    match mime.as_str() {
        "text/plain" => {
            let text = String::from_utf8(body.to_vec()).map_err(|_| IngestError::InvalidUtf8)?;
            Ok(vec![Payload::Text(text)])
        }
        "application/json" => parse_json(body),
        _ => Ok(vec![Payload::Binary(body.to_vec())]),
    }
}

fn parse_json(body: &[u8]) -> Result<Vec<Payload>, IngestError> {
    let is_array = body.iter().find(|byte| !byte.is_ascii_whitespace()) == Some(&b'[');
    let values: Vec<&RawValue> = if is_array {
        serde_json::from_slice(body).map_err(|_| IngestError::InvalidJson)?
    } else {
        vec![serde_json::from_slice(body).map_err(|_| IngestError::InvalidJson)?]
    };
    Ok(values
        .into_iter()
        .map(|value| Payload::Json(value.get().to_owned()))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(value: &str) -> Payload {
        Payload::Json(value.to_owned())
    }

    #[test]
    fn text_is_one_record() {
        let body = "Failed to save file\n  at upload.rs:42";

        assert_eq!(
            parse_body(Some("text/plain; charset=utf-8"), body.as_bytes()),
            Ok(vec![Payload::Text(body.to_owned())])
        );
        assert_eq!(
            parse_body(Some("Text/Plain"), b"hi"),
            Ok(vec![Payload::Text("hi".to_owned())])
        );
    }

    #[test]
    fn text_must_be_utf8() {
        assert_eq!(
            parse_body(Some("text/plain"), &[0xff, 0xfe]),
            Err(IngestError::InvalidUtf8)
        );
    }

    #[test]
    fn json_object_is_one_record() {
        assert_eq!(
            parse_body(
                Some("application/json"),
                br#" {"level":"error","retry":2} "#
            ),
            Ok(vec![json(r#"{"level":"error","retry":2}"#)])
        );
    }

    #[test]
    fn json_array_is_record_per_element() {
        assert_eq!(
            parse_body(Some("application/json"), br#"[{"a":1}, "text", 3]"#),
            Ok(vec![json(r#"{"a":1}"#), json(r#""text""#), json("3")])
        );
    }

    #[test]
    fn rejects_invalid_json() {
        for body in [&b"{bad"[..], b"[1, 2", b"{} {}"] {
            assert_eq!(
                parse_body(Some("application/json"), body),
                Err(IngestError::InvalidJson)
            );
        }
    }

    #[test]
    fn binary_is_passed_as_is() {
        assert_eq!(
            parse_body(Some("application/octet-stream"), &[0, 159, 146, 150]),
            Ok(vec![Payload::Binary(vec![0, 159, 146, 150])])
        );
    }

    #[test]
    fn rejects_unsupported_or_missing_type() {
        assert_eq!(
            parse_body(Some("application/xml"), b"<log/>"),
            Err(IngestError::UnsupportedType)
        );
        assert_eq!(parse_body(None, b"hi"), Err(IngestError::UnsupportedType));
    }

    #[test]
    fn rejects_empty_body() {
        assert_eq!(
            parse_body(Some("text/plain"), b""),
            Err(IngestError::EmptyBody)
        );
    }
}
