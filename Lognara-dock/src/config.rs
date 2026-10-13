//! Параметры запуска из переменных окружения `LOGNARA_DOCK_*`.

use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use url::Url;

const DEFAULT_LISTEN_ADDR: &str = "127.0.0.1:7403";
const DEFAULT_HTPASSWD_FILE: &str = "/etc/lognara-dock/auth/htpasswd";
const DEFAULT_DATA_DIR: &str = "/var/lib/lognara-dock";
const DEFAULT_WEB_DIR: &str = "/usr/share/lognara-dock/web";

#[derive(Clone, PartialEq)]
pub struct Config {
    /// Внешний адрес за nginx, например `https://dock.example.com`.
    pub public_url: Url,
    /// `scheme://host[:port]` из `public_url`; с ним сравнивается заголовок Origin.
    pub public_origin: String,
    /// RP ID для passkey: хост `public_url` или его родительский домен.
    pub rp_id: String,
    /// Базовый адрес core, например `http://lognara-core:7402`.
    pub core_url: Url,
    /// Query-токен core, в браузер не передаётся.
    pub core_token: String,
    /// Admin-токен core для определений метрик; без него admin-маршруты отключены.
    pub core_admin_token: Option<String>,
    pub listen_addr: SocketAddr,
    pub htpasswd_file: PathBuf,
    pub data_dir: PathBuf,
    pub web_dir: PathBuf,
    pub session_ttl: Duration,
    pub session_idle: Duration,
    pub bcrypt_cost: u32,
    pub password_min_length: usize,
    pub live_poll: Duration,
    pub live_lookback: Duration,
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("public_url", &self.public_url.as_str())
            .field("rp_id", &self.rp_id)
            .field("core_url", &self.core_url.as_str())
            .field("core_token", &"[redacted]")
            .field(
                "core_admin_token",
                &self.core_admin_token.as_ref().map(|_| "[redacted]"),
            )
            .field("listen_addr", &self.listen_addr)
            .field("htpasswd_file", &self.htpasswd_file)
            .field("data_dir", &self.data_dir)
            .field("web_dir", &self.web_dir)
            .field("session_ttl", &self.session_ttl)
            .field("session_idle", &self.session_idle)
            .field("bcrypt_cost", &self.bcrypt_cost)
            .field("password_min_length", &self.password_min_length)
            .field("live_poll", &self.live_poll)
            .field("live_lookback", &self.live_lookback)
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

        let public_url = required("LOGNARA_DOCK_PUBLIC_URL")?;
        let public_url = match public_url.parse::<Url>() {
            Ok(url) if is_public_url(&url) => url,
            _ => {
                return Err(ConfigError::Invalid {
                    var: "LOGNARA_DOCK_PUBLIC_URL",
                    value: public_url,
                    expected: "an https URL without path, or http://localhost for development",
                });
            }
        };
        let host = public_url.host_str().unwrap_or_default().to_owned();
        let rp_id = get("LOGNARA_DOCK_RP_ID").unwrap_or_else(|| host.clone());
        if !(host == rp_id || host.ends_with(&format!(".{rp_id}"))) {
            return Err(ConfigError::Invalid {
                var: "LOGNARA_DOCK_RP_ID",
                value: rp_id,
                expected: "the host of LOGNARA_DOCK_PUBLIC_URL or its parent domain",
            });
        }

        let core_url = required("LOGNARA_DOCK_CORE_URL")?;
        let core_url = match core_url.parse::<Url>() {
            Ok(url) if matches!(url.scheme(), "http" | "https") => url,
            _ => {
                return Err(ConfigError::Invalid {
                    var: "LOGNARA_DOCK_CORE_URL",
                    value: core_url,
                    expected: "an http or https URL",
                });
            }
        };
        let core_token = required("LOGNARA_DOCK_CORE_TOKEN")?;
        check_token("LOGNARA_DOCK_CORE_TOKEN", &core_token)?;
        let core_admin_token = get("LOGNARA_DOCK_CORE_ADMIN_TOKEN");
        if let Some(token) = &core_admin_token {
            check_token("LOGNARA_DOCK_CORE_ADMIN_TOKEN", token)?;
        }

        let bcrypt_cost = positive(&get, "LOGNARA_DOCK_BCRYPT_COST", 12u32)?;
        if !(10..=14).contains(&bcrypt_cost) {
            return Err(ConfigError::Invalid {
                var: "LOGNARA_DOCK_BCRYPT_COST",
                value: bcrypt_cost.to_string(),
                expected: "a cost from 10 to 14",
            });
        }

        Ok(Self {
            public_origin: public_url.origin().ascii_serialization(),
            public_url,
            rp_id,
            core_url,
            core_token,
            core_admin_token,
            listen_addr: parse(
                &get,
                "LOGNARA_DOCK_LISTEN_ADDR",
                DEFAULT_LISTEN_ADDR,
                "an address like 127.0.0.1:7403",
            )?,
            htpasswd_file: get("LOGNARA_DOCK_HTPASSWD_FILE")
                .unwrap_or_else(|| DEFAULT_HTPASSWD_FILE.to_owned())
                .into(),
            data_dir: get("LOGNARA_DOCK_DATA_DIR")
                .unwrap_or_else(|| DEFAULT_DATA_DIR.to_owned())
                .into(),
            web_dir: get("LOGNARA_DOCK_WEB_DIR")
                .unwrap_or_else(|| DEFAULT_WEB_DIR.to_owned())
                .into(),
            session_ttl: Duration::from_secs(positive(
                &get,
                "LOGNARA_DOCK_SESSION_TTL_SECONDS",
                43_200,
            )?),
            session_idle: Duration::from_secs(positive(
                &get,
                "LOGNARA_DOCK_SESSION_IDLE_SECONDS",
                3_600,
            )?),
            bcrypt_cost,
            password_min_length: positive(&get, "LOGNARA_DOCK_PASSWORD_MIN_LENGTH", 12)?,
            live_poll: Duration::from_millis(positive(&get, "LOGNARA_DOCK_LIVE_POLL_MS", 1_000)?),
            live_lookback: Duration::from_secs(positive(
                &get,
                "LOGNARA_DOCK_LIVE_LOOKBACK_SECONDS",
                300,
            )?),
        })
    }
}

/// Secure-cookie и WebAuthn требуют https; без TLS они работают только на localhost.
fn is_public_url(url: &Url) -> bool {
    let local = matches!(url.host_str(), Some("localhost" | "127.0.0.1"));
    let scheme = url.scheme() == "https" || (url.scheme() == "http" && local);
    scheme && url.path() == "/" && url.query().is_none() && url.fragment().is_none()
}

/// Токен уходит в заголовок Authorization: только ASCII без пробелов и управляющих символов.
fn check_token(var: &'static str, token: &str) -> Result<(), ConfigError> {
    if token.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Ok(());
    }
    Err(ConfigError::Invalid {
        var,
        value: "[redacted]".into(),
        expected: "an ASCII token without whitespace or control characters",
    })
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
        ("LOGNARA_DOCK_PUBLIC_URL", "https://dock.example.com"),
        ("LOGNARA_DOCK_CORE_URL", "http://lognara-core:7402"),
        ("LOGNARA_DOCK_CORE_TOKEN", "query-secret"),
    ];

    fn with_required(extra: &[(&'static str, &'static str)]) -> Result<Config, ConfigError> {
        let vars = [&REQUIRED[..], extra].concat();
        Config::from_lookup(|name| {
            vars.iter()
                .rev()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.to_string())
        })
    }

    #[test]
    fn applies_defaults() {
        let config = with_required(&[]).unwrap();
        assert_eq!(config.public_origin, "https://dock.example.com");
        assert_eq!(config.rp_id, "dock.example.com");
        assert_eq!(config.listen_addr.to_string(), "127.0.0.1:7403");
        assert_eq!(
            config.htpasswd_file,
            PathBuf::from("/etc/lognara-dock/auth/htpasswd")
        );
        assert_eq!(config.data_dir, PathBuf::from("/var/lib/lognara-dock"));
        assert_eq!(config.session_ttl, Duration::from_secs(43_200));
        assert_eq!(config.session_idle, Duration::from_secs(3_600));
        assert_eq!(config.bcrypt_cost, 12);
        assert_eq!(config.password_min_length, 12);
        assert_eq!(config.live_poll, Duration::from_secs(1));
        assert_eq!(config.live_lookback, Duration::from_secs(300));
    }

    #[test]
    fn names_missing_variable() {
        for (var, _) in REQUIRED {
            let err = with_required(&[(var, "")]).unwrap_err();
            assert_eq!(err, ConfigError::Missing(var));
            assert!(err.to_string().contains(var));
        }
    }

    #[test]
    fn validates_public_url_and_rp_id() {
        for url in [
            "http://dock.example.com",
            "https://dock.example.com/logs",
            "https://dock.example.com/?a=1",
            "ftp://dock.example.com",
        ] {
            let err = with_required(&[("LOGNARA_DOCK_PUBLIC_URL", url)]).unwrap_err();
            assert!(err.to_string().contains("LOGNARA_DOCK_PUBLIC_URL"), "{url}");
        }
        let local = with_required(&[("LOGNARA_DOCK_PUBLIC_URL", "http://localhost:7403")]).unwrap();
        assert_eq!(local.public_origin, "http://localhost:7403");

        let parent = with_required(&[("LOGNARA_DOCK_RP_ID", "example.com")]).unwrap();
        assert_eq!(parent.rp_id, "example.com");
        for rp_id in ["other.com", "ample.com", "sub.dock.example.com"] {
            assert!(
                with_required(&[("LOGNARA_DOCK_RP_ID", rp_id)]).is_err(),
                "{rp_id}"
            );
        }
    }

    #[test]
    fn validates_bcrypt_cost() {
        for cost in ["9", "15", "0", "x"] {
            assert!(with_required(&[("LOGNARA_DOCK_BCRYPT_COST", cost)]).is_err());
        }
        assert_eq!(
            with_required(&[("LOGNARA_DOCK_BCRYPT_COST", "10")])
                .unwrap()
                .bcrypt_cost,
            10
        );
    }

    #[test]
    fn rejects_invalid_token_without_exposing_it() {
        for token in ["query secret", "секрет", "secret\n"] {
            let err = with_required(&[("LOGNARA_DOCK_CORE_TOKEN", token)]).unwrap_err();
            assert!(err.to_string().contains("LOGNARA_DOCK_CORE_TOKEN"));
            assert!(!err.to_string().contains(token));
        }
    }

    #[test]
    fn debug_redacts_token() {
        let debug = format!("{:?}", with_required(&[]).unwrap());
        assert!(!debug.contains("query-secret"));
        assert!(debug.contains("[redacted]"));
    }

    #[test]
    fn admin_token_is_optional_and_empty_means_unset() {
        assert_eq!(with_required(&[]).unwrap().core_admin_token, None);
        let empty = with_required(&[("LOGNARA_DOCK_CORE_ADMIN_TOKEN", "")]).unwrap();
        assert_eq!(empty.core_admin_token, None);
        let set = with_required(&[("LOGNARA_DOCK_CORE_ADMIN_TOKEN", "admin-secret")]).unwrap();
        assert_eq!(set.core_admin_token.as_deref(), Some("admin-secret"));
    }

    #[test]
    fn rejects_invalid_admin_token_without_exposing_it() {
        for token in ["admin secret", "секрет", "secret\n"] {
            let err = with_required(&[("LOGNARA_DOCK_CORE_ADMIN_TOKEN", token)]).unwrap_err();
            assert!(err.to_string().contains("LOGNARA_DOCK_CORE_ADMIN_TOKEN"));
            assert!(!err.to_string().contains(token));
        }
    }

    #[test]
    fn debug_redacts_admin_token() {
        let config = with_required(&[("LOGNARA_DOCK_CORE_ADMIN_TOKEN", "admin-secret")]).unwrap();
        let debug = format!("{config:?}");
        assert!(!debug.contains("admin-secret"));
        assert!(debug.contains("core_admin_token: Some(\"[redacted]\")"));
    }
}
