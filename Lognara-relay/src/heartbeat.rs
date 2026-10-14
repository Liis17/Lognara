//! Телеметрия relay и последние heartbeat агентов. Раз в интервал relay отправляет их в
//! core `POST /v1/heartbeat`. Время передаётся возрастом в миллисекундах: core переводит
//! его в свои часы, поэтому часы relay и агентов не влияют на свежесть.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use reqwest::header::CONTENT_TYPE;
use reqwest::{Client, StatusCode, Url};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::time::{self, MissedTickBehavior};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::agents::{Agents, Hash};
use crate::config::Config;
use crate::spool::Spool;

/// Heartbeat агента повторяется в core, пока не устарел; core хранит самый свежий.
const KEEP_AGENT: Duration = Duration::from_secs(300);
const MAX_TEXT: usize = 256;
/// Та же граница исторических отметок (100 лет), что у core.
const MAX_AGE_MS: u64 = 100 * 365 * 24 * 60 * 60 * 1_000;
pub const MAX_BODY: usize = 64 << 10;

#[derive(Clone, Serialize, Deserialize)]
pub struct Source {
    pub environment: Option<String>,
    pub server: String,
    pub backend: String,
    pub service: String,
    pub service_instance: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct SpoolUsage {
    pub used_bytes: u64,
    pub limit_bytes: u64,
    pub records: u64,
}

/// Heartbeat агента `POST /v1/heartbeat`. Неизвестные поля новых агентов игнорируются.
#[derive(Clone, Serialize, Deserialize)]
pub struct AgentReport {
    pub version: String,
    pub source: Source,
    pub spool: SpoolUsage,
    pub last_record_ms_ago: Option<u64>,
    pub last_delivery_ms_ago: Option<u64>,
    pub last_failure_ms_ago: Option<u64>,
    pub delivery_failures: u64,
}

impl AgentReport {
    /// Те же правила, что у core: один неверный агент не должен отклонить heartbeat relay.
    pub fn valid(&self) -> bool {
        let source = &self.source;
        !self.version.is_empty()
            && [
                &self.version,
                &source.server,
                &source.backend,
                &source.service,
            ]
            .into_iter()
            .chain(source.environment.as_ref())
            .chain(source.service_instance.as_ref())
            .all(|value| value.len() <= MAX_TEXT && !value.chars().any(char::is_control))
            && [
                self.last_record_ms_ago,
                self.last_delivery_ms_ago,
                self.last_failure_ms_ago,
            ]
            .into_iter()
            .flatten()
            .all(|ago| ago <= MAX_AGE_MS)
    }
}

/// Отметки хранятся как миллисекунды от запуска + 1; 0 означает «не было».
pub struct Telemetry {
    started: Instant,
    last_ingest: AtomicU64,
    events: AtomicU64,
    last_delivery: AtomicU64,
    last_failure: AtomicU64,
    failures: AtomicU64,
    agents: Mutex<HashMap<Hash, (Instant, AgentReport)>>,
}

impl Default for Telemetry {
    fn default() -> Self {
        Self {
            started: Instant::now(),
            last_ingest: AtomicU64::new(0),
            events: AtomicU64::new(0),
            last_delivery: AtomicU64::new(0),
            last_failure: AtomicU64::new(0),
            failures: AtomicU64::new(0),
            agents: Mutex::default(),
        }
    }
}

impl Telemetry {
    pub fn ingested(&self, events: u64) {
        self.events.fetch_add(events, Ordering::Relaxed);
        self.mark(&self.last_ingest);
    }

    pub fn delivered(&self) {
        self.mark(&self.last_delivery);
    }

    pub fn failed(&self) {
        self.failures.fetch_add(1, Ordering::Relaxed);
        self.mark(&self.last_failure);
    }

    pub fn agent(&self, hash: Hash, report: AgentReport) {
        self.agents
            .lock()
            .unwrap()
            .insert(hash, (Instant::now(), report));
    }

    fn mark(&self, mark: &AtomicU64) {
        mark.store(
            self.started.elapsed().as_millis() as u64 + 1,
            Ordering::Relaxed,
        );
    }

    fn ago(&self, mark: &AtomicU64) -> Option<u64> {
        match mark.load(Ordering::Relaxed) {
            0 => None,
            at => Some((self.started.elapsed().as_millis() as u64 + 1).saturating_sub(at)),
        }
    }

    /// Тело для core; устаревшие и отозванные агенты забываются.
    fn body(
        &self,
        agents: &Agents,
        spool: &Spool,
        limit: u64,
        events_per_second: f64,
    ) -> serde_json::Value {
        let mut known = self.agents.lock().unwrap();
        known.retain(|hash, (at, _)| at.elapsed() < KEEP_AGENT && agents.contains_hash(hash));
        let reports: Vec<_> = known
            .iter()
            .map(|(hash, (at, report))| {
                json!({
                    "token_sha256": hex(hash),
                    "heartbeat_ms_ago": at.elapsed().as_millis() as u64,
                    "telemetry": report,
                })
            })
            .collect();
        json!({
            "relay": {
                "version": env!("CARGO_PKG_VERSION"),
                "spool": SpoolUsage {
                    used_bytes: spool.bytes(),
                    limit_bytes: limit,
                    records: spool.events(),
                },
                "last_ingest_ms_ago": self.ago(&self.last_ingest),
                "last_delivery_ms_ago": self.ago(&self.last_delivery),
                "last_failure_ms_ago": self.ago(&self.last_failure),
                "delivery_failures": self.failures.load(Ordering::Relaxed),
                "events_per_second": events_per_second,
            },
            "agents": reports,
        })
    }
}

fn hex(hash: &Hash) -> String {
    hash.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Отправляет heartbeat каждые `config.heartbeat_interval`, первый — сразу.
pub async fn run(
    telemetry: Arc<Telemetry>,
    agents: Arc<Agents>,
    spool: Spool,
    config: &Config,
    stop: CancellationToken,
) {
    let client = Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("HTTP client builds");
    // Core URL проверен конфигурацией как http(s), он всегда может быть базой.
    let url: Url = config
        .core_url
        .join("heartbeat")
        .expect("core URL is a base URL");
    let mut ticker = time::interval(config.heartbeat_interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let (mut counted, mut since) = (0, Instant::now());
    let mut failing = false;
    loop {
        tokio::select! {
            _ = stop.cancelled() => return,
            _ = ticker.tick() => {}
        }
        let events = telemetry.events.load(Ordering::Relaxed);
        let rate = (events - counted) as f64 / since.elapsed().as_secs_f64().max(0.001);
        (counted, since) = (events, Instant::now());
        let body = telemetry.body(&agents, &spool, config.spool_max_bytes, rate);
        let request = client
            .post(url.clone())
            .bearer_auth(&config.core_token)
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .send();
        let result = tokio::select! {
            _ = stop.cancelled() => return,
            result = request => result,
        };
        match result {
            Ok(response) if response.status() == StatusCode::NO_CONTENT => {
                if failing {
                    info!("heartbeat delivered to core");
                }
                failing = false;
            }
            Ok(response) if !failing => {
                warn!(status = %response.status(), "core rejected heartbeat");
                failing = true;
            }
            Err(error) if !failing => {
                warn!(error = %error.without_url(), "heartbeat not delivered");
                failing = true;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    #[test]
    fn historical_agent_marks_remain_valid_with_a_bounded_age() {
        let mut report: AgentReport = serde_json::from_value(json!({
            "version": "1", "source": {"environment": null, "server": "s", "backend": "b", "service": "api", "service_instance": null},
            "spool": {"used_bytes": 0, "limit_bytes": 1, "records": 0},
            "last_record_ms_ago": null, "last_delivery_ms_ago": null, "last_failure_ms_ago": null, "delivery_failures": 1
        })).unwrap();
        for ago in [86_400_001, MAX_AGE_MS] {
            report.last_record_ms_ago = Some(ago);
            report.last_delivery_ms_ago = Some(ago);
            report.last_failure_ms_ago = Some(ago);
            assert!(report.valid());
        }
        for ago in [MAX_AGE_MS + 1, u64::MAX] {
            for mark in 0..3 {
                report.last_record_ms_ago = (mark == 0).then_some(ago);
                report.last_delivery_ms_ago = (mark == 1).then_some(ago);
                report.last_failure_ms_ago = (mark == 2).then_some(ago);
                assert!(!report.valid());
            }
        }
    }

    #[tokio::test]
    async fn revoked_agents_are_not_forwarded() {
        let dir = tempfile::tempdir().unwrap();
        let spool = Spool::open(dir.path(), 1 << 20).await.unwrap();
        let telemetry = Telemetry::default();
        let report: AgentReport = serde_json::from_value(json!({
            "version": "1", "source": {"environment": null, "server": "s", "backend": "b", "service": "api", "service_instance": null},
            "spool": {"used_bytes": 0, "limit_bytes": 1, "records": 0},
            "last_record_ms_ago": null, "last_delivery_ms_ago": null, "last_failure_ms_ago": null, "delivery_failures": 0
        })).unwrap();
        for token in ["kept", "revoked"] {
            telemetry.agent(Sha256::digest(token).into(), report.clone());
        }
        let body = telemetry.body(&Agents::with_tokens(&["kept"]), &spool, 1, 0.0);
        let agents = body["agents"].as_array().unwrap();
        assert_eq!(agents.len(), 1);
        assert_eq!(
            agents[0]["token_sha256"],
            hex(&Sha256::digest("kept").into())
        );
        assert!(body["relay"]["last_ingest_ms_ago"].is_null());
        assert_eq!(telemetry.agents.lock().unwrap().len(), 1);
    }
}
