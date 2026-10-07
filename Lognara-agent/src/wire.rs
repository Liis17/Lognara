//! Формат пачки, которую агент отправляет в lognara-relay.
//!
//! Тело запроса: `Batch` в MessagePack (поля по именам), сжатый zstd.
//! Заголовки: `Content-Type: application/msgpack`, `Content-Encoding: zstd`.
//! Авторизация: `Authorization: Bearer <LOGNARA_RELAY_TOKEN>`.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

const ZSTD_LEVEL: i32 = 3;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Batch {
    pub service: String,
    pub server: String,
    pub backend: String,
    pub environment: Option<String>,
    pub service_instance: Option<String>,
    /// Момент отправки пачки, Unix-время в наносекундах.
    pub sent_at: i64,
    /// Счётчик совместимости со старыми агентами; новые пачки содержат 0.
    pub dropped: u64,
    pub records: Vec<Record>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    /// Момент приёма агентом, Unix-время в наносекундах.
    pub received_at: i64,
    pub payload: Payload,
}

/// Лог в том виде, в каком его прислало приложение. Разбирает его relay.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Payload {
    Text(String),
    /// Проверенный текст одного JSON-значения.
    Json(String),
    Binary(#[serde(with = "serde_bytes")] Vec<u8>),
}

/// Кодирует пачку в MessagePack и сжимает zstd.
pub fn encode(batch: &Batch) -> Vec<u8> {
    // Запись в Vec не может завершиться ошибкой, а сериализация этих типов всегда успешна.
    let packed = rmp_serde::to_vec_named(batch).expect("batch is always serializable");
    zstd::encode_all(packed.as_slice(), ZSTD_LEVEL).expect("in-memory zstd encoding")
}

pub fn unix_nanos() -> i64 {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is before 1970");
    elapsed.as_nanos() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoded_batch_decodes_back() {
        let batch = Batch {
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
        };

        let body = encode(&batch);
        let packed = zstd::decode_all(body.as_slice()).unwrap();
        let decoded: Batch = rmp_serde::from_slice(&packed).unwrap();

        assert_eq!(decoded, batch);
    }
}
