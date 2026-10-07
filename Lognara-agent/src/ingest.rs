//! Один входящий запрос: bounded чтение, разбор и атомарный durable commit.

use std::cell::Cell;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::http::header::CONTENT_TYPE;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use http_body::Body;
use lognara_spool::Error;
use serde::de::{DeserializeSeed, SeqAccess, Visitor};
use serde_json::value::RawValue;
use tracing::error;

use crate::buffer::Buffer;
use crate::wire::{self, Payload, Record};

const MAX_BODY: usize = 2 << 20;

pub fn router(buffer: Arc<Buffer>) -> Router {
    Router::new()
        .route("/v1/logs", post(ingest))
        .with_state(buffer)
}

async fn ingest(
    State(buffer): State<Arc<Buffer>>,
    request: Request,
) -> Result<StatusCode, IngestError> {
    if !buffer.admitted() {
        return Err(IngestError::Busy);
    }
    let slot = buffer
        .input
        .clone()
        .try_acquire_owned()
        .map_err(|_| IngestError::Busy)?;
    let models = buffer
        .models
        .clone()
        .try_acquire_many_owned(buffer.budget())
        .map_err(|_| IngestError::Busy)?;
    let received_at = wire::unix_nanos();
    let content_type = request
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let mut body = request.into_body();
    let read = async {
        let mut bytes = Vec::with_capacity(MAX_BODY);
        while let Some(frame) =
            std::future::poll_fn(|cx| std::pin::Pin::new(&mut body).poll_frame(cx)).await
        {
            let frame = frame.map_err(|_| IngestError::EmptyBody)?;
            if let Ok(data) = frame.into_data() {
                if data.len() > MAX_BODY - bytes.len() {
                    return Err(IngestError::TooLarge);
                }
                bytes.extend_from_slice(&data);
            }
        }
        Ok(bytes)
    };
    let bytes = tokio::time::timeout(Duration::from_secs(30), read)
        .await
        .map_err(|_| IngestError::Timeout)??;
    drop(body);
    tokio::task::spawn_blocking(move || {
        let _reservation = (slot, models);
        let bytes = bytes;
        let payloads = parse_body_limited(
            content_type.as_deref(),
            &bytes,
            buffer.record_limit(),
            buffer.max_record_bytes(),
        )?;
        let records = payloads
            .into_iter()
            .map(|payload| Record {
                received_at,
                payload,
            })
            .collect();
        drop(bytes);
        buffer.push(records).map_err(|e| match e {
            Error::Full => IngestError::Busy,
            Error::TooLarge => IngestError::TooLarge,
            Error::Io(e) => {
                buffer.set_ready(false);
                error!(error = %e, "agent spool write failed; accepted data retained");
                IngestError::Busy
            }
        })?;
        Ok(StatusCode::ACCEPTED)
    })
    .await
    .map_err(|_| IngestError::Busy)?
}

#[derive(Debug, PartialEq)]
pub enum IngestError {
    UnsupportedType,
    EmptyBody,
    InvalidUtf8,
    InvalidJson,
    TooLarge,
    Busy,
    Timeout,
}
impl IntoResponse for IngestError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::UnsupportedType => (
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "expected text/plain, application/json or application/octet-stream",
            ),
            Self::EmptyBody => (StatusCode::BAD_REQUEST, "empty or unreadable body"),
            Self::InvalidUtf8 => (StatusCode::BAD_REQUEST, "text body is not valid UTF-8"),
            Self::InvalidJson => (StatusCode::BAD_REQUEST, "body is not valid JSON"),
            Self::TooLarge => (
                StatusCode::PAYLOAD_TOO_LARGE,
                "record or request exceeds resource limits",
            ),
            Self::Busy => (
                StatusCode::SERVICE_UNAVAILABLE,
                "agent queue or resources unavailable; retry request",
            ),
            Self::Timeout => (StatusCode::REQUEST_TIMEOUT, "body read timed out"),
        };
        (status, message).into_response()
    }
}

#[cfg(test)]
fn parse_body(content_type: Option<&str>, body: &[u8]) -> Result<Vec<Payload>, IngestError> {
    parse_body_limited(content_type, body, 12000, MAX_BODY)
}
fn parse_body_limited(
    content_type: Option<&str>,
    body: &[u8],
    records: usize,
    max_record: usize,
) -> Result<Vec<Payload>, IngestError> {
    let mime = content_type
        .and_then(|v| v.split(';').next())
        .map(|v| v.trim().to_ascii_lowercase())
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
            if body.len() > max_record {
                return Err(IngestError::TooLarge);
            }
            let text = std::str::from_utf8(body).map_err(|_| IngestError::InvalidUtf8)?;
            Ok(vec![Payload::Text(text.to_owned())])
        }
        "application/json" => {
            let is_array = body.iter().find(|b| !b.is_ascii_whitespace()) == Some(&b'[');
            let oversized = Cell::new(false);
            let mut decoder = serde_json::Deserializer::from_slice(body);
            let values = if is_array {
                JsonRecords {
                    limit: records,
                    max_record,
                    oversized: &oversized,
                }
                .deserialize(&mut decoder)
            } else {
                serde::Deserialize::deserialize(&mut decoder).map(|value: &RawValue| vec![value])
            }
            .map_err(|_| {
                if oversized.get() {
                    IngestError::TooLarge
                } else {
                    IngestError::InvalidJson
                }
            })?;
            decoder.end().map_err(|_| IngestError::InvalidJson)?;
            if values.iter().any(|v| v.get().len() > max_record) {
                return Err(IngestError::TooLarge);
            }
            Ok(values
                .into_iter()
                .map(|v| Payload::Json(v.get().to_owned()))
                .collect())
        }
        _ => {
            if body.len() > max_record {
                return Err(IngestError::TooLarge);
            }
            Ok(vec![Payload::Binary(body.to_vec())])
        }
    }
}
struct JsonRecords<'a> {
    limit: usize,
    max_record: usize,
    oversized: &'a Cell<bool>,
}
impl<'de> DeserializeSeed<'de> for JsonRecords<'_> {
    type Value = Vec<&'de RawValue>;
    fn deserialize<D: serde::Deserializer<'de>>(self, decoder: D) -> Result<Self::Value, D::Error> {
        decoder.deserialize_seq(self)
    }
}
impl<'de> Visitor<'de> for JsonRecords<'_> {
    type Value = Vec<&'de RawValue>;
    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("bounded JSON array")
    }
    fn visit_seq<S: SeqAccess<'de>>(self, mut seq: S) -> Result<Self::Value, S::Error> {
        let mut values = Vec::new();
        while let Some(value) = seq.next_element::<&RawValue>()? {
            if values.len() == self.limit || value.get().len() > self.max_record {
                self.oversized.set(true);
                return Err(serde::de::Error::custom("record budget exceeded"));
            }
            values.push(value);
        }
        Ok(values)
    }
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

#[cfg(test)]
mod resource_tests {
    use super::*;
    #[test]
    fn rejects_record_and_array_capacity_before_materializing_payloads() {
        assert_eq!(
            parse_body_limited(Some("application/json"), b"[1,2,3]", 2, 10),
            Err(IngestError::TooLarge)
        );
        assert_eq!(
            parse_body_limited(Some("application/json"), br#"[1,"oversized"]"#, 3, 4),
            Err(IngestError::TooLarge)
        );
        assert_eq!(
            parse_body_limited(Some("text/plain"), b"1234", 1, 4),
            Ok(vec![Payload::Text("1234".into())])
        );
        assert_eq!(
            parse_body_limited(Some("text/plain"), b"12345", 1, 4),
            Err(IngestError::TooLarge)
        );
    }
    struct Unreadable;
    impl http_body::Body for Unreadable {
        type Data = axum::body::Bytes;
        type Error = std::io::Error;
        fn poll_frame(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
            panic!("busy admission must not read body");
        }
    }
    #[tokio::test]
    async fn busy_rejection_precedes_body_and_record_boundaries_release_reservations() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = crate::config::Config::from_lookup(|key| match key {
            "LOGNARA_SERVICE" | "LOGNARA_SERVER" | "LOGNARA_BACKEND" | "LOGNARA_RELAY_TOKEN" => {
                Some("test".into())
            }
            _ => None,
        })
        .unwrap();
        config.spool_dir = dir.path().into();
        let buffer = Buffer::open(config).await.unwrap();
        let slot = buffer.input.clone().acquire_owned().await.unwrap();
        let request = Request::new(axum::body::Body::new(Unreadable));
        assert_eq!(
            ingest(State(buffer.clone()), request).await,
            Err(IngestError::Busy)
        );
        drop(slot);
        for (length, expected) in [
            (MAX_BODY, Ok(StatusCode::ACCEPTED)),
            (MAX_BODY + 1, Err(IngestError::TooLarge)),
        ] {
            let request = Request::builder()
                .header(CONTENT_TYPE, "application/octet-stream")
                .body(axum::body::Body::from(vec![1u8; length]))
                .unwrap();
            assert_eq!(ingest(State(buffer.clone()), request).await, expected);
            assert_eq!(buffer.input.available_permits(), 1);
        }
        assert_eq!(buffer.len(), 1);
    }
}
