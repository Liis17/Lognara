//! Проверяемая конфигурация из окружения. Секреты не попадают в Debug или ошибки.

use std::{net::SocketAddr, path::PathBuf, time::Duration};

use anyhow::{Result, bail};

#[derive(Clone)]
pub struct Config {
    pub listen_addr: SocketAddr,
    pub data_dir: PathBuf,
    pub ingest_token: String,
    pub query_token: String,
    pub max_body_bytes: usize,
    pub max_decoded_bytes: usize,
    pub max_model_bytes: usize,
    pub ingest_memory_bytes: usize,
    pub wal_max_bytes: u64,
    pub disk_reserve_bytes: u64,
    pub segment_rows: usize,
    pub segment_bytes: usize,
    pub segment_age: Duration,
    pub refresh_interval: Duration,
    pub retention: Duration,
    pub query_memory_bytes: usize,
    pub search_memory_bytes: usize,
    pub query_timeout: Duration,
    pub index_memory_bytes: usize,
    pub index_cache_entries: usize,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let get = |key| lookup(key).filter(|value| !value.is_empty());
        let required = |key| get(key).ok_or_else(|| anyhow::anyhow!("{key} is required"));
        let number = |key, default: u64| -> Result<u64> {
            match get(key) {
                None => Ok(default),
                Some(value) => match value.parse::<u64>() {
                    Ok(value) if value > 0 => Ok(value),
                    _ => bail!("{key} must be a positive integer"),
                },
            }
        };
        let config = Self {
            listen_addr: get("LOGNARA_LISTEN_ADDR")
                .unwrap_or_else(|| "127.0.0.1:7402".into())
                .parse()?,
            data_dir: get("LOGNARA_DATA_DIR")
                .unwrap_or_else(|| "/var/lib/lognara-core".into())
                .into(),
            ingest_token: required("LOGNARA_INGEST_TOKEN")?,
            query_token: required("LOGNARA_QUERY_TOKEN")?,
            max_body_bytes: number("LOGNARA_MAX_BODY_BYTES", 64 << 20)? as usize,
            max_decoded_bytes: number("LOGNARA_MAX_DECODED_BYTES", 256 << 20)? as usize,
            max_model_bytes: number(
                "LOGNARA_MAX_MODEL_BYTES",
                crate::wire_budget::DEFAULT_MODEL_BYTES as u64,
            )? as usize,
            ingest_memory_bytes: number("LOGNARA_INGEST_MEMORY_BYTES", 1 << 30)? as usize,
            wal_max_bytes: number("LOGNARA_WAL_MAX_BYTES", 4 << 30)?,
            disk_reserve_bytes: number("LOGNARA_DISK_RESERVE_BYTES", 1 << 30)?,
            segment_rows: number("LOGNARA_SEGMENT_ROWS", 250_000)? as usize,
            segment_bytes: number("LOGNARA_SEGMENT_BYTES", 256 << 20)? as usize,
            segment_age: Duration::from_secs(number("LOGNARA_SEGMENT_SECONDS", 300)?),
            refresh_interval: Duration::from_millis(number("LOGNARA_REFRESH_MS", 1000)?),
            retention: Duration::from_secs(number("LOGNARA_RETENTION_SECONDS", 7 * 86400)?),
            query_memory_bytes: number("LOGNARA_QUERY_MEMORY_BYTES", 1 << 30)? as usize,
            search_memory_bytes: number("LOGNARA_SEARCH_MEMORY_BYTES", 128 << 20)? as usize,
            query_timeout: Duration::from_secs(number("LOGNARA_QUERY_TIMEOUT_SECONDS", 30)?),
            index_memory_bytes: number("LOGNARA_INDEX_MEMORY_BYTES", 64 << 20)? as usize,
            index_cache_entries: number("LOGNARA_INDEX_CACHE_ENTRIES", 32)? as usize,
        };
        if config.ingest_token == config.query_token {
            bail!("LOGNARA_INGEST_TOKEN and LOGNARA_QUERY_TOKEN must differ");
        }
        if config.max_body_bytes > u32::MAX as usize
            || config.max_decoded_bytes > u32::MAX as usize
            || config.max_model_bytes > u32::MAX as usize
            || config.ingest_memory_bytes > u32::MAX as usize
            || config.ingest_memory_bytes < config.ingest_request_bytes()
        {
            bail!(
                "ingest byte limits must fit u32 and memory must cover body, decoded capacity and model"
            );
        }
        if config.index_memory_bytes < 15_000_000 {
            bail!("LOGNARA_INDEX_MEMORY_BYTES must be at least 15000000");
        }
        if config.retention.as_secs() > i64::MAX as u64 / 1_000_000_000 {
            bail!("LOGNARA_RETENTION_SECONDS is too large");
        }
        if config.query_timeout > Duration::from_secs(3600)
            || config.refresh_interval > Duration::from_secs(3600)
            || config.segment_age.as_secs() > i64::MAX as u64 / 1_000_000_000
        {
            bail!("query/refresh must be <= 3600 seconds; segment age must fit i64 nanoseconds");
        }
        Ok(config)
    }

    pub fn ingest_request_bytes(&self) -> usize {
        // read_to_end может зарезервировать до удвоенного decoded limit.
        self.max_body_bytes + 2 * (self.max_decoded_bytes + 1) + self.max_model_bytes
    }
}
