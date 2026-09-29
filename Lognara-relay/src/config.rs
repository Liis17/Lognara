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
const DEFAULT_LISTEN_ADDR: &str = "0.0.0.0:7401";
const DEFAULT_SPOOL_DIR: &str = "/var/lib/lognara-relay/spool";
const DEFAULT_SPOOL_MAX_MB: u64 = 1024;

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub core_url: Url,
    pub core_token: String,
    /// Как часто отправлять накопленное в core.
    pub flush_interval: Duration,
    /// Сколько событий накопить, чтобы отправить их раньше интервала.
    pub batch_size: usize,
    /// Сколько событий держать в памяти; сверх лимита агенты получают 503.
    pub max_buffer: usize,
    pub listen_addr: SocketAddr,
    /// Каталог для пачек, которые не удалось отправить в core.
    pub spool_dir: PathBuf,
    /// Лимит spool; при переполнении удаляются самые старые пачки.
    pub spool_max_bytes: u64,
    /// Должны быть не больше соответствующих лимитов core.
    pub core_max_body_bytes: usize,
    pub core_max_decoded_bytes: usize,
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

        Ok(Self {
            core_url,
            core_token: required("LOGNARA_CORE_TOKEN")?,
            flush_interval: Duration::from_millis(flush_interval_ms),
            batch_size,
            max_buffer,
            listen_addr: parse(
                &get,
                "LOGNARA_LISTEN_ADDR",
                DEFAULT_LISTEN_ADDR,
                "an address like 0.0.0.0:7401",
            )?,
            spool_dir: get("LOGNARA_SPOOL_DIR")
                .unwrap_or_else(|| DEFAULT_SPOOL_DIR.to_owned())
                .into(),
            spool_max_bytes: spool_max_mb.saturating_mul(1024 * 1024),
            core_max_body_bytes: positive(&get, "LOGNARA_CORE_MAX_BODY_BYTES", 64 << 20)?,
            core_max_decoded_bytes: positive(&get, "LOGNARA_CORE_MAX_DECODED_BYTES", 256 << 20)?,
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
            "0.0.0.0:7401".parse::<SocketAddr>().unwrap()
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
    fn requires_core_variables() {
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
