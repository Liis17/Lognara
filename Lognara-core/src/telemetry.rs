//! Heartbeat relay и агентов. Узлы присылают возраст отметок, а не время своих часов:
//! core переводит его в абсолютное время по своим часам, поэтому расхождение часов узлов
//! не искажает свежесть. Агента определяет хеш токена, принадлежащий relay запроса.
use serde::{Deserialize, Serialize};

use crate::{model::format_timestamp, wire::Source};

pub const MAX_BODY: usize = 4 << 20;
const MAX_AGENTS: usize = 10_000;
const MAX_TEXT: usize = 256;
/// Возраст больше суток считается ошибкой узла.
const MAX_AGE_MS: u64 = 86_400_000;

#[derive(Deserialize)]
pub struct RelayHeartbeat {
    pub relay: RelayReport,
    #[serde(default)]
    pub agents: Vec<AgentHeartbeat>,
}

#[derive(Deserialize)]
pub struct AgentHeartbeat {
    pub token_sha256: String,
    /// Сколько миллисекунд назад relay получил heartbeat агента.
    pub heartbeat_ms_ago: u64,
    pub telemetry: AgentReport,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Spool {
    pub used_bytes: u64,
    pub limit_bytes: u64,
    pub records: u64,
}

#[derive(Deserialize)]
pub struct Delivery {
    pub last_delivery_ms_ago: Option<u64>,
    pub last_failure_ms_ago: Option<u64>,
    /// Неудачные отправки с запуска узла.
    pub delivery_failures: u64,
}

#[derive(Deserialize)]
pub struct RelayReport {
    pub version: String,
    pub spool: Spool,
    pub last_ingest_ms_ago: Option<u64>,
    #[serde(flatten)]
    pub delivery: Delivery,
    /// События, принятые от агентов, в среднем за последний интервал heartbeat.
    pub events_per_second: f64,
}

#[derive(Deserialize)]
pub struct AgentReport {
    pub version: String,
    pub source: Source,
    pub spool: Spool,
    pub last_record_ms_ago: Option<u64>,
    #[serde(flatten)]
    pub delivery: Delivery,
}

/// Сохранённое состояние relay: время уже в часах core.
#[derive(Serialize)]
pub struct RelayState {
    pub received_at: String,
    pub version: String,
    pub spool: Spool,
    pub last_ingest_at: Option<String>,
    pub last_delivery_at: Option<String>,
    pub last_failure_at: Option<String>,
    pub delivery_failures: u64,
    pub events_per_second: f64,
}

#[derive(Serialize)]
pub struct AgentState {
    pub received_at: String,
    pub version: String,
    pub source: Source,
    pub spool: Spool,
    pub last_record_at: Option<String>,
    pub last_delivery_at: Option<String>,
    pub last_failure_at: Option<String>,
    pub delivery_failures: u64,
}

impl RelayHeartbeat {
    pub fn valid(&self) -> bool {
        let relay = &self.relay;
        self.agents.len() <= MAX_AGENTS
            && version(&relay.version)
            && relay.events_per_second.is_finite()
            && relay.events_per_second >= 0.0
            && age(relay.last_ingest_ms_ago)
            && relay.delivery.valid()
            && self.agents.iter().all(AgentHeartbeat::valid)
    }
}

impl AgentHeartbeat {
    fn valid(&self) -> bool {
        let report = &self.telemetry;
        let source = &report.source;
        self.token_sha256.len() == 64
            && self.heartbeat_ms_ago <= MAX_AGE_MS
            && version(&report.version)
            && [&source.server, &source.backend, &source.service]
                .into_iter()
                .chain(source.environment.as_ref())
                .chain(source.service_instance.as_ref())
                .all(|value| text(value))
            && age(report.last_record_ms_ago)
            && report.delivery.valid()
    }

    pub fn hash(&self) -> Option<[u8; 32]> {
        hex::decode(&self.token_sha256).ok()?.try_into().ok()
    }
}

impl Delivery {
    fn valid(&self) -> bool {
        age(self.last_delivery_ms_ago) && age(self.last_failure_ms_ago)
    }
}

impl RelayReport {
    pub fn state(self, now: i64) -> RelayState {
        RelayState {
            received_at: format_timestamp(now),
            version: self.version,
            spool: self.spool,
            last_ingest_at: at(now, self.last_ingest_ms_ago),
            last_delivery_at: at(now, self.delivery.last_delivery_ms_ago),
            last_failure_at: at(now, self.delivery.last_failure_ms_ago),
            delivery_failures: self.delivery.delivery_failures,
            events_per_second: self.events_per_second,
        }
    }
}

impl AgentReport {
    /// `now` — момент, когда heartbeat получил relay, в часах core.
    pub fn state(self, now: i64) -> AgentState {
        AgentState {
            received_at: format_timestamp(now),
            version: self.version,
            source: self.source,
            spool: self.spool,
            last_record_at: at(now, self.last_record_ms_ago),
            last_delivery_at: at(now, self.delivery.last_delivery_ms_ago),
            last_failure_at: at(now, self.delivery.last_failure_ms_ago),
            delivery_failures: self.delivery.delivery_failures,
        }
    }
}

/// Момент `ago` миллисекунд до `now` (наносекунды core).
pub fn before(now: i64, ago: u64) -> i64 {
    now - ago as i64 * 1_000_000
}

fn at(now: i64, ago: Option<u64>) -> Option<String> {
    ago.map(|ago| format_timestamp(before(now, ago)))
}

fn age(ago: Option<u64>) -> bool {
    ago.is_none_or(|ago| ago <= MAX_AGE_MS)
}

fn version(value: &str) -> bool {
    !value.is_empty() && text(value)
}

fn text(value: &str) -> bool {
    value.len() <= MAX_TEXT && !value.chars().any(char::is_control)
}
