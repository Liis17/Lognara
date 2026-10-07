//! Параметры запуска из переменных окружения `LOGNARA_*`.

use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use reqwest::Url;

pub const INDEX_BYTES: usize = 8 << 20;
pub const INDEX_ENTRIES: usize = INDEX_BYTES / 768;

const DEFAULT_BATCH_SIZE: usize = 1000;
const DEFAULT_FLUSH_INTERVAL_MS: u64 = 5000;
const DEFAULT_MAX_BUFFER: usize = 100_000;
const DEFAULT_LISTEN_ADDR: &str = "127.0.0.1:7400";
const DEFAULT_RELAY_URL: &str = "http://lognara-relay:7401/v1/batches";

#[derive(Clone, PartialEq)]
pub struct Config {
    pub service: String,
    pub server: String,
    pub backend: String,
    pub environment: Option<String>,
    pub service_instance: Option<String>,
    /// Сколько записей накопить перед отправкой.
    pub batch_size: usize,
    /// Через сколько отправить накопленное, даже если пачка не набрана.
    pub flush_interval: Duration,
    /// Сколько подтверждённых записей держать в очереди; сверх лимита 503.
    pub max_buffer: usize,
    pub max_buffer_bytes: usize,
    pub max_batch_bytes: usize,
    pub max_record_bytes: usize,
    pub spool_dir: PathBuf,
    pub spool_max_bytes: u64,
    pub listen_addr: SocketAddr,
    pub relay_url: Url,
    pub relay_token: String,
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("service", &self.service)
            .field("server", &self.server)
            .field("backend", &self.backend)
            .field("environment", &self.environment)
            .field("service_instance", &self.service_instance)
            .field("batch_size", &self.batch_size)
            .field("flush_interval", &self.flush_interval)
            .field("max_buffer", &self.max_buffer)
            .field("max_buffer_bytes", &self.max_buffer_bytes)
            .field("max_batch_bytes", &self.max_batch_bytes)
            .field("max_record_bytes", &self.max_record_bytes)
            .field("spool_dir", &self.spool_dir)
            .field("spool_max_bytes", &self.spool_max_bytes)
            .field("listen_addr", &self.listen_addr)
            .field("relay_url", &self.relay_url)
            .field("relay_token", &"[redacted]")
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

        let relay_token = required("LOGNARA_RELAY_TOKEN")?;
        if !relay_token.bytes().all(|byte| byte.is_ascii_graphic()) {
            return Err(ConfigError::Invalid {
                var: "LOGNARA_RELAY_TOKEN",
                value: "[redacted]".into(),
                expected: "an ASCII token without whitespace or control characters",
            });
        }

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

        let config = Self {
            service: required("LOGNARA_SERVICE")?,
            server: required("LOGNARA_SERVER")?,
            backend: required("LOGNARA_BACKEND")?,
            environment: get("LOGNARA_ENVIRONMENT"),
            service_instance: get("LOGNARA_SERVICE_INSTANCE").or_else(|| get("HOSTNAME")),
            batch_size,
            flush_interval: Duration::from_millis(flush_interval_ms),
            max_buffer,
            max_buffer_bytes: positive(&get, "LOGNARA_MAX_BUFFER_BYTES", 64usize << 20)?,
            max_batch_bytes: positive(&get, "LOGNARA_MAX_BATCH_BYTES", 8usize << 20)?,
            max_record_bytes: positive(&get, "LOGNARA_MAX_RECORD_BYTES", 2usize << 20)?,
            spool_dir: get("LOGNARA_SPOOL_DIR")
                .unwrap_or_else(|| "/var/lib/lognara-agent/spool".into())
                .into(),
            spool_max_bytes: positive(&get, "LOGNARA_SPOOL_MAX_MB", 1024u64)?
                .checked_mul(1 << 20)
                .ok_or(ConfigError::Invalid {
                    var: "LOGNARA_SPOOL_MAX_MB",
                    value: "[overflow]".into(),
                    expected: "a byte quota without overflow",
                })?,
            listen_addr: parse(
                &get,
                "LOGNARA_LISTEN_ADDR",
                DEFAULT_LISTEN_ADDR,
                "an address like 127.0.0.1:7400",
            )?,
            relay_url: parse(&get, "LOGNARA_RELAY_URL", DEFAULT_RELAY_URL, "a URL")?,
            relay_token,
        };
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.flush_interval.is_zero()
            || self.batch_size == 0
            || self.max_buffer < self.batch_size
            || self.spool_max_bytes == 0
            || self.max_record_bytes == 0
            || self.max_record_bytes > 2 << 20
            || self.max_batch_bytes == 0
            || self.max_batch_bytes > 8 << 20
            || self.max_record_bytes >= self.max_batch_bytes
            || self.max_buffer_bytes > u32::MAX as usize
            || self.pipeline_bytes()
                < self
                    .max_batch_bytes
                    .saturating_mul(2)
                    .saturating_add(4 << 20)
                    .saturating_add(8192)
        {
            return Err(ConfigError::Invalid {
                var: "LOGNARA_MAX_BUFFER_BYTES",
                value: self.max_buffer_bytes.to_string(),
                expected: "positive limits: record <= 2 MiB, record < batch <= 8 MiB, memory covering two pipelines, and max buffer >= batch size",
            });
        }
        let metadata = self.metadata_bytes();
        if metadata
            .saturating_add(self.max_record_bytes)
            .saturating_add(128 << 10)
            > self.max_batch_bytes
            || metadata > self.max_buffer_bytes / 16
        {
            return Err(ConfigError::Invalid {
                var: "LOGNARA_MAX_BATCH_BYTES",
                value: self.max_batch_bytes.to_string(),
                expected: "room for maximum record and source metadata",
            });
        }
        Ok(())
    }

    pub fn metadata_bytes(&self) -> usize {
        self.service
            .capacity()
            .saturating_add(self.server.capacity())
            .saturating_add(self.backend.capacity())
            .saturating_add(self.environment.as_ref().map_or(0, String::capacity))
            .saturating_add(self.service_instance.as_ref().map_or(0, String::capacity))
            .saturating_add(1024)
    }
    pub fn pipeline_bytes(&self) -> usize {
        self.max_buffer_bytes
            .saturating_sub(INDEX_BYTES)
            .saturating_sub(self.metadata_bytes().saturating_mul(4))
            / 2
    }
    pub fn input_record_limit(&self) -> usize {
        self.pipeline_bytes()
            .saturating_sub((4 << 20) + 2 * self.max_batch_bytes + 2 * self.metadata_bytes())
            / 1024
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
    fn rejects_invalid_token_without_exposing_it() {
        for token in [
            "relay secret",
            " secret",
            "secret ",
            "secret\t",
            "secret\r\n",
            "secret\0",
            "secret\u{7f}",
            "секрет",
        ] {
            let err = with_required(&[("LOGNARA_RELAY_TOKEN", token)]).unwrap_err();
            assert!(matches!(
                &err,
                ConfigError::Invalid {
                    var: "LOGNARA_RELAY_TOKEN",
                    ..
                }
            ));
            assert!(!err.to_string().contains(token));
            assert!(!format!("{err:?}").contains(token));
            assert!(err.to_string().contains("LOGNARA_RELAY_TOKEN"));
        }
    }

    #[test]
    fn debug_redacts_tokens() {
        let config = with_required(&[("LOGNARA_RELAY_TOKEN", "unique-relay-secret")]).unwrap();
        let debug = format!("{config:?}");
        assert!(!debug.contains("unique-relay-secret"));
        assert!(debug.contains("[redacted]"));
    }

    #[test]
    fn requires_relay_token() {
        assert_eq!(
            with_required(&[("LOGNARA_RELAY_TOKEN", "")]),
            Err(ConfigError::Missing("LOGNARA_RELAY_TOKEN"))
        );
    }

    const REQUIRED: [(&str, &str); 4] = [
        ("LOGNARA_SERVICE", "api"),
        ("LOGNARA_SERVER", "eu-prod-01"),
        ("LOGNARA_BACKEND", "barkcloud"),
        ("LOGNARA_RELAY_TOKEN", "relay-secret"),
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

        assert_eq!(config.service, "api");
        assert_eq!(config.server, "eu-prod-01");
        assert_eq!(config.backend, "barkcloud");
        assert_eq!(config.relay_token, "relay-secret");
        assert_eq!(config.environment, None);
        assert_eq!(config.service_instance, None);
        assert_eq!(config.batch_size, 1000);
        assert_eq!(config.flush_interval, Duration::from_millis(5000));
        assert_eq!(config.max_buffer, 100_000);
        assert_eq!(
            config.listen_addr,
            "127.0.0.1:7400".parse::<SocketAddr>().unwrap()
        );
        assert_eq!(
            config.relay_url.as_str(),
            "http://lognara-relay:7401/v1/batches"
        );
    }

    #[test]
    fn reads_all_variables() {
        let config = with_required(&[
            ("LOGNARA_ENVIRONMENT", "production"),
            ("LOGNARA_SERVICE_INSTANCE", "api-2"),
            ("LOGNARA_BATCH_SIZE", "50"),
            ("LOGNARA_FLUSH_INTERVAL_MS", "250"),
            ("LOGNARA_MAX_BUFFER", "500"),
            ("LOGNARA_LISTEN_ADDR", "0.0.0.0:9000"),
            ("LOGNARA_RELAY_URL", "http://127.0.0.1:9001/batches"),
            ("LOGNARA_RELAY_TOKEN", "new-relay-secret"),
        ])
        .unwrap();

        assert_eq!(config.environment.as_deref(), Some("production"));
        assert_eq!(config.service_instance.as_deref(), Some("api-2"));
        assert_eq!(config.batch_size, 50);
        assert_eq!(config.flush_interval, Duration::from_millis(250));
        assert_eq!(config.max_buffer, 500);
        assert_eq!(
            config.listen_addr,
            "0.0.0.0:9000".parse::<SocketAddr>().unwrap()
        );
        assert_eq!(config.relay_url.as_str(), "http://127.0.0.1:9001/batches");
        assert_eq!(config.relay_token, "new-relay-secret");
    }

    #[test]
    fn requires_agent_variables() {
        for (missing, _) in REQUIRED {
            let vars: Vec<_> = REQUIRED
                .into_iter()
                .filter(|(name, _)| *name != missing)
                .collect();

            assert_eq!(config(&vars), Err(ConfigError::Missing(missing)));
        }
        assert_eq!(
            ConfigError::Missing("LOGNARA_SERVICE").to_string(),
            "LOGNARA_SERVICE is required"
        );
    }

    #[test]
    fn treats_empty_value_as_missing() {
        assert_eq!(
            with_required(&[("LOGNARA_SERVER", "")]),
            Err(ConfigError::Missing("LOGNARA_SERVER"))
        );
        assert_eq!(
            with_required(&[("LOGNARA_ENVIRONMENT", "")])
                .unwrap()
                .environment,
            None
        );
    }

    #[test]
    fn rejects_non_positive_numbers() {
        for value in ["abc", "0", "-5"] {
            assert_eq!(
                with_required(&[("LOGNARA_BATCH_SIZE", value)]),
                Err(ConfigError::Invalid {
                    var: "LOGNARA_BATCH_SIZE",
                    value: value.to_string(),
                    expected: "a positive integer",
                })
            );
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

    #[test]
    fn service_instance_falls_back_to_hostname() {
        let config = with_required(&[("HOSTNAME", "a1b2c3")]).unwrap();
        assert_eq!(config.service_instance.as_deref(), Some("a1b2c3"));

        let config = with_required(&[
            ("HOSTNAME", "a1b2c3"),
            ("LOGNARA_SERVICE_INSTANCE", "api-2"),
        ])
        .unwrap();
        assert_eq!(config.service_instance.as_deref(), Some("api-2"));
    }
    #[test]
    fn validates_resource_partitions_and_checked_disk_quota() {
        for (var, value) in [
            ("LOGNARA_MAX_RECORD_BYTES", "2097153"),
            ("LOGNARA_MAX_BATCH_BYTES", "8388609"),
            ("LOGNARA_MAX_BUFFER_BYTES", "1048576"),
            ("LOGNARA_SPOOL_MAX_MB", "18446744073709551615"),
        ] {
            assert!(with_required(&[(var, value)]).is_err(), "{var}");
        }
        let c = with_required(&[]).unwrap();
        assert_eq!(c.max_buffer_bytes, 64 << 20);
        assert_eq!(c.max_batch_bytes, 8 << 20);
        assert_eq!(c.max_record_bytes, 2 << 20);
        assert_eq!(c.spool_max_bytes, 1 << 30);
        assert!(
            2 * c.pipeline_bytes() + INDEX_BYTES + 4 * c.metadata_bytes() <= c.max_buffer_bytes
        );
    }
}
