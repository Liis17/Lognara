//! Формат пачки, которую relay отправляет в lognara-core.
//!
//! Тело запроса: `CoreBatch` в MessagePack (поля по именам), сжатый zstd.
//! Заголовки: `Content-Type: application/msgpack`, `Content-Encoding: zstd`,
//! `Authorization: Bearer <токен>`.
//!
//! События сгруппированы по источнику: полный `LogEvent` в core = `Source` группы + `Event`.

use std::collections::HashMap;
use std::io::Write;

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
    let mut encoder = BatchEncoder::new(batch, body_limit, decoded_limit, model_limit);
    let mut encoded = Vec::new();
    for part in encoder.by_ref() {
        encoded.push(part);
    }
    (encoded, encoder.take_dropped())
}

/// Ленивые части: единственная исходная модель, без копирования Source/Event.
pub struct BatchEncoder {
    batch: CoreBatch,
    pending: Vec<Part>,
    body_limit: usize,
    decoded_limit: usize,
    model_limit: usize,
    dropped: u64,
}

#[derive(Clone, Copy)]
struct Part {
    groups_start: usize,
    groups_end: usize,
    events: Option<(usize, usize)>,
    batch_counter: bool,
}

impl BatchEncoder {
    pub fn new(
        batch: CoreBatch,
        body_limit: usize,
        decoded_limit: usize,
        model_limit: usize,
    ) -> Self {
        let end = batch.groups.len();
        Self {
            batch,
            pending: vec![Part {
                groups_start: 0,
                groups_end: end,
                events: None,
                batch_counter: true,
            }],
            body_limit,
            decoded_limit,
            model_limit,
            dropped: 0,
        }
    }

    pub fn finished(&self) -> bool {
        self.pending.is_empty()
    }
    pub fn take_dropped(&mut self) -> u64 {
        std::mem::take(&mut self.dropped)
    }
}

#[derive(Serialize)]
struct BatchView<'a> {
    dropped: u64,
    groups: GroupsView<'a>,
}

struct GroupsView<'a> {
    groups: &'a [Group],
    events: Option<(usize, usize)>,
}

#[derive(Serialize)]
struct GroupView<'a> {
    source: &'a Source,
    dropped: u64,
    events: &'a [Event],
}

impl Serialize for GroupsView<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut sequence = serializer.serialize_seq(Some(self.groups.len()))?;
        for group in self.groups {
            let (start, end) = self.events.unwrap_or((0, group.events.len()));
            sequence.serialize_element(&GroupView {
                source: &group.source,
                dropped: if start == 0 { group.dropped } else { 0 },
                events: &group.events[start..end],
            })?;
        }
        sequence.end()
    }
}

impl Iterator for BatchEncoder {
    type Item = EncodedBatch;

    fn next(&mut self) -> Option<EncodedBatch> {
        while let Some(part) = self.pending.pop() {
            let groups = &self.batch.groups[part.groups_start..part.groups_end];
            let events = part.events.map_or_else(
                || groups.iter().map(|g| g.events.len()).sum(),
                |(start, end)| end - start,
            );
            let counter = if part.batch_counter {
                self.batch.dropped
            } else {
                0
            };
            let view = BatchView {
                dropped: counter,
                groups: GroupsView {
                    groups,
                    events: part.events,
                },
            };
            let mut packed = LimitedWriter::new(self.decoded_limit);
            if view
                .serialize(&mut rmp_serde::Serializer::new(&mut packed).with_struct_map())
                .is_ok()
                && crate::wire_budget::validate(&packed.bytes, self.model_limit).is_ok()
            {
                let mut compressed = LimitedWriter::new(self.body_limit);
                let result = (|| {
                    let mut encoder =
                        zstd::stream::write::Encoder::new(&mut compressed, ZSTD_LEVEL)?;
                    encoder.window_log(25)?;
                    encoder.write_all(&packed.bytes)?;
                    encoder.finish()?;
                    Ok::<_, std::io::Error>(())
                })();
                if result.is_ok() {
                    return Some(EncodedBatch {
                        body: compressed.bytes,
                        events,
                    });
                }
            }
            let split = if groups.len() > 1 {
                let middle = part.groups_start + groups.len() / 2;
                Some((
                    Part {
                        groups_end: middle,
                        ..part
                    },
                    Part {
                        groups_start: middle,
                        batch_counter: false,
                        ..part
                    },
                ))
            } else if events > 1 {
                let (start, end) = part.events.unwrap_or((0, events));
                let middle = start + (end - start) / 2;
                Some((
                    Part {
                        events: Some((start, middle)),
                        ..part
                    },
                    Part {
                        events: Some((middle, end)),
                        batch_counter: false,
                        ..part
                    },
                ))
            } else {
                None
            };
            if let Some((left, right)) = split {
                self.pending.push(right);
                self.pending.push(left);
            } else {
                tracing::error!(
                    events,
                    "single event or source metadata exceeds core byte limits"
                );
                self.dropped = self
                    .dropped
                    .saturating_add(events as u64)
                    .saturating_add(counter);
            }
        }
        None
    }
}

struct LimitedWriter {
    bytes: Vec<u8>,
    limit: usize,
}

impl LimitedWriter {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(limit),
            limit,
        }
    }
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
