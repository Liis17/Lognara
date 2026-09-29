//! Разбор записей агента в события для core.

use std::collections::HashMap;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Map, Value};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use crate::agent_wire::{Payload, Record};
use crate::core_wire::{Event, LogId, LogLevel, SpanId, TraceId};

/// Превращает запись агента в событие. Всё, что не легло в поля `Event`,
/// остаётся в `attributes`, поэтому данные записи не теряются.
pub fn event(record: Record) -> Event {
    let mut event = Event {
        id: LogId(Uuid::now_v7()),
        timestamp: record.received_at,
        ingested_at: record.received_at,
        action: None,
        level: LogLevel::Unknown,
        message: String::new(),
        trace_id: None,
        span_id: None,
        parent_span_id: None,
        request_id: None,
        attributes: HashMap::new(),
    };
    match record.payload {
        Payload::Text(text) => event.message = text,
        Payload::Binary(bytes) => {
            event.message = format!("binary payload, {} bytes", bytes.len());
            event
                .attributes
                .insert("payload_base64".into(), STANDARD.encode(bytes).into());
        }
        Payload::Json(json) => match serde_json::from_str(&json) {
            Ok(Value::Object(object)) => fill(&mut event, object),
            Ok(Value::String(text)) => event.message = text,
            // Числа, массивы и прочие значения, а также невалидный JSON остаются текстом.
            _ => event.message = json,
        },
    }
    event
}

/// Раскладывает канонические ключи JSON-объекта по полям события. Неподходящие
/// по типу или формату значения и остальные ключи уходят в `attributes`.
fn fill(event: &mut Event, object: Map<String, Value>) {
    let mut nested = None;
    for (key, value) in object {
        let unparsed = match key.as_str() {
            "id" => parsed(value, |text| Uuid::parse_str(text).ok()).map(|id| event.id = LogId(id)),
            "timestamp" => parsed(value, unix_nanos).map(|nanos| event.timestamp = nanos),
            "level" => parsed(value, level).map(|level| event.level = level),
            "message" => text(value).map(|message| event.message = message),
            "action" => text(value).map(|action| event.action = Some(action)),
            "request_id" => text(value).map(|id| event.request_id = Some(id)),
            "trace_id" => parsed(value, hex).map(|id| event.trace_id = Some(TraceId(id))),
            "span_id" => parsed(value, hex).map(|id| event.span_id = Some(SpanId(id))),
            "parent_span_id" => {
                parsed(value, hex).map(|id| event.parent_span_id = Some(SpanId(id)))
            }
            "attributes" => match value {
                Value::Object(object) => {
                    nested = Some(object);
                    Ok(())
                }
                other => Err(other),
            },
            _ => Err(value),
        };
        if let Err(value) = unparsed {
            event.attributes.insert(key, value);
        }
    }
    // Ключи вложенного `attributes` перекрывают одноимённые ключи верхнего уровня.
    event.attributes.extend(nested.unwrap_or_default());
}

fn text(value: Value) -> Result<String, Value> {
    match value {
        Value::String(text) => Ok(text),
        other => Err(other),
    }
}

/// Разбирает строковое значение; если не вышло, возвращает значение как было.
fn parsed<T>(value: Value, parse: impl FnOnce(&str) -> Option<T>) -> Result<T, Value> {
    let text = text(value)?;
    parse(&text).ok_or(Value::String(text))
}

/// RFC 3339 в Unix-время в наносекундах.
fn unix_nanos(text: &str) -> Option<i64> {
    let time = OffsetDateTime::parse(text, &Rfc3339).ok()?;
    i64::try_from(time.unix_timestamp_nanos()).ok()
}

fn level(text: &str) -> Option<LogLevel> {
    Some(match text.to_ascii_lowercase().as_str() {
        "trace" => LogLevel::Trace,
        "debug" => LogLevel::Debug,
        "info" => LogLevel::Info,
        "warn" => LogLevel::Warn,
        "error" => LogLevel::Error,
        "fatal" => LogLevel::Fatal,
        _ => return None,
    })
}

/// Ровно `2 * N` hex-символов в `N` байт.
fn hex<const N: usize>(text: &str) -> Option<[u8; N]> {
    if text.len() != 2 * N {
        return None;
    }
    let digit = |byte: u8| char::from(byte).to_digit(16).map(|digit| digit as u8);
    let mut bytes = [0; N];
    for (byte, [high, low]) in bytes.iter_mut().zip(text.as_bytes().as_chunks().0) {
        *byte = digit(*high)? << 4 | digit(*low)?;
    }
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const RECEIVED_AT: i64 = 1_790_678_511_894_000_000;

    fn record(payload: Payload) -> Record {
        Record {
            received_at: RECEIVED_AT,
            payload,
        }
    }

    fn from_json(value: Value) -> Event {
        event(record(Payload::Json(value.to_string())))
    }

    fn attributes(value: Value) -> HashMap<String, Value> {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn parses_canonical_json_log() {
        let event = from_json(json!({
            "id": "019d3f8e-7c2a-7b3e-9f1d-2a4b6c8d0e1f",
            "timestamp": "2026-09-29T10:41:51.871Z",
            "action": "file.upload",
            "level": "error",
            "message": "Failed to save file metadata",
            "trace_id": "8c21f0a4b6c8d0e12a4b6c8d0e1f3a5b",
            "span_id": "91af2a4b6c8d0e1f",
            "parent_span_id": "72DE2A4B6C8D0E1F",
            "request_id": "req_019d",
            "attributes": {
                "user_id": 18271,
                "filename": "cat.png",
                "exception.type": "S3TimeoutException"
            }
        }));

        assert_eq!(
            event,
            Event {
                id: LogId("019d3f8e-7c2a-7b3e-9f1d-2a4b6c8d0e1f".parse().unwrap()),
                timestamp: 1_790_678_511_871_000_000,
                ingested_at: RECEIVED_AT,
                action: Some("file.upload".into()),
                level: LogLevel::Error,
                message: "Failed to save file metadata".into(),
                trace_id: Some(TraceId([
                    0x8c, 0x21, 0xf0, 0xa4, 0xb6, 0xc8, 0xd0, 0xe1, 0x2a, 0x4b, 0x6c, 0x8d, 0x0e,
                    0x1f, 0x3a, 0x5b
                ])),
                span_id: Some(SpanId([0x91, 0xaf, 0x2a, 0x4b, 0x6c, 0x8d, 0x0e, 0x1f])),
                parent_span_id: Some(SpanId([0x72, 0xde, 0x2a, 0x4b, 0x6c, 0x8d, 0x0e, 0x1f])),
                request_id: Some("req_019d".into()),
                attributes: attributes(json!({
                    "user_id": 18271,
                    "filename": "cat.png",
                    "exception.type": "S3TimeoutException"
                })),
            }
        );
    }

    #[test]
    fn keeps_unknown_and_source_keys_in_attributes() {
        let event = from_json(json!({
            "message": "hi",
            "service": "api",
            "ingested_at": "2026-09-29T10:41:51.894Z",
            "retry": 2,
            "attributes": {"retry": 3}
        }));

        assert_eq!(event.message, "hi");
        assert_eq!(
            event.attributes,
            attributes(json!({
                "service": "api",
                "ingested_at": "2026-09-29T10:41:51.894Z",
                "retry": 3
            }))
        );
    }

    #[test]
    fn keeps_invalid_canonical_values_in_attributes() {
        let invalid = json!({
            "id": "not-a-uuid",
            "timestamp": 1_790_678_511,
            "level": "verbose",
            "message": {"text": "nested"},
            "action": 42,
            "trace_id": "xyz",
            "span_id": "91af",
            "parent_span_id": "+1af2a4b6c8d0e1f",
            "request_id": null,
            "attributes": "flat"
        });

        let event = from_json(invalid.clone());

        assert_eq!(event.timestamp, RECEIVED_AT);
        assert_eq!(event.level, LogLevel::Unknown);
        assert_eq!(event.message, "");
        assert_eq!(event.action, None);
        assert_eq!(event.trace_id, None);
        assert_eq!(event.span_id, None);
        assert_eq!(event.parent_span_id, None);
        assert_eq!(event.request_id, None);
        assert_eq!(event.attributes, attributes(invalid));
    }

    #[test]
    fn level_is_case_insensitive() {
        assert_eq!(from_json(json!({"level": "WARN"})).level, LogLevel::Warn);
    }

    #[test]
    fn text_becomes_message() {
        let event = event(record(Payload::Text("plain line".into())));

        assert_eq!(event.message, "plain line");
        assert_eq!(event.level, LogLevel::Unknown);
        assert_eq!(event.timestamp, RECEIVED_AT);
        assert_eq!(event.ingested_at, RECEIVED_AT);
        assert!(event.attributes.is_empty());
    }

    #[test]
    fn binary_is_kept_as_base64() {
        let event = event(record(Payload::Binary(vec![0, 159, 146, 150])));

        assert_eq!(event.message, "binary payload, 4 bytes");
        assert_eq!(event.level, LogLevel::Unknown);
        assert_eq!(
            event.attributes,
            attributes(json!({"payload_base64": "AJ+Slg=="}))
        );
    }

    #[test]
    fn non_object_json_becomes_message() {
        for (json, message) in [
            (r#""quoted text""#, "quoted text"),
            ("3", "3"),
            ("[1,2]", "[1,2]"),
            ("{bad", "{bad"),
        ] {
            let event = event(record(Payload::Json(json.into())));

            assert_eq!(event.message, message);
            assert!(event.attributes.is_empty());
        }
    }

    #[test]
    fn events_get_distinct_ids() {
        let first = event(record(Payload::Text("a".into())));
        let second = event(record(Payload::Text("a".into())));

        assert_ne!(first.id, second.id);
    }
}
