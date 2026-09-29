//! Генерация совместимой fixture настоящим encoder relay.
use lognara_relay::{
    agent_wire::{Payload, Record},
    core_wire::{self, CoreBatch, Group, Source},
    normalize,
};

fn main() -> anyhow::Result<()> {
    let path = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("usage: relay_fixture <output>"))?;
    let batch = CoreBatch {
        dropped: 2,
        groups: vec![Group {
            source: Source { environment: Some("production".into()), server: "eu-prod-01".into(),
                backend: "barkcloud".into(), service: "api".into(), service_instance: Some("api-2".into()) },
            dropped: 3,
            events: vec![normalize::event(Record {
                received_at: 1_790_678_511_894_000_001,
                payload: Payload::Json(serde_json::json!({
                    "id": "019d3f8e-7c2a-7b3e-9f1d-2a4b6c8d0e1f",
                    "timestamp": "2026-09-29T10:41:51.871000001Z",
                    "level": "error", "action": "file.upload", "message": "Failed to save file metadata",
                    "trace_id": "8c21f0a4b6c8d0e12a4b6c8d0e1f3a5b", "span_id": "91af2a4b6c8d0e1f",
                    "parent_span_id": "72de2a4b6c8d0e1f", "request_id": "req_019d",
                    "attributes": {"user_id": 18271, "large": 18446744073709551615_u64,
                        "nested": [true, null, {"x": 0.5}], "exception.type": "S3TimeoutException"}
                }).to_string()),
            })],
        }],
    };
    std::fs::write(path, core_wire::encode(&batch))?;
    Ok(())
}
