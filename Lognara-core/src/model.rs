//! Событие и внутренний адрес. JSON API отделён от бинарного контракта relay.

use serde::Serialize;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::wire::{Event, Source};

#[derive(Debug, Clone, PartialEq)]
pub struct StoredEvent {
    pub sequence: u64,
    pub core_received_at: i64,
    pub relay_id: Option<i64>,
    pub agent_id: Option<i64>,
    pub source: Source,
    pub event: Event,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Node {
    pub relay_id: i64,
    pub agent_id: i64,
}

#[derive(Debug, Serialize)]
pub struct LogEvent {
    pub id: String,
    pub timestamp: String,
    pub ingested_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relay_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<i64>,
    #[serde(flatten)]
    pub source: Source,
    pub action: Option<String>,
    pub level: crate::wire::LogLevel,
    pub message: String,
    pub trace_id: Option<String>,
    pub span_id: Option<String>,
    pub parent_span_id: Option<String>,
    pub request_id: Option<String>,
    pub attributes: std::collections::HashMap<String, serde_json::Value>,
}

impl From<StoredEvent> for LogEvent {
    fn from(row: StoredEvent) -> Self {
        let event = row.event;
        Self {
            id: event.id.0.to_string(),
            timestamp: format_timestamp(event.timestamp),
            ingested_at: format_timestamp(event.ingested_at),
            relay_id: row.relay_id,
            agent_id: row.agent_id,
            source: row.source,
            action: event.action,
            level: event.level,
            message: event.message,
            trace_id: event.trace_id.map(|id| hex::encode(id.0)),
            span_id: event.span_id.map(|id| hex::encode(id.0)),
            parent_span_id: event.parent_span_id.map(|id| hex::encode(id.0)),
            request_id: event.request_id,
            attributes: event.attributes,
        }
    }
}

pub fn now_nanos() -> i64 {
    // i64 наносекунды представляют даты 1677..2262.
    OffsetDateTime::now_utc()
        .unix_timestamp_nanos()
        .try_into()
        .expect("system time outside i64 nanoseconds")
}

pub fn format_timestamp(nanos: i64) -> String {
    OffsetDateTime::from_unix_timestamp_nanos(nanos as i128)
        .expect("i64 nanoseconds are representable")
        .format(&Rfc3339)
        .expect("UTC timestamp is RFC3339")
}

pub fn parse_timestamp(text: &str) -> anyhow::Result<i64> {
    Ok(OffsetDateTime::parse(text, &Rfc3339)?
        .unix_timestamp_nanos()
        .try_into()?)
}
