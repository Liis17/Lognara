//! Переменные `LOGNARA_*` и JSON-файл хоста (`LOGNARA_HWAGENT_CONFIG`).

use std::fmt;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

use lognara_agent::config::Config as AgentConfig;
use lognara_agent::sources::{Source, Sources};
use reqwest::Url;
use serde::Deserialize;

use crate::journald::JournalSettings;
use crate::metrics::MetricsSettings;
use crate::processes::ProcessSettings;

pub const SERVICE: &str = "host";
const DEFAULT_CONFIG: &str = "/etc/lognara/hwagent.json";
const DEFAULT_SPOOL: &str = "/var/lib/lognara-hwagent/spool";
const DEFAULT_STATE: &str = "/var/lib/lognara-hwagent/state";
const MAX_CONFIG_BYTES: u64 = 256 << 10;

/// Содержимое JSON-файла хоста; неизвестные поля - ошибка запуска.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostFile {
    /// Файлы логов в формате источников агента.
    #[serde(default)]
    pub sources: Vec<Source>,
    #[serde(default)]
    pub metrics: MetricsSettings,
    #[serde(default)]
    pub processes: ProcessSettings,
    #[serde(default)]
    pub journald: JournalSettings,
}

impl HostFile {
    pub fn parse(bytes: &[u8]) -> io::Result<Self> {
        let host: Self = serde_json::from_slice(bytes)
            .map_err(|error| invalid(format!("host config: {error}")))?;
        host.metrics.validate().map_err(invalid)?;
        host.processes.validate().map_err(invalid)?;
        host.journald.validate().map_err(invalid)?;
        Ok(host)
    }
}

#[derive(Clone)]
pub struct Config {
    pub agent: AgentConfig,
    pub host: HostFile,
    /// `.../v1/batches` core; heartbeat идёт на соседний `.../v1/heartbeat`.
    pub core_url: Url,
    pub token: String,
    pub heartbeat_interval: Duration,
    /// Корень `/proc` и `/sys`; в тестах - каталог с фикстурами.
    pub root: PathBuf,
    /// Состояние сборщиков вне spool (известные исполняемые файлы, cursor journald).
    pub state_dir: PathBuf,
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("agent", &self.agent)
            .field("host", &self.host)
            .field("core_url", &self.core_url)
            .field("heartbeat_interval", &self.heartbeat_interval)
            .field("root", &self.root)
            .field("state_dir", &self.state_dir)
            .field("token", &"[redacted]")
            .finish()
    }
}

impl Config {
    pub fn from_env() -> io::Result<Self> {
        let path = std::env::var("LOGNARA_HWAGENT_CONFIG")
            .ok()
            .filter(|path| !path.is_empty())
            .unwrap_or_else(|| DEFAULT_CONFIG.into());
        let host = HostFile::parse(&read_limited(Path::new(&path))?)?;
        Self::from_lookup(|name| std::env::var(name).ok(), host)
    }

    /// Параметры очереди и отправки берутся у агента; источник событий фиксирован:
    /// service и backend `host`, server - имя хоста.
    pub fn from_lookup(
        lookup: impl Fn(&str) -> Option<String>,
        host: HostFile,
    ) -> io::Result<Self> {
        let get = |name: &str| lookup(name).filter(|value| !value.is_empty());
        let core_url =
            get("LOGNARA_CORE_URL").ok_or_else(|| invalid("LOGNARA_CORE_URL is required"))?;
        let parsed: Url = core_url
            .parse()
            .map_err(|_| invalid("LOGNARA_CORE_URL must be an http(s) URL"))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(invalid("LOGNARA_CORE_URL must be an http(s) URL"));
        }
        let token =
            get("LOGNARA_HOST_TOKEN").ok_or_else(|| invalid("LOGNARA_HOST_TOKEN is required"))?;
        let server = match get("LOGNARA_SERVER") {
            Some(server) => server,
            None => hostname()?,
        };
        let spool = get("LOGNARA_SPOOL_DIR").unwrap_or_else(|| DEFAULT_SPOOL.into());
        let mut agent = AgentConfig::from_lookup(|name| match name {
            "LOGNARA_SERVICE" | "LOGNARA_BACKEND" => Some(SERVICE.into()),
            "LOGNARA_SERVER" => Some(server.clone()),
            "LOGNARA_RELAY_URL" => Some(core_url.clone()),
            "LOGNARA_RELAY_TOKEN" => Some(token.clone()),
            "LOGNARA_SPOOL_DIR" => Some(spool.clone()),
            // Нормализация в процессе требует запаса памяти: пачка меньше, резерв больше.
            "LOGNARA_MAX_BUFFER_BYTES" => Some((128usize << 20).to_string()),
            "LOGNARA_MAX_BATCH_BYTES" => Some((4usize << 20).to_string()),
            "LOGNARA_MAX_RECORD_BYTES" => Some((512usize << 10).to_string()),
            "LOGNARA_BATCH_SIZE" => get(name).or_else(|| Some("500".into())),
            "LOGNARA_SERVICE_INSTANCE" | "HOSTNAME" | "LOGNARA_LISTEN_ADDR" => None,
            "LOGNARA_ENVIRONMENT"
            | "LOGNARA_FLUSH_INTERVAL_MS"
            | "LOGNARA_MAX_BUFFER"
            | "LOGNARA_SPOOL_MAX_MB"
            | "LOGNARA_HEARTBEAT_INTERVAL_MS" => get(name),
            _ => None,
        })
        .map_err(|error| invalid(error.to_string()))?;
        agent.sources = Sources {
            sources: host.sources.clone(),
        };
        agent.sources.validate(false)?;
        agent
            .validate()
            .map_err(|error| invalid(error.to_string()))?;
        Ok(Self {
            heartbeat_interval: agent.heartbeat_interval,
            agent,
            host,
            core_url: parsed,
            token,
            root: PathBuf::from("/"),
            state_dir: get("LOGNARA_STATE_DIR")
                .unwrap_or_else(|| DEFAULT_STATE.into())
                .into(),
        })
    }
}

fn read_limited(path: &Path) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|error| io::Error::new(error.kind(), format!("{}: {error}", path.display())))?
        .take(MAX_CONFIG_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_CONFIG_BYTES {
        return Err(invalid("host config exceeds 256 KiB"));
    }
    Ok(bytes)
}

fn hostname() -> io::Result<String> {
    let name = std::fs::read_to_string("/proc/sys/kernel/hostname")?;
    let name = name.trim();
    if name.is_empty() {
        return Err(invalid("hostname is empty; set LOGNARA_SERVER"));
    }
    Ok(name.into())
}

fn invalid(message: impl fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup(vars: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        |name| {
            vars.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.to_string())
        }
    }

    const BASE: &[(&str, &str)] = &[
        ("LOGNARA_CORE_URL", "https://core.example.com/v1/batches"),
        ("LOGNARA_HOST_TOKEN", "lnh_secret"),
        ("LOGNARA_SERVER", "mail"),
        ("LOGNARA_SERVICE", "ignored"),
        ("HOSTNAME", "container"),
    ];

    #[test]
    fn maps_host_identity_and_hides_token() {
        let config = Config::from_lookup(lookup(BASE), HostFile::default()).unwrap();
        assert_eq!(config.agent.service, "host");
        assert_eq!(config.agent.backend, "host");
        assert_eq!(config.agent.server, "mail");
        assert_eq!(config.agent.service_instance, None);
        assert_eq!(
            config.agent.relay_url.as_str(),
            "https://core.example.com/v1/batches"
        );
        assert_eq!(config.agent.max_record_bytes, 512 << 10);
        assert!(!format!("{config:?}").contains("lnh_secret"));
    }

    #[test]
    fn requires_core_url_and_token() {
        for missing in ["LOGNARA_CORE_URL", "LOGNARA_HOST_TOKEN"] {
            let error = Config::from_lookup(
                |name| {
                    if name == missing {
                        None
                    } else {
                        lookup(BASE)(name)
                    }
                },
                HostFile::default(),
            )
            .unwrap_err();
            assert!(error.to_string().contains(missing));
        }
    }

    #[test]
    fn host_file_reuses_agent_sources_and_rejects_unknown_fields() {
        let host = HostFile::parse(
            br#"{"sources":[{"id":"auth","type":"file","include":["/var/log/auth.log*"],"start_at":"beginning"}]}"#,
        )
        .unwrap();
        let config = Config::from_lookup(lookup(BASE), host).unwrap();
        assert_eq!(config.agent.sources.sources[0].id, "auth");
        assert!(HostFile::parse(br#"{"unknown":1}"#).is_err());
        let streams = HostFile::parse(br#"{"sources":[{"id":"out","type":"stdout"}]}"#).unwrap();
        assert!(Config::from_lookup(lookup(BASE), streams).is_err());
    }

    #[test]
    fn shipped_examples_are_valid() {
        for file in ["examples/hwagent.json", "examples/hwagent-rsyslog.json"] {
            let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(file);
            let host = HostFile::parse(&std::fs::read(path).unwrap()).unwrap();
            Config::from_lookup(lookup(BASE), host).unwrap();
        }
    }
}
