//! Пачка агента → `CoreBatch` по тем же правилам, что у relay.
//!
//! Тело готовится один раз при подготовке пачки и повторяется теми же байтами,
//! поэтому UUID событий фиксируются до первой попытки, а core дедуплицирует повтор.

use std::io;

use lognara_agent::{BodyEncoder, SpoolError};
use lognara_relay::agent_wire::{Batch, Payload, Record};
use lognara_relay::core_wire::{self, AgentHash, CoreBatch, Group, LogLevel, Source};
use lognara_relay::model_budget::ModelBudget;
use lognara_relay::normalize;
use sha2::{Digest, Sha256};
use tracing::warn;

/// Сжатое тело для core; лимит core по умолчанию 64 MiB.
pub const BODY_LIMIT: usize = 16 << 20;

pub struct CoreEncoder {
    agent: AgentHash,
}

impl CoreEncoder {
    /// Группы подписываются хешем текущего токена: core относит их к узлу хоста.
    pub fn new(token: &str) -> Self {
        Self {
            agent: AgentHash(Sha256::digest(token.as_bytes()).into()),
        }
    }
}

impl BodyEncoder for CoreEncoder {
    fn encode(&self, packed: Vec<u8>, model_bytes: usize) -> Result<Vec<u8>, SpoolError> {
        let batch: Batch = rmp_serde::from_slice(&packed)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        drop(packed);
        let mut budget = ModelBudget::new(model_bytes);
        let mut events = Vec::with_capacity(batch.records.len());
        for record in batch.records {
            let received_at = record.received_at;
            let size = payload_len(&record.payload);
            match normalize::event_with_budget(record, &mut budget) {
                Ok(event) => events.push(event),
                // Одна неподходящая запись не должна останавливать всю очередь хоста.
                Err(_) => {
                    warn!(
                        size,
                        "record exceeds normalization budget; replaced by marker"
                    );
                    events.push(oversized(received_at, size));
                }
            }
        }
        let body = core_wire::encode(&CoreBatch {
            dropped: 0,
            groups: vec![Group {
                source: Source {
                    environment: batch.environment,
                    server: batch.server,
                    backend: batch.backend,
                    service: batch.service,
                    service_instance: batch.service_instance,
                },
                dropped: 0,
                events,
                agent: Some(self.agent),
            }],
        });
        if body.len() > BODY_LIMIT {
            return Err(SpoolError::TooLarge);
        }
        Ok(body)
    }

    fn body_limit(&self) -> usize {
        BODY_LIMIT
    }
}

fn payload_len(payload: &Payload) -> usize {
    match payload {
        Payload::Text(text) | Payload::Json(text) => text.len(),
        Payload::Binary(bytes) => bytes.len(),
    }
}

fn oversized(received_at: i64, size: usize) -> core_wire::Event {
    let mut event = normalize::event(Record {
        received_at,
        origin: None,
        payload: Payload::Text(format!(
            "lognara-hwagent: record of {size} bytes exceeds the normalization budget"
        )),
    });
    event.level = LogLevel::Warn;
    event
}

#[cfg(test)]
mod tests {
    use super::*;
    use lognara_relay::agent_wire::Origin;

    fn packed(records: Vec<Record>) -> Vec<u8> {
        rmp_serde::to_vec_named(&Batch {
            service: "host".into(),
            server: "mail".into(),
            backend: "host".into(),
            environment: Some("prod".into()),
            service_instance: None,
            sent_at: 1,
            dropped: 0,
            records,
        })
        .unwrap()
    }

    fn decode(body: &[u8]) -> CoreBatch {
        rmp_serde::from_slice(&zstd::decode_all(body).unwrap()).unwrap()
    }

    #[test]
    fn normalizes_like_relay_and_signs_group_with_token_hash() {
        let encoder = CoreEncoder::new("lnh_secret");
        let body = encoder
            .encode(
                packed(vec![
                    Record {
                        received_at: 5,
                        origin: Some(Origin {
                            source: "auth".into(),
                            path: Some("/var/log/auth.log".into()),
                            stream: None,
                            offset: 0,
                            end: 10,
                            fragment: 0,
                            continued: false,
                        }),
                        payload: Payload::Text("sudo: root".into()),
                    },
                    Record {
                        received_at: 6,
                        origin: None,
                        payload: Payload::Json(r#"{"level":"warn","message":"m","k":1}"#.into()),
                    },
                ]),
                64 << 20,
            )
            .unwrap();
        let batch = decode(&body);
        let group = &batch.groups[0];
        let hash: [u8; 32] = Sha256::digest(b"lnh_secret").into();
        assert_eq!(group.agent, Some(AgentHash(hash)));
        assert_eq!(group.source.server, "mail");
        assert_eq!(group.source.environment.as_deref(), Some("prod"));
        assert_eq!(group.events[0].message, "sudo: root");
        assert_eq!(
            group.events[0].attributes["log.file.path"],
            "/var/log/auth.log"
        );
        assert_eq!(group.events[1].level, LogLevel::Warn);
        assert_eq!(group.events[1].attributes["k"], 1);
    }

    #[test]
    fn record_over_budget_is_replaced_by_marker_instead_of_stalling() {
        let encoder = CoreEncoder::new("t");
        let body = encoder
            .encode(
                packed(vec![Record {
                    received_at: 7,
                    origin: None,
                    payload: Payload::Binary(vec![1; 64 << 10]),
                }]),
                16 << 10,
            )
            .unwrap();
        let batch = decode(&body);
        let event = &batch.groups[0].events[0];
        assert_eq!(event.level, LogLevel::Warn);
        assert!(event.message.contains("65536 bytes"));
        assert_eq!(event.timestamp, 7);
    }
}
