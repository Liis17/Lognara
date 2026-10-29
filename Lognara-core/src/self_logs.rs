//! Internal diagnostics use the same WAL and query path as application logs.
use crate::{
    storage::Core,
    wire::{CoreBatch, Event, Group, LogId, LogLevel, Source},
};
use axum::{Json, body::Bytes, extract::State, http::StatusCode};
use lognara_selflog::{Destination, Record, Settings};
use std::{
    io,
    path::PathBuf,
    sync::{Arc, Mutex},
};

pub struct Control {
    path: PathBuf,
    value: Mutex<Settings>,
}
impl Control {
    pub fn open(dir: &std::path::Path) -> anyhow::Result<Self> {
        let path = dir.join("self-logs.json");
        let loaded = std::fs::read(&path)
            .and_then(|bytes| serde_json::from_slice(&bytes).map_err(io::Error::other));
        let value = match loaded {
            Ok(value) => value,
            Err(error) if error.kind() == io::ErrorKind::NotFound => Settings::default(),
            Err(error) => {
                tracing::warn!(%error, "self-log settings unavailable; using defaults");
                Settings::default()
            }
        };
        Ok(Self {
            path,
            value: Mutex::new(value),
        })
    }
    pub fn get(&self) -> Settings {
        self.value.lock().unwrap().clone()
    }
    fn set(&self, value: Settings) -> anyhow::Result<()> {
        let mut current = self.value.lock().unwrap();
        let dir = self.path.parent().unwrap();
        lognara_spool::atomic_file(dir, "self-logs.json", &serde_json::to_vec(&value)?)?;
        *current = value;
        Ok(())
    }
}

pub struct Local(pub Arc<Core>);
impl Destination for Local {
    fn settings(&mut self) -> io::Result<Settings> {
        Ok(self.0.self_logs.get())
    }
    fn send(&mut self, body: &[u8]) -> io::Result<()> {
        let _permit = self
            .0
            .journal
            .ingest_slots
            .try_acquire()
            .map_err(io::Error::other)?;
        accept(&self.0, body, "lognara-core").map_err(|status| {
            let kind = if matches!(
                status,
                StatusCode::BAD_REQUEST | StatusCode::PAYLOAD_TOO_LARGE
            ) {
                io::ErrorKind::InvalidData
            } else {
                io::ErrorKind::Other
            };
            io::Error::new(kind, status.to_string())
        })
    }
}

pub async fn settings(State(core): State<Arc<Core>>) -> Json<Settings> {
    Json(core.self_logs.get())
}
pub async fn update(State(core): State<Arc<Core>>, Json(value): Json<Settings>) -> StatusCode {
    match tokio::task::spawn_blocking(move || core.self_logs.set(value)).await {
        Ok(Ok(())) => StatusCode::NO_CONTENT,
        _ => StatusCode::SERVICE_UNAVAILABLE,
    }
}
pub async fn dock(State(core): State<Arc<Core>>, body: Bytes) -> StatusCode {
    receive(core, body, "lognara-dock").await
}
pub async fn relay(State(core): State<Arc<Core>>, body: Bytes) -> StatusCode {
    receive(core, body, "lognara-relay").await
}
async fn receive(core: Arc<Core>, body: Bytes, service: &'static str) -> StatusCode {
    let Ok(permit) = core.journal.ingest_slots.clone().try_acquire_owned() else {
        return StatusCode::SERVICE_UNAVAILABLE;
    };
    match tokio::task::spawn_blocking(move || {
        let _permit = permit;
        accept(&core, &body, service)
    })
    .await
    {
        Ok(Ok(())) => StatusCode::NO_CONTENT,
        Ok(Err(status)) => status,
        Err(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
}

fn encode(body: &[u8], service: &str) -> Result<Vec<u8>, StatusCode> {
    if body.len() > lognara_selflog::MAX_BODY {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }
    let records: Vec<Record> = serde_json::from_slice(body).map_err(|_| StatusCode::BAD_REQUEST)?;
    if records.is_empty()
        || records.len() > 16
        || records.iter().any(|r| !r.valid() || r.service != service)
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    let groups = records
        .into_iter()
        .map(|record| Group {
            source: Source {
                environment: None,
                server: record.server.clone(),
                backend: "lognara".into(),
                service: record.service,
                service_instance: Some(record.server),
            },
            dropped: 0,
            events: vec![Event {
                id: LogId(record.id),
                timestamp: record.timestamp,
                ingested_at: record.timestamp,
                action: None,
                level: match record.level.as_str() {
                    "TRACE" => LogLevel::Trace,
                    "DEBUG" => LogLevel::Debug,
                    "INFO" => LogLevel::Info,
                    "WARN" => LogLevel::Warn,
                    _ => LogLevel::Error,
                },
                message: record.message,
                trace_id: None,
                span_id: None,
                parent_span_id: None,
                request_id: None,
                // One map entry keeps MessagePack bytes stable across retries for SHA-256 deduplication.
                attributes: [(
                    "selflog".into(),
                    serde_json::json!({"target":record.target,"fields":record.fields}),
                )]
                .into(),
            }],
        })
        .collect();
    let packed = rmp_serde::to_vec_named(&CoreBatch { dropped: 0, groups })
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    zstd::encode_all(&packed[..], 3).map_err(|_| StatusCode::SERVICE_UNAVAILABLE)
}
fn accept(core: &Core, body: &[u8], service: &str) -> Result<(), StatusCode> {
    let packed = encode(body, service)?;
    core.journal
        .accept(&packed)
        .map(|_| ())
        .map_err(ingest_status)
}

fn ingest_status(error: crate::journal::IngestError) -> StatusCode {
    use crate::{journal::IngestError, wire::DecodeError};
    match error {
        IngestError::TooLarge | IngestError::Invalid(DecodeError::TooLarge) => {
            StatusCode::PAYLOAD_TOO_LARGE
        }
        IngestError::Invalid(_) => StatusCode::BAD_REQUEST,
        IngestError::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_diagnostics_settings_do_not_block_core_startup() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("self-logs.json"), b"broken").unwrap();
        assert_eq!(
            Control::open(dir.path()).unwrap().get(),
            Settings::default()
        );
        assert_eq!(
            ingest_status(crate::journal::IngestError::TooLarge),
            StatusCode::PAYLOAD_TOO_LARGE
        );
        assert_eq!(
            ingest_status(crate::journal::IngestError::Invalid(
                crate::wire::DecodeError::Invalid
            )),
            StatusCode::BAD_REQUEST
        );
    }
    #[test]
    fn retries_keep_wire_bytes_and_cannot_impersonate_other_components() {
        let records = vec![Record {
            id: uuid::Uuid::now_v7(),
            timestamp: 1_790_678_400_000_000_000,
            service: "lognara-relay".into(),
            server: "node-1".into(),
            level: "ERROR".into(),
            target: "lognara_relay".into(),
            message: "delivery failed".into(),
            fields: [("error".into(), "unavailable".into())].into(),
        }];
        let body = serde_json::to_vec(&records).unwrap();
        let packed = encode(&body, "lognara-relay").unwrap();
        assert_eq!(packed, encode(&body, "lognara-relay").unwrap());
        let decoded = crate::wire::decode(&packed, 1 << 20).unwrap();
        assert_eq!(decoded.groups[0].source.service, "lognara-relay");
        assert_eq!(decoded.groups[0].events[0].level, LogLevel::Error);
        assert_eq!(encode(&body, "lognara-dock"), Err(StatusCode::BAD_REQUEST));
    }
}
