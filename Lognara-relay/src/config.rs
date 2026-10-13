//! Параметры запуска из переменных окружения `LOGNARA_*`.

use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use reqwest::Url;

const DEFAULT_FLUSH_INTERVAL_MS: u64 = 60_000;
const DEFAULT_BATCH_SIZE: usize = 10_000;
const DEFAULT_MAX_BUFFER: usize = 100_000;
const DEFAULT_LISTEN_ADDR: &str = "127.0.0.1:7401";
const DEFAULT_SPOOL_DIR: &str = "/var/lib/lognara-relay/spool";
const DEFAULT_SPOOL_MAX_MB: u64 = 1024;

#[derive(Clone, PartialEq)]
pub struct Config {
    pub core_url: Url,
    pub core_token: String,
    /// Как часто отправлять накопленное в core.
    pub flush_interval: Duration,
    /// Сколько событий накопить, чтобы отправить их раньше интервала.
    pub batch_size: usize,
    /// Сколько подтверждённых событий держать в очереди; сверх лимита 503.
    pub max_buffer: usize,
    pub listen_addr: SocketAddr,
    /// Каталог для пачек, которые не удалось отправить в core.
    pub spool_dir: PathBuf,
    /// Лимит spool; при переполнении новые запросы получают 503.
    pub spool_max_bytes: u64,
    /// Должны быть не больше соответствующих лимитов core.
    pub core_max_body_bytes: usize,
    pub core_max_decoded_bytes: usize,
    pub core_max_model_bytes: usize,
    /// Общий RAM процесса или контейнера, MB; резервы получают его за вычетом `RUNTIME_RESERVE`.
    pub memory_mb: usize,
    pub max_model_bytes: usize,
    pub max_buffer_bytes: usize,
    pub max_ingest_concurrency: usize,
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("core_url", &self.core_url)
            .field("core_token", &"[redacted]")
            .field("flush_interval", &self.flush_interval)
            .field("batch_size", &self.batch_size)
            .field("max_buffer", &self.max_buffer)
            .field("listen_addr", &self.listen_addr)
            .field("spool_dir", &self.spool_dir)
            .field("spool_max_bytes", &self.spool_max_bytes)
            .field("core_max_body_bytes", &self.core_max_body_bytes)
            .field("core_max_decoded_bytes", &self.core_max_decoded_bytes)
            .field("core_max_model_bytes", &self.core_max_model_bytes)
            .field("memory_mb", &self.memory_mb)
            .field("max_model_bytes", &self.max_model_bytes)
            .field("max_buffer_bytes", &self.max_buffer_bytes)
            .field("max_ingest_concurrency", &self.max_ingest_concurrency)
            .finish()
    }
}

#[derive(Debug, PartialEq)]
pub enum ConfigError {
    Missing(&'static str),
    Invalid {
        var: &'static str,
        value: String,
        expected: &'static str,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(var) => write!(f, "{var} is required"),
            Self::Invalid {
                var,
                value,
                expected,
            } => write!(f, "{var}={value:?} is invalid, expected {expected}"),
        }
    }
}

impl std::error::Error for ConfigError {}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// Собирает конфигурацию через `lookup`; пустые значения считаются незаданными.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let get = |name: &str| lookup(name).filter(|value| !value.is_empty());
        let required = |var: &'static str| get(var).ok_or(ConfigError::Missing(var));

        let core_url = required("LOGNARA_CORE_URL")?;
        let core_url = match core_url.parse::<Url>() {
            Ok(url) if matches!(url.scheme(), "http" | "https") => url,
            _ => {
                return Err(ConfigError::Invalid {
                    var: "LOGNARA_CORE_URL",
                    value: core_url,
                    expected: "an http or https URL",
                });
            }
        };

        let batch_size = positive(&get, "LOGNARA_BATCH_SIZE", DEFAULT_BATCH_SIZE)?;
        let max_buffer = positive(&get, "LOGNARA_MAX_BUFFER", DEFAULT_MAX_BUFFER)?;
        if max_buffer < batch_size {
            return Err(ConfigError::Invalid {
                var: "LOGNARA_MAX_BUFFER",
                value: max_buffer.to_string(),
                expected: "a value >= LOGNARA_BATCH_SIZE",
            });
        }
        let flush_interval_ms =
            positive(&get, "LOGNARA_FLUSH_INTERVAL_MS", DEFAULT_FLUSH_INTERVAL_MS)?;
        let spool_max_mb = positive(&get, "LOGNARA_SPOOL_MAX_MB", DEFAULT_SPOOL_MAX_MB)?;

        let config = Self {
            core_url,
            core_token: required("LOGNARA_CORE_TOKEN")?,
            flush_interval: Duration::from_millis(flush_interval_ms),
            batch_size,
            max_buffer,
            listen_addr: parse(
                &get,
                "LOGNARA_LISTEN_ADDR",
                DEFAULT_LISTEN_ADDR,
                "an address like 127.0.0.1:7401",
            )?,
            spool_dir: get("LOGNARA_SPOOL_DIR")
                .unwrap_or_else(|| DEFAULT_SPOOL_DIR.to_owned())
                .into(),
            spool_max_bytes: spool_max_mb
                .checked_mul(1024 * 1024)
                .ok_or(ConfigError::Invalid {
                    var: "LOGNARA_SPOOL_MAX_MB",
                    value: spool_max_mb.to_string(),
                    expected: "a byte quota without overflow",
                })?,
            core_max_body_bytes: positive(&get, "LOGNARA_CORE_MAX_BODY_BYTES", 64 << 20)?,
            core_max_decoded_bytes: positive(&get, "LOGNARA_CORE_MAX_DECODED_BYTES", 256 << 20)?,
            core_max_model_bytes: positive(
                &get,
                "LOGNARA_CORE_MAX_MODEL_BYTES",
                crate::wire_budget::DEFAULT_MODEL_BYTES,
            )?,
            memory_mb: positive(
                &get,
                "LOGNARA_RELAY_MEMORY_MB",
                crate::memory::DEFAULT_MEMORY_MB,
            )?,
            max_model_bytes: positive(
                &get,
                "LOGNARA_RELAY_MAX_MODEL_BYTES",
                crate::memory::DEFAULT_MODEL,
            )?,
            max_buffer_bytes: positive(
                &get,
                "LOGNARA_RELAY_MAX_BUFFER_BYTES",
                crate::memory::DEFAULT_BUFFER,
            )?,
            max_ingest_concurrency: positive(&get, "LOGNARA_RELAY_MAX_INGEST_CONCURRENCY", 1usize)?,
        };
        config.validate_memory()?;
        Ok(config)
    }

    /// Входной резерв округляется вверх до 128 MiB, по умолчанию 1 GiB.
    pub fn ingest_request_bytes(&self) -> Option<usize> {
        use crate::memory::{CODEC_WORKSPACE, MAX_BODY, MAX_DECODED};
        let unit = 128 << 20;
        let bytes = MAX_BODY
            .max(self.core_max_body_bytes)
            .checked_mul(2)?
            .checked_add((MAX_DECODED + 1).max(self.core_max_decoded_bytes))?
            .checked_add(self.max_model_bytes)?
            .checked_add(CODEC_WORKSPACE)?
            .checked_add(unit)?;
        bytes.checked_add(unit - 1).map(|n| n / unit * unit)
    }

    pub fn sender_bytes(&self) -> Option<usize> {
        self.core_max_body_bytes
            .checked_mul(2)?
            .checked_add(self.core_max_decoded_bytes)?
            .checked_add(crate::memory::CODEC_WORKSPACE)
    }

    /// Память под резервы приёма, моделей и отправки: общий RAM без запаса runtime.
    pub fn reserve_bytes(&self) -> Option<usize> {
        self.memory_mb
            .checked_mul(1 << 20)?
            .checked_sub(crate::memory::RUNTIME_RESERVE)
    }

    pub fn validate_memory(&self) -> Result<(), ConfigError> {
        if self.flush_interval.is_zero()
            || self.batch_size == 0
            || self.max_buffer < self.batch_size
            || self.spool_max_bytes == 0
        {
            return Err(ConfigError::Invalid {
                var: "LOGNARA_MAX_BUFFER",
                value: self.max_buffer.to_string(),
                expected: "positive spool quota and max buffer >= positive batch size",
            });
        }
        let required = self
            .ingest_request_bytes()
            .and_then(|bytes| bytes.checked_mul(self.max_ingest_concurrency))
            .and_then(|bytes| bytes.checked_add(self.max_buffer_bytes))
            .and_then(|bytes| {
                self.sender_bytes()
                    .and_then(|sender| bytes.checked_add(sender))
            });
        if self.max_ingest_concurrency == 0
            || self.max_ingest_concurrency > u32::MAX as usize
            // Должны помещаться хотя бы короткая текстовая запись и её контейнеры.
            || self.max_model_bytes < 8192
            || self.max_buffer_bytes < 8192
            || self.core_max_body_bytes == 0
            || self.core_max_decoded_bytes == 0
            || self.core_max_model_bytes == 0
            || self.memory_mb < crate::memory::MIN_MEMORY_MB
            || self
                .reserve_bytes()
                .is_none_or(|reserve| required.is_none_or(|bytes| bytes > reserve))
        {
            return Err(ConfigError::Invalid {
                var: "LOGNARA_RELAY_MEMORY_MB",
                value: self.memory_mb.to_string(),
                expected: "model/buffer >= 8192 bytes and total RAM in MB >= 2048 covering the 256 MiB runtime reserve plus ingest, buffer and sender reserves without overflow",
            });
        }
        Ok(())
    }
}

fn positive<T>(
    get: &impl Fn(&str) -> Option<String>,
    var: &'static str,
    default: T,
) -> Result<T, ConfigError>
where
    T: FromStr + PartialOrd + Default,
{
    let Some(value) = get(var) else {
        return Ok(default);
    };
    match value.parse::<T>() {
        Ok(number) if number > T::default() => Ok(number),
        _ => Err(ConfigError::Invalid {
            var,
            value,
            expected: "a positive integer",
        }),
    }
}

fn parse<T: FromStr>(
    get: &impl Fn(&str) -> Option<String>,
    var: &'static str,
    default: &str,
    expected: &'static str,
) -> Result<T, ConfigError> {
    let value = get(var).unwrap_or_else(|| default.to_owned());
    value.parse().map_err(|_| ConfigError::Invalid {
        var,
        value,
        expected,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_memory_below_partition_requirements() {
        assert!(with_required(&[("LOGNARA_RELAY_MEMORY_MB", "1280")]).is_err());
        assert!(with_required(&[("LOGNARA_RELAY_MAX_INGEST_CONCURRENCY", "2")]).is_err());
    }

    #[test]
    fn rejects_model_and_buffer_that_cannot_accept_a_record() {
        for var in [
            "LOGNARA_RELAY_MAX_MODEL_BYTES",
            "LOGNARA_RELAY_MAX_BUFFER_BYTES",
        ] {
            for bytes in ["1", "8191"] {
                assert!(with_required(&[(var, bytes)]).is_err(), "{var}={bytes}");
            }
            assert!(with_required(&[(var, "8192")]).is_ok());
        }
    }

    #[test]
    fn validates_defaults_concurrency_and_checked_arithmetic() {
        let config = with_required(&[]).unwrap();
        assert_eq!(config.ingest_request_bytes(), Some(1024 << 20));
        assert_eq!(config.sender_bytes(), Some(512 << 20));
        assert_eq!(config.memory_mb, 2048);
        assert_eq!(config.reserve_bytes(), Some(1792 << 20));
        let two = with_required(&[
            ("LOGNARA_RELAY_MEMORY_MB", "3072"),
            ("LOGNARA_RELAY_MAX_INGEST_CONCURRENCY", "2"),
        ])
        .unwrap();
        assert_eq!(two.max_ingest_concurrency, 2);
        assert_eq!(two.reserve_bytes(), Some(2816 << 20));
        assert!(
            with_required(&[
                ("LOGNARA_RELAY_MEMORY_MB", "3071"),
                ("LOGNARA_RELAY_MAX_INGEST_CONCURRENCY", "2"),
            ])
            .is_err()
        );
        let mut overflow = config;
        overflow.memory_mb = usize::MAX;
        assert!(overflow.validate_memory().is_err());
        overflow.memory_mb = 2048;
        overflow.max_model_bytes = usize::MAX;
        assert!(overflow.validate_memory().is_err());
        overflow.max_model_bytes = 1;
        overflow.core_max_body_bytes = usize::MAX;
        assert!(overflow.validate_memory().is_err());
    }

    #[test]
    fn debug_redacts_core_token() {
        let config = with_required(&[("LOGNARA_CORE_TOKEN", "unique-core-secret")]).unwrap();
        let debug = format!("{config:?}");
        assert!(!debug.contains("unique-core-secret"));
        assert!(debug.contains("[redacted]"));
    }

    #[test]
    fn memory_is_total_ram_in_mb_and_names_its_variable_on_error() {
        for mb in ["2047", "1280"] {
            let err = with_required(&[("LOGNARA_RELAY_MEMORY_MB", mb)]).unwrap_err();
            assert!(matches!(
                &err,
                ConfigError::Invalid {
                    var: "LOGNARA_RELAY_MEMORY_MB",
                    ..
                }
            ));
            assert!(err.to_string().contains("LOGNARA_RELAY_MEMORY_MB"));
        }
        for mb in ["0", "abc", "-1"] {
            assert!(with_required(&[("LOGNARA_RELAY_MEMORY_MB", mb)]).is_err());
        }
    }

    const REQUIRED: [(&str, &str); 2] = [
        ("LOGNARA_CORE_URL", "https://core.example.com/v1/batches"),
        ("LOGNARA_CORE_TOKEN", "secret"),
    ];

    fn config(vars: &[(&str, &str)]) -> Result<Config, ConfigError> {
        Config::from_lookup(|name| {
            vars.iter()
                .rev()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.to_string())
        })
    }

    fn with_required(extra: &[(&'static str, &'static str)]) -> Result<Config, ConfigError> {
        config(&[&REQUIRED[..], extra].concat())
    }

    #[test]
    fn applies_defaults() {
        let config = with_required(&[]).unwrap();

        assert_eq!(
            config.core_url.as_str(),
            "https://core.example.com/v1/batches"
        );
        assert_eq!(config.core_token, "secret");
        assert_eq!(config.flush_interval, Duration::from_secs(60));
        assert_eq!(config.batch_size, 10_000);
        assert_eq!(config.max_buffer, 100_000);
        assert_eq!(
            config.listen_addr,
            "127.0.0.1:7401".parse::<SocketAddr>().unwrap()
        );
        assert_eq!(
            config.spool_dir,
            PathBuf::from("/var/lib/lognara-relay/spool")
        );
        assert_eq!(config.spool_max_bytes, 1024 * 1024 * 1024);
    }

    #[test]
    fn reads_all_variables() {
        let config = with_required(&[
            ("LOGNARA_CORE_URL", "http://127.0.0.1:9000/batches"),
            ("LOGNARA_FLUSH_INTERVAL_MS", "250"),
            ("LOGNARA_BATCH_SIZE", "50"),
            ("LOGNARA_MAX_BUFFER", "500"),
            ("LOGNARA_LISTEN_ADDR", "127.0.0.1:9001"),
            ("LOGNARA_SPOOL_DIR", "/data/spool"),
            ("LOGNARA_SPOOL_MAX_MB", "2"),
        ])
        .unwrap();

        assert_eq!(config.core_url.as_str(), "http://127.0.0.1:9000/batches");
        assert_eq!(config.flush_interval, Duration::from_millis(250));
        assert_eq!(config.batch_size, 50);
        assert_eq!(config.max_buffer, 500);
        assert_eq!(
            config.listen_addr,
            "127.0.0.1:9001".parse::<SocketAddr>().unwrap()
        );
        assert_eq!(config.spool_dir, PathBuf::from("/data/spool"));
        assert_eq!(config.spool_max_bytes, 2 * 1024 * 1024);
    }

    #[test]
    fn requires_relay_variables() {
        for (missing, _) in REQUIRED {
            let vars: Vec<_> = REQUIRED
                .into_iter()
                .filter(|(name, _)| *name != missing)
                .collect();

            assert_eq!(config(&vars), Err(ConfigError::Missing(missing)));
        }
        assert_eq!(
            ConfigError::Missing("LOGNARA_CORE_URL").to_string(),
            "LOGNARA_CORE_URL is required"
        );
    }

    #[test]
    fn treats_empty_value_as_missing() {
        assert_eq!(
            with_required(&[("LOGNARA_CORE_TOKEN", "")]),
            Err(ConfigError::Missing("LOGNARA_CORE_TOKEN"))
        );
        assert_eq!(
            with_required(&[("LOGNARA_SPOOL_DIR", "")])
                .unwrap()
                .spool_dir,
            PathBuf::from("/var/lib/lognara-relay/spool")
        );
    }

    #[test]
    fn rejects_non_http_core_url() {
        for value in ["core:9000", "ftp://core/batches"] {
            assert_eq!(
                with_required(&[("LOGNARA_CORE_URL", value)]),
                Err(ConfigError::Invalid {
                    var: "LOGNARA_CORE_URL",
                    value: value.to_string(),
                    expected: "an http or https URL",
                })
            );
        }
    }

    #[test]
    fn rejects_non_positive_numbers() {
        for var in [
            "LOGNARA_FLUSH_INTERVAL_MS",
            "LOGNARA_BATCH_SIZE",
            "LOGNARA_SPOOL_MAX_MB",
        ] {
            for value in ["abc", "0", "-5"] {
                assert_eq!(
                    with_required(&[(var, value)]),
                    Err(ConfigError::Invalid {
                        var,
                        value: value.to_string(),
                        expected: "a positive integer",
                    })
                );
            }
        }
    }

    #[test]
    fn rejects_max_buffer_below_batch_size() {
        let result = with_required(&[("LOGNARA_BATCH_SIZE", "100"), ("LOGNARA_MAX_BUFFER", "99")]);

        assert!(matches!(
            result,
            Err(ConfigError::Invalid {
                var: "LOGNARA_MAX_BUFFER",
                ..
            })
        ));
    }

    #[test]
    fn rejects_invalid_listen_addr() {
        let result = with_required(&[("LOGNARA_LISTEN_ADDR", "localhost")]);

        assert!(matches!(
            result,
            Err(ConfigError::Invalid {
                var: "LOGNARA_LISTEN_ADDR",
                ..
            })
        ));
    }
}
