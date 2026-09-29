//! Параметры запуска из переменных окружения `LOGNARA_*`.

use std::fmt;
use std::net::SocketAddr;
use std::str::FromStr;
use std::time::Duration;

use reqwest::Url;

const DEFAULT_BATCH_SIZE: usize = 1000;
const DEFAULT_FLUSH_INTERVAL_MS: u64 = 5000;
const DEFAULT_MAX_BUFFER: usize = 100_000;
const DEFAULT_LISTEN_ADDR: &str = "127.0.0.1:7400";
const DEFAULT_RELAY_URL: &str = "http://lognara-relay:7401/v1/batches";

#[derive(Debug, Clone, PartialEq)]
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
    /// Сколько записей держать в памяти; при переполнении вытесняются самые старые.
    pub max_buffer: usize,
    pub listen_addr: SocketAddr,
    pub relay_url: Url,
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

        Ok(Self {
            service: required("LOGNARA_SERVICE")?,
            server: required("LOGNARA_SERVER")?,
            backend: required("LOGNARA_BACKEND")?,
            environment: get("LOGNARA_ENVIRONMENT"),
            service_instance: get("LOGNARA_SERVICE_INSTANCE").or_else(|| get("HOSTNAME")),
            batch_size,
            flush_interval: Duration::from_millis(flush_interval_ms),
            max_buffer,
            listen_addr: parse(
                &get,
                "LOGNARA_LISTEN_ADDR",
                DEFAULT_LISTEN_ADDR,
                "an address like 127.0.0.1:7400",
            )?,
            relay_url: parse(&get, "LOGNARA_RELAY_URL", DEFAULT_RELAY_URL, "a URL")?,
        })
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

    const REQUIRED: [(&str, &str); 3] = [
        ("LOGNARA_SERVICE", "api"),
        ("LOGNARA_SERVER", "eu-prod-01"),
        ("LOGNARA_BACKEND", "barkcloud"),
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
    }

    #[test]
    fn requires_identity_variables() {
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
}
