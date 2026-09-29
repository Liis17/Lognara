//! Низкокардинальные метрики Prometheus без содержимого логов и токенов.
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[derive(Default)]
pub struct Metrics {
    pub accepted_batches: AtomicU64,
    pub accepted_events: AtomicU64,
    pub duplicate_batches: AtomicU64,
    pub rejected_batches: AtomicU64,
    pub relay_dropped: AtomicU64,
    pub agent_dropped: AtomicU64,
    pub wal_bytes: AtomicU64,
    pub visible_sequence: AtomicU64,
    pub visibility_lag_ms: AtomicU64,
    pub sealed_segments: AtomicU64,
    pub deleted_segments: AtomicU64,
    pub queries: AtomicU64,
    pub query_errors: AtomicU64,
    pub query_duration_us: AtomicU64,
    pub ack_duration_us: AtomicU64,
}

impl Metrics {
    pub fn render(&self) -> String {
        let mut text = String::new();
        for (name, value, kind) in [
            ("accepted_batches_total", &self.accepted_batches, "counter"),
            ("accepted_events_total", &self.accepted_events, "counter"),
            (
                "duplicate_batches_total",
                &self.duplicate_batches,
                "counter",
            ),
            ("rejected_batches_total", &self.rejected_batches, "counter"),
            ("relay_dropped_events_total", &self.relay_dropped, "counter"),
            ("agent_dropped_events_total", &self.agent_dropped, "counter"),
            ("wal_bytes", &self.wal_bytes, "gauge"),
            ("visible_sequence", &self.visible_sequence, "gauge"),
            (
                "visibility_lag_milliseconds",
                &self.visibility_lag_ms,
                "gauge",
            ),
            ("sealed_segments_total", &self.sealed_segments, "counter"),
            ("deleted_segments_total", &self.deleted_segments, "counter"),
            ("queries_total", &self.queries, "counter"),
            ("query_errors_total", &self.query_errors, "counter"),
            (
                "query_duration_microseconds_total",
                &self.query_duration_us,
                "counter",
            ),
            (
                "ack_duration_microseconds_total",
                &self.ack_duration_us,
                "counter",
            ),
        ] {
            text.push_str(&format!(
                "# TYPE lognara_core_{name} {kind}\nlognara_core_{name} {}\n",
                value.load(Relaxed)
            ));
        }
        text
    }
}
