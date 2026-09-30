//! Формат пачки, которую relay отправляет в lognara-core.
//!
//! Тело запроса: `CoreBatch` в MessagePack (поля по именам), сжатый zstd.
//! Заголовки: `Content-Type: application/msgpack`, `Content-Encoding: zstd`,
//! `Authorization: Bearer <токен>`.
//!
//! События сгруппированы по источнику: полный `LogEvent` в core = `Source` группы + `Event`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

const ZSTD_LEVEL: i32 = 3;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoreBatch {
    /// Сколько событий relay потерял при переполнении spool с прошлой пачки.
    pub dropped: u64,
    pub groups: Vec<Group>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Group {
    pub source: Source,
    /// Сколько записей этого источника агенты вытеснили из своих буферов.
    pub dropped: u64,
    pub events: Vec<Event>,
}

/// Откуда пришли события: место в иерархии инфраструктуры.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Source {
    pub environment: Option<String>,
    pub server: String,
    pub backend: String,
    pub service: String,
    pub service_instance: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub id: LogId,
    /// Момент события, Unix-время в наносекундах.
    pub timestamp: i64,
    /// Момент приёма агентом, Unix-время в наносекундах.
    pub ingested_at: i64,
    pub action: Option<String>,
    pub level: LogLevel,
    pub message: String,
    pub trace_id: Option<TraceId>,
    pub span_id: Option<SpanId>,
    pub parent_span_id: Option<SpanId>,
    pub request_id: Option<String>,
    pub attributes: HashMap<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
    Fatal,
    /// Уровень не указан или не распознан.
    Unknown,
}

/// UUIDv7 события.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LogId(pub Uuid);

/// Идентификатор трейса W3C Trace Context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TraceId(#[serde(with = "serde_bytes")] pub [u8; 16]);

/// Идентификатор спана W3C Trace Context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SpanId(#[serde(with = "serde_bytes")] pub [u8; 8]);

/// Кодирует пачку в MessagePack и сжимает zstd.
pub fn encode(batch: &CoreBatch) -> Vec<u8> {
    // Запись в Vec не может завершиться ошибкой, а сериализация этих типов всегда успешна.
    let packed = rmp_serde::to_vec_named(batch).expect("batch is always serializable");
    zstd::encode_all(packed.as_slice(), ZSTD_LEVEL).expect("in-memory zstd encoding")
}

/// Готовые тела не перекодируются при повторной отправке: их байты служат ключом core.
pub struct EncodedBatch {
    pub body: Vec<u8>,
    pub events: usize,
}

/// Делит пачку до выполнения обоих лимитов. Непомещающееся одиночное событие
/// учитывается как потеря relay; остальные события сохраняют порядок и идентификаторы.
pub fn encode_split(
    batch: CoreBatch,
    body_limit: usize,
    decoded_limit: usize,
    model_limit: usize,
) -> (Vec<EncodedBatch>, u64) {
    let mut pending = vec![batch];
    let mut encoded = Vec::new();
    let mut dropped = 0u64;
    while let Some(mut batch) = pending.pop() {
        let events = batch.groups.iter().map(|group| group.events.len()).sum();
        let mut packed = LimitedWriter {
            bytes: Vec::new(),
            limit: decoded_limit,
        };
        if batch
            .serialize(&mut rmp_serde::Serializer::new(&mut packed).with_struct_map())
            .is_ok()
            && crate::wire_budget::validate(&packed.bytes, model_limit).is_ok()
        {
            let mut compressed = LimitedWriter {
                bytes: Vec::new(),
                limit: body_limit,
            };
            if zstd::stream::copy_encode(&packed.bytes[..], &mut compressed, ZSTD_LEVEL).is_ok() {
                encoded.push(EncodedBatch {
                    body: compressed.bytes,
                    events,
                });
                continue;
            }
        }
        // Метаданные и счётчики передаются ровно в одной из дочерних пачек.
        let right = if batch.groups.len() > 1 {
            let middle = batch.groups.len() / 2;
            Some(CoreBatch {
                dropped: 0,
                groups: batch.groups.split_off(middle),
            })
        } else if let Some(group) = batch.groups.first_mut()
            && group.events.len() > 1
        {
            let middle = group.events.len() / 2;
            Some(CoreBatch {
                dropped: 0,
                groups: vec![Group {
                    source: group.source.clone(),
                    dropped: 0,
                    events: group.events.split_off(middle),
                }],
            })
        } else {
            None
        };
        if let Some(right) = right {
            pending.push(right);
            pending.push(batch);
        } else {
            tracing::error!(
                events,
                "single event or source metadata exceeds core byte limits"
            );
            dropped = dropped
                .saturating_add(events as u64)
                .saturating_add(batch.dropped);
        }
    }
    (encoded, dropped)
}

struct LimitedWriter {
    bytes: Vec<u8>,
    limit: usize,
}

impl std::io::Write for LimitedWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(std::io::Error::other("encoded batch exceeds byte limit"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn split_respects_both_limits_preserves_ids_order_and_counters() {
        let mut seed = 7u64;
        let events: Vec<_> = (0..12)
            .map(|_| {
                let message = (0..500)
                    .map(|_| {
                        seed ^= seed << 13;
                        seed ^= seed >> 7;
                        seed ^= seed << 17;
                        char::from(b'a' + (seed % 26) as u8)
                    })
                    .collect();
                crate::normalize::event(crate::agent_wire::Record {
                    received_at: 123,
                    payload: crate::agent_wire::Payload::Text(message),
                })
            })
            .collect();
        let source = Source {
            environment: None,
            server: "srv".into(),
            backend: "backend".into(),
            service: "api".into(),
            service_instance: None,
        };
        let batch = CoreBatch {
            dropped: 9,
            groups: vec![
                Group {
                    source: source.clone(),
                    dropped: 3,
                    events: events[..6].to_vec(),
                },
                Group {
                    source,
                    dropped: 5,
                    events: events[6..].to_vec(),
                },
            ],
        };
        for (body_limit, decoded_limit) in [(800, 100_000), (100_000, 1200)] {
            let (parts, dropped) =
                encode_split(batch.clone(), body_limit, decoded_limit, usize::MAX);
            assert_eq!(dropped, 0);
            assert!(parts.len() > 1);
            let mut restored = Vec::new();
            let mut relay_dropped = 0;
            let mut agent_dropped = 0;
            for part in parts {
                assert!(part.body.len() <= body_limit);
                let packed = zstd::decode_all(&part.body[..]).unwrap();
                assert!(packed.len() <= decoded_limit);
                let decoded: CoreBatch = rmp_serde::from_slice(&packed).unwrap();
                assert_eq!(
                    part.events,
                    decoded
                        .groups
                        .iter()
                        .map(|group| group.events.len())
                        .sum::<usize>()
                );
                relay_dropped += decoded.dropped;
                for group in decoded.groups {
                    agent_dropped += group.dropped;
                    restored.extend(group.events);
                }
            }
            assert_eq!(restored, events);
            assert_eq!(relay_dropped, 9);
            assert_eq!(agent_dropped, 8);
        }
        let (parts, dropped) = encode_split(batch, 1, 1, usize::MAX);
        assert!(parts.is_empty());
        assert_eq!(dropped, 21);
    }

    #[test]
    fn encoded_batch_decodes_back() {
        let batch = CoreBatch {
            dropped: 2,
            groups: vec![Group {
                source: Source {
                    environment: Some("production".into()),
                    server: "eu-prod-01".into(),
                    backend: "barkcloud".into(),
                    service: "api".into(),
                    service_instance: None,
                },
                dropped: 7,
                events: vec![Event {
                    id: LogId(Uuid::now_v7()),
                    timestamp: 1,
                    ingested_at: 2,
                    action: Some("file.upload".into()),
                    level: LogLevel::Error,
                    message: "Failed to save file metadata".into(),
                    trace_id: Some(TraceId([0x8c; 16])),
                    span_id: Some(SpanId([0x91; 8])),
                    parent_span_id: None,
                    request_id: Some("req_1".into()),
                    attributes: HashMap::from([
                        ("user_id".into(), json!(18271)),
                        ("tags".into(), json!(["a", {"b": null}])),
                    ]),
                }],
            }],
        };

        let body = encode(&batch);
        let packed = zstd::decode_all(body.as_slice()).unwrap();
        let decoded: CoreBatch = rmp_serde::from_slice(&packed).unwrap();

        assert_eq!(decoded, batch);
    }
}
