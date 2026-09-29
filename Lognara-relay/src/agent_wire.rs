//! Формат пачки, которую присылает lognara-agent. Типы повторяют `Lognara-agent/src/wire.rs`.
//!
//! Тело запроса: `Batch` в MessagePack (поля по именам), сжатый zstd.
//! Заголовки: `Content-Type: application/msgpack`, `Content-Encoding: zstd`.

use std::io::Read;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Batch {
    pub service: String,
    pub server: String,
    pub backend: String,
    pub environment: Option<String>,
    pub service_instance: Option<String>,
    /// Момент отправки пачки агентом, Unix-время в наносекундах.
    pub sent_at: i64,
    /// Сколько записей агент вытеснил из переполненного буфера с прошлой пачки.
    pub dropped: u64,
    pub records: Vec<Record>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    /// Момент приёма агентом, Unix-время в наносекундах.
    pub received_at: i64,
    pub payload: Payload,
}

/// Лог в том виде, в каком его прислало приложение.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Payload {
    Text(String),
    /// Текст одного JSON-значения.
    Json(String),
    Binary(#[serde(with = "serde_bytes")] Vec<u8>),
}

#[derive(Debug, PartialEq)]
pub enum DecodeError {
    /// Распакованная пачка больше лимита.
    TooLarge,
    /// Тело не является zstd с пачкой в MessagePack.
    Invalid,
}

/// Распаковывает zstd не больше чем в `limit` байт и разбирает пачку из MessagePack.
pub fn decode(body: &[u8], limit: u64) -> Result<Batch, DecodeError> {
    let decoder = zstd::stream::read::Decoder::new(body).map_err(|_| DecodeError::Invalid)?;
    let mut packed = Vec::new();
    decoder
        .take(limit + 1)
        .read_to_end(&mut packed)
        .map_err(|_| DecodeError::Invalid)?;
    if packed.len() as u64 > limit {
        return Err(DecodeError::TooLarge);
    }
    rmp_serde::from_slice(&packed).map_err(|_| DecodeError::Invalid)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Пачка, закодированная `wire::encode` из lognara-agent.
    const AGENT_BATCH: &[u8] = include_bytes!("../tests/fixtures/agent-batch.bin");

    #[test]
    fn decodes_batch_encoded_by_agent() {
        let batch = decode(AGENT_BATCH, 1024).unwrap();

        assert_eq!(
            batch,
            Batch {
                service: "api".into(),
                server: "eu-prod-01".into(),
                backend: "barkcloud".into(),
                environment: Some("production".into()),
                service_instance: None,
                sent_at: 1_790_000_000_000_000_000,
                dropped: 7,
                records: vec![
                    Record {
                        received_at: 1,
                        payload: Payload::Text("hello".into()),
                    },
                    Record {
                        received_at: 2,
                        payload: Payload::Json(r#"{"level":"error"}"#.into()),
                    },
                    Record {
                        received_at: 3,
                        payload: Payload::Binary(vec![0, 159, 146, 150]),
                    },
                ],
            }
        );
    }

    #[test]
    fn rejects_batch_larger_than_limit() {
        assert_eq!(decode(AGENT_BATCH, 16), Err(DecodeError::TooLarge));
    }

    #[test]
    fn rejects_invalid_body() {
        let not_msgpack = zstd::encode_all(&b"hello"[..], 3).unwrap();

        assert_eq!(decode(b"not zstd", 1024), Err(DecodeError::Invalid));
        assert_eq!(decode(&not_msgpack, 1024), Err(DecodeError::Invalid));
    }
}
