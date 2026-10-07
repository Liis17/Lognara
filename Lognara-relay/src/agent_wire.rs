//! Формат пачки, которую присылает lognara-agent. Типы повторяют `Lognara-agent/src/wire.rs`.
//!
//! Тело запроса: `Batch` в MessagePack (поля по именам), сжатый zstd.
//! Заголовки: `Content-Type: application/msgpack`, `Content-Encoding: zstd`.
//! Авторизация: `Authorization: Bearer <LOGNARA_RELAY_TOKEN>`.

use std::io::Read;

use crate::model_budget::ModelBudget;
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
    let limit = usize::try_from(limit).map_err(|_| DecodeError::TooLarge)?;
    decode_with_budget(
        body,
        limit,
        &mut ModelBudget::new(crate::wire_budget::DEFAULT_MODEL_BYTES),
    )
}

pub fn decode_with_budget(
    body: &[u8],
    limit: usize,
    budget: &mut ModelBudget,
) -> Result<Batch, DecodeError> {
    let mut decoder = zstd::stream::read::Decoder::new(body).map_err(|_| DecodeError::Invalid)?;
    decoder
        .window_log_max(25)
        .map_err(|_| DecodeError::Invalid)?;
    let capacity = limit.checked_add(1).ok_or(DecodeError::TooLarge)?;
    let mut packed = vec![0; capacity];
    let mut length = 0;
    while length < capacity {
        let read = decoder
            .read(&mut packed[length..])
            .map_err(|_| DecodeError::Invalid)?;
        if read == 0 {
            break;
        }
        length += read;
    }
    if length > limit {
        return Err(DecodeError::TooLarge);
    }
    packed.truncate(length);
    let estimated =
        crate::wire_budget::estimate(&packed, budget.remaining()).map_err(|error| match error {
            crate::wire_budget::BudgetError::TooLarge => DecodeError::TooLarge,
            crate::wire_budget::BudgetError::Invalid => DecodeError::Invalid,
        })?;
    budget
        .charge(estimated)
        .map_err(|_| DecodeError::TooLarge)?;
    let mut decoder = rmp_serde::Deserializer::from_read_ref(&packed);
    decoder.set_max_depth(64);
    Batch::deserialize(&mut decoder).map_err(|_| DecodeError::Invalid)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Пачка, закодированная `wire::encode` из lognara-agent.
    const AGENT_BATCH: &[u8] = include_bytes!("../tests/fixtures/agent-batch.bin");

    #[test]
    fn enforces_decoded_boundary_model_budget_and_window_before_serde() {
        let length = zstd::decode_all(AGENT_BATCH).unwrap().len();
        assert!(decode(AGENT_BATCH, length as u64).is_ok());
        assert_eq!(
            decode(AGENT_BATCH, length as u64 - 1),
            Err(DecodeError::TooLarge)
        );
        assert_eq!(
            decode_with_budget(AGENT_BATCH, 1024, &mut ModelBudget::new(128)),
            Err(DecodeError::TooLarge)
        );
        // Пустой zstd frame с окном 128 MiB (window descriptor 0x88).
        let excessive_window = [0x28, 0xb5, 0x2f, 0xfd, 0x00, 0x88, 0x01, 0x00, 0x00];
        assert_eq!(decode(&excessive_window, 1024), Err(DecodeError::Invalid));
        let forged = zstd::encode_all(&[0xdd, 0xff, 0xff, 0xff, 0xff][..], 3).unwrap();
        assert_eq!(decode(&forged, 1024), Err(DecodeError::Invalid));
    }

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
