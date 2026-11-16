//! Инфраструктура: состояние core и dock, зарегистрированные relay и агенты по heartbeat.
//! Свежесть считается по часам core (`as_of` списков), а не по часам dock или узлов.

use std::time::Duration;

use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::api::AppState;
use crate::session::AuthUser;

/// Без heartbeat дольше этого узел отключён: relay и агенты шлют его раз в 10 секунд.
pub const HEARTBEAT_TTL: Duration = Duration::from_secs(60);
/// Heartbeat есть, а событий дольше этого нет: «Нет новых логов».
pub const IDLE: Duration = Duration::from_secs(300);
pub const SPOOL_WARN_PERCENT: u64 = 80;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Spool {
    pub used_bytes: u64,
    pub limit_bytes: u64,
    pub records: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Source {
    pub environment: Option<String>,
    pub server: String,
    pub backend: String,
    pub service: String,
    pub service_instance: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CoreStatus {
    pub version: String,
    #[serde(default)]
    pub release_version: Option<String>,
    #[serde(default)]
    pub git_commit: Option<String>,
    pub ready: bool,
    pub visibility_lag_ms: u64,
    pub wal_bytes: u64,
    pub wal_limit_bytes: u64,
    pub storage: Storage,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Storage {
    pub segments: u64,
    pub parquet_bytes: u64,
    pub index_bytes: u64,
    pub oldest_event_at: Option<String>,
    pub retention_seconds: u64,
    pub disk_total_bytes: u64,
    pub disk_free_bytes: u64,
}

#[derive(Deserialize)]
struct Relays {
    relays: Vec<Relay>,
    as_of: String,
}

#[derive(Deserialize)]
struct Relay {
    id: i64,
    name: String,
    #[serde(default = "relay_kind")]
    kind: String,
    heartbeat: Option<RelayHeartbeat>,
}

fn relay_kind() -> String {
    "relay".into()
}

#[derive(Deserialize)]
struct RelayHeartbeat {
    received_at: String,
    version: String,
    #[serde(default)]
    release_version: Option<String>,
    spool: Spool,
    last_ingest_at: Option<String>,
    last_delivery_at: Option<String>,
    last_failure_at: Option<String>,
    delivery_failures: u64,
    events_per_second: f64,
}

#[derive(Deserialize)]
struct Agents {
    agents: Vec<Agent>,
}

#[derive(Deserialize)]
struct Agent {
    id: i64,
    name: String,
    relay_id: i64,
    heartbeat: Option<AgentHeartbeat>,
}

#[derive(Deserialize)]
struct AgentHeartbeat {
    received_at: String,
    version: String,
    source: Source,
    spool: Spool,
    last_record_at: Option<String>,
    last_delivery_at: Option<String>,
    last_failure_at: Option<String>,
    delivery_failures: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeStatus {
    Offline,
    Degraded,
    Unknown,
    Idle,
    Online,
}

/// Почему узел не «онлайн»: `no_heartbeat`, `relay_silent`, `spool`, `delivery`.
pub type Reason = Option<&'static str>;

#[derive(Debug, Serialize)]
pub struct Node {
    pub kind: &'static str,
    pub id: i64,
    pub name: String,
    /// ID relay из реестра Core для агента; у relay всегда `None`.
    pub relay_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<i64>,
    /// Relay агента.
    pub relay: Option<String>,
    pub status: NodeStatus,
    pub reason: Reason,
    pub heartbeat_at: Option<String>,
    pub last_event_at: Option<String>,
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_version: Option<String>,
    pub spool: Option<Spool>,
    pub delivery_failures: Option<u64>,
    pub events_per_second: Option<f64>,
    /// Источник из конфигурации агента: его поля ставятся на события агента.
    pub source: Option<Source>,
}

#[derive(Debug, Serialize)]
pub struct CoreView {
    pub status: &'static str,
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_commit: Option<String>,
    pub visibility_lag_ms: Option<u64>,
    pub wal_bytes: Option<u64>,
    pub wal_limit_bytes: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct DockView {
    pub version: &'static str,
    pub release_version: Option<String>,
    pub git_commit: Option<&'static str>,
    pub sse_clients: usize,
    pub sessions: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct Thresholds {
    pub heartbeat_ttl_seconds: u64,
    pub idle_seconds: u64,
    pub spool_warn_percent: u64,
}

#[derive(Debug, Serialize)]
pub struct Infrastructure {
    pub as_of: String,
    pub thresholds: Thresholds,
    pub core: CoreView,
    pub storage: Option<Storage>,
    pub dock: DockView,
    /// Список узлов получен; иначе `nodes` пуст из-за недоступного core.
    pub registry: bool,
    pub nodes: Vec<Node>,
}

/// `GET /api/infrastructure`.
pub async fn overview(State(app): State<AppState>, _user: AuthUser) -> Json<Infrastructure> {
    let (status, relays, agents, sessions) = tokio::join!(
        app.core.status(),
        app.core.list_relays(),
        app.core.list_agents(),
        app.sessions.active(),
    );
    let core = match &status {
        Ok(status) => CoreView {
            status: if status.ready { "online" } else { "not_ready" },
            version: Some(status.version.clone()),
            release_version: status.release_version.clone(),
            git_commit: status.git_commit.clone(),
            visibility_lag_ms: Some(status.visibility_lag_ms),
            wal_bytes: Some(status.wal_bytes),
            wal_limit_bytes: Some(status.wal_limit_bytes),
        },
        Err(error) => {
            tracing::warn!("core status unavailable: {error}");
            CoreView {
                status: "unavailable",
                version: None,
                release_version: None,
                git_commit: None,
                visibility_lag_ms: None,
                wal_bytes: None,
                wal_limit_bytes: None,
            }
        }
    };
    let registry = relays.ok().zip(agents.ok()).and_then(|(relays, agents)| {
        Some((
            serde_json::from_value::<Relays>(relays).ok()?,
            serde_json::from_value::<Agents>(agents).ok()?,
        ))
    });
    let (as_of, nodes) = match registry {
        Some((relays, agents)) => {
            let nodes = time(&relays.as_of).map(|now| nodes(now, relays.relays, agents.agents));
            (relays.as_of, nodes)
        }
        None => (now(), None),
    };
    Json(Infrastructure {
        as_of,
        thresholds: Thresholds {
            heartbeat_ttl_seconds: HEARTBEAT_TTL.as_secs(),
            idle_seconds: IDLE.as_secs(),
            spool_warn_percent: SPOOL_WARN_PERCENT,
        },
        core,
        storage: status.ok().map(|status| status.storage),
        dock: DockView {
            version: env!("CARGO_PKG_VERSION"),
            release_version: Some(crate::version::release_version()),
            git_commit: option_env!("LOGNARA_GIT_COMMIT").filter(|value| !value.is_empty()),
            sse_clients: app.live.clients(),
            sessions: sessions.ok(),
        },
        registry: nodes.is_some(),
        nodes: nodes.unwrap_or_default(),
    })
}

fn nodes(now: OffsetDateTime, relays: Vec<Relay>, agents: Vec<Agent>) -> Vec<Node> {
    let host_relays: std::collections::HashMap<_, _> = relays
        .iter()
        .filter(|relay| relay.kind == "host")
        .map(|relay| (relay.id, relay.name.as_str()))
        .collect();
    let mut host_agents = std::collections::HashMap::new();
    let mut regular_agents = Vec::new();
    for agent in agents {
        if host_relays
            .get(&agent.relay_id)
            .is_some_and(|name| *name == agent.name)
        {
            host_agents.insert(agent.relay_id, agent);
        } else if !host_relays.contains_key(&agent.relay_id) {
            regular_agents.push(agent);
        }
    }

    let mut nodes: Vec<Node> = relays
        .into_iter()
        .map(|relay| {
            let is_host = relay.kind == "host";
            let kind = if is_host { "host" } else { "relay" };
            let host_agent = host_agents.remove(&relay.id);
            let agent_id = host_agent.as_ref().map(|agent| agent.id);
            let host_heartbeat = host_agent.and_then(|agent| agent.heartbeat);
            let host_source = host_heartbeat
                .as_ref()
                .map(|heartbeat| heartbeat.source.clone());
            let host_last_event = host_heartbeat
                .as_ref()
                .and_then(|heartbeat| heartbeat.last_record_at.clone());
            let host_version = host_heartbeat.map(|heartbeat| heartbeat.version);
            let Some(beat) = relay.heartbeat else {
                let mut node = silent(
                    kind,
                    relay.id,
                    relay.name,
                    is_host.then_some(relay.id),
                    None,
                );
                node.agent_id = agent_id;
                node.source = host_source;
                node.last_event_at = host_last_event;
                node.version = host_version;
                return node;
            };
            let (status, reason) = assess(
                now,
                &beat.received_at,
                beat.last_ingest_at.as_deref(),
                &beat.spool,
                (
                    beat.last_delivery_at.as_deref(),
                    beat.last_failure_at.as_deref(),
                ),
            );
            Node {
                kind,
                id: relay.id,
                name: relay.name,
                relay_id: is_host.then_some(relay.id),
                agent_id,
                relay: None,
                status,
                reason,
                heartbeat_at: Some(beat.received_at),
                last_event_at: host_last_event.or(beat.last_ingest_at),
                version: host_version.or(Some(beat.version)),
                release_version: beat.release_version,
                spool: Some(beat.spool),
                delivery_failures: Some(beat.delivery_failures),
                events_per_second: Some(beat.events_per_second),
                source: host_source,
            }
        })
        .collect();
    for agent in regular_agents {
        let relay = nodes
            .iter()
            .find(|node| node.kind == "relay" && node.id == agent.relay_id)
            .map(|node| (node.name.clone(), node.status));
        let relay_name = relay.as_ref().map(|(name, _)| name.clone());
        let Some(beat) = agent.heartbeat else {
            nodes.push(silent(
                "agent",
                agent.id,
                agent.name,
                Some(agent.relay_id),
                relay_name,
            ));
            continue;
        };
        let (mut status, mut reason) = assess(
            now,
            &beat.received_at,
            beat.last_record_at.as_deref(),
            &beat.spool,
            (
                beat.last_delivery_at.as_deref(),
                beat.last_failure_at.as_deref(),
            ),
        );
        // Heartbeat агента приходит через relay: молчащий relay не доказывает отключение агента.
        if status == NodeStatus::Offline
            && relay.is_none_or(|(_, status)| {
                matches!(status, NodeStatus::Offline | NodeStatus::Unknown)
            })
        {
            (status, reason) = (NodeStatus::Unknown, Some("relay_silent"));
        }
        nodes.push(Node {
            kind: "agent",
            id: agent.id,
            name: agent.name,
            relay_id: Some(agent.relay_id),
            agent_id: Some(agent.id),
            relay: relay_name,
            status,
            reason,
            heartbeat_at: Some(beat.received_at),
            last_event_at: beat.last_record_at,
            version: Some(beat.version),
            release_version: None,
            spool: Some(beat.spool),
            delivery_failures: Some(beat.delivery_failures),
            events_per_second: None,
            source: Some(beat.source),
        });
    }
    nodes.sort_by(|a, b| (a.status, a.kind, &a.name).cmp(&(b.status, b.kind, &b.name)));
    nodes
}

fn silent(
    kind: &'static str,
    id: i64,
    name: String,
    relay_id: Option<i64>,
    relay: Option<String>,
) -> Node {
    Node {
        kind,
        id,
        name,
        relay_id,
        agent_id: (kind == "agent").then_some(id),
        relay,
        status: NodeStatus::Unknown,
        reason: Some("no_heartbeat"),
        heartbeat_at: None,
        last_event_at: None,
        version: None,
        release_version: None,
        spool: None,
        delivery_failures: None,
        events_per_second: None,
        source: None,
    }
}

/// Отключён без свежего heartbeat; деградирован при заполненном spool или ошибке доставки
/// после последнего успеха; «нет новых логов», если давно не было событий.
fn assess(
    now: OffsetDateTime,
    heartbeat: &str,
    last_event: Option<&str>,
    spool: &Spool,
    (delivery, failure): (Option<&str>, Option<&str>),
) -> (NodeStatus, Reason) {
    let age = |value: Option<&str>| value.and_then(time).map(|at| now - at);
    if age(Some(heartbeat)).is_none_or(|age| age > HEARTBEAT_TTL) {
        return (NodeStatus::Offline, None);
    }
    if spool.limit_bytes > 0
        && u128::from(spool.used_bytes) * 100
            >= u128::from(spool.limit_bytes) * u128::from(SPOOL_WARN_PERCENT)
    {
        return (NodeStatus::Degraded, Some("spool"));
    }
    let failing = match (age(failure), age(delivery)) {
        (Some(failure), Some(delivery)) => failure < delivery && failure <= HEARTBEAT_TTL,
        (Some(failure), None) => failure <= HEARTBEAT_TTL,
        _ => false,
    };
    if failing {
        return (NodeStatus::Degraded, Some("delivery"));
    }
    if age(last_event).is_none_or(|age| age > IDLE) {
        return (NodeStatus::Idle, None);
    }
    (NodeStatus::Online, None)
}

fn time(value: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(value, &Rfc3339).ok()
}

fn now() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .expect("UTC time is RFC3339")
}
