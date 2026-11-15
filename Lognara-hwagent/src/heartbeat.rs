//! Heartbeat хоста в core `POST /v1/heartbeat` в формате relay с одной записью агента:
//! core сохраняет состояние и relay, и агента хоста.

use std::time::Duration;

use lognara_agent::TelemetrySnapshot;
use reqwest::{Client, StatusCode, Url};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::time::{self, MissedTickBehavior};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::config::Config;

pub fn body(config: &Config, telemetry: TelemetrySnapshot) -> Value {
    let agent = &config.agent;
    let spool = json!({
        "used_bytes": telemetry.spool_bytes,
        "limit_bytes": agent.spool_max_bytes,
        "records": telemetry.spool_records,
    });
    let version = env!("CARGO_PKG_VERSION");
    json!({
        "relay": {
            "version": version,
            "spool": spool,
            "last_ingest_ms_ago": telemetry.last_record_ms_ago,
            "last_delivery_ms_ago": telemetry.last_delivery_ms_ago,
            "last_failure_ms_ago": telemetry.last_failure_ms_ago,
            "delivery_failures": telemetry.delivery_failures,
            "events_per_second": 0.0,
        },
        "agents": [{
            "token_sha256": hex(&Sha256::digest(config.token.as_bytes())),
            "heartbeat_ms_ago": 0,
            "telemetry": {
                "version": version,
                "source": {
                    "environment": agent.environment,
                    "server": agent.server,
                    "backend": agent.backend,
                    "service": agent.service,
                    "service_instance": agent.service_instance,
                },
                "spool": spool,
                "last_record_ms_ago": telemetry.last_record_ms_ago,
                "last_delivery_ms_ago": telemetry.last_delivery_ms_ago,
                "last_failure_ms_ago": telemetry.last_failure_ms_ago,
                "delivery_failures": telemetry.delivery_failures,
            },
        }],
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Первый тик сразу; ошибки не влияют на сбор и доставку.
pub async fn run(
    config: Config,
    telemetry: impl Fn() -> TelemetrySnapshot,
    stop: CancellationToken,
) {
    let Ok(url) = config.core_url.join("heartbeat") else {
        warn!("heartbeat URL is invalid; heartbeat disabled");
        return;
    };
    let url: Url = url;
    let client = Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("HTTP client");
    let mut ticker = time::interval(config.heartbeat_interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut failing = false;
    loop {
        tokio::select! {
            _ = stop.cancelled() => return,
            _ = ticker.tick() => {}
        }
        let request = client
            .post(url.clone())
            .bearer_auth(&config.token)
            .json(&body(&config, telemetry()))
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
