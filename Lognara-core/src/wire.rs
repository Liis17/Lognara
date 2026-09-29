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

/// Распаковывает одну пачку с ограничением памяти и глубины MessagePack.
pub fn decode(body: &[u8], limit: usize) -> Result<CoreBatch, DecodeError> {
    use std::io::Read;
    let decoder = zstd::stream::read::Decoder::new(body).map_err(|_| DecodeError::Invalid)?;
    let mut packed = Vec::new();
    decoder
        .take(limit as u64 + 1)
        .read_to_end(&mut packed)
        .map_err(|_| DecodeError::Invalid)?;
    if packed.len() > limit {
        return Err(DecodeError::TooLarge);
    }
    let mut decoder = rmp_serde::Deserializer::new(std::io::Cursor::new(&packed));
    decoder.set_max_depth(64);
    let batch = CoreBatch::deserialize(&mut decoder).map_err(|_| DecodeError::Invalid)?;
    if decoder.position() != packed.len() as u64 {
        return Err(DecodeError::Invalid);
    }
    Ok(batch)
}

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("decoded batch exceeds the byte limit")]
    TooLarge,
    #[error("expected one zstd-compressed MessagePack CoreBatch")]
    Invalid,
}

impl CoreBatch {
    pub fn event_count(&self) -> usize {
        self.groups.iter().map(|group| group.events.len()).sum()
    }

    pub fn into_events(
        self,
        first_sequence: u64,
        received_at: i64,
    ) -> impl Iterator<Item = crate::model::StoredEvent> {
        self.groups
            .into_iter()
            .flat_map(|group| {
                group
                    .events
                    .into_iter()
                    .map(move |event| (group.source.clone(), event))
            })
            .enumerate()
            .map(move |(offset, (source, event))| crate::model::StoredEvent {
                sequence: first_sequence + offset as u64,
                core_received_at: received_at,
                source,
                event,
            })
    }
}

impl LogLevel {
    pub fn code(self) -> u8 {
        match self {
            Self::Trace => 0,
            Self::Debug => 1,
            Self::Info => 2,
            Self::Warn => 3,
            Self::Error => 4,
            Self::Fatal => 5,
            Self::Unknown => 255,
        }
    }
    pub fn from_code(code: u8) -> anyhow::Result<Self> {
        Ok(match code {
            0 => Self::Trace,
            1 => Self::Debug,
            2 => Self::Info,
            3 => Self::Warn,
            4 => Self::Error,
            5 => Self::Fatal,
            255 => Self::Unknown,
            _ => anyhow::bail!("invalid level code {code}"),
        })
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Trace => "trace",
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
            Self::Fatal => "fatal",
            Self::Unknown => "unknown",
        }
    }
}
