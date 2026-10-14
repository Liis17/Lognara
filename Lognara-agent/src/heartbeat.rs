//! Heartbeat агента в relay `POST /v1/heartbeat`: версия, источник, spool и доставка.
//! Время передаётся возрастом в миллисекундах, поэтому часы агента не важны.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use reqwest::header::CONTENT_TYPE;
use reqwest::{Client, StatusCode, Url};
use serde_json::json;
use tokio::time::{self, MissedTickBehavior};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::buffer::Buffer;
use crate::config::Config;

/// Отметки хранятся как миллисекунды от запуска + 1; 0 означает «не было».
pub struct Telemetry {
    started: Instant,
    last_record: AtomicU64,
    last_delivery: AtomicU64,
    last_failure: AtomicU64,
    failures: AtomicU64,
}

impl Default for Telemetry {
    fn default() -> Self {
        Self {
            started: Instant::now(),
            last_record: AtomicU64::new(0),
            last_delivery: AtomicU64::new(0),
            last_failure: AtomicU64::new(0),
            failures: AtomicU64::new(0),
        }
    }
}

impl Telemetry {
    pub fn recorded(&self) {
        self.mark(&self.last_record);
    }

    pub fn delivered(&self) {
        self.mark(&self.last_delivery);
    }

    pub fn failed(&self) {
        self.failures.fetch_add(1, Ordering::Relaxed);
        self.mark(&self.last_failure);
    }

    fn mark(&self, mark: &AtomicU64) {
        mark.store(self.now(), Ordering::Relaxed);
    }

    fn ago(&self, mark: &AtomicU64) -> Option<u64> {
        match mark.load(Ordering::Relaxed) {
            0 => None,
            at => Some(self.now().saturating_sub(at)),
        }
    }

    fn now(&self) -> u64 {
        self.started.elapsed().as_millis() as u64 + 1
    }
}

fn body(config: &Config, buffer: &Buffer) -> String {
    let telemetry = &buffer.telemetry;
    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "source": {
            "environment": config.environment,
            "server": config.server,
            "backend": config.backend,
            "service": config.service,
            "service_instance": config.service_instance,
        },
        "spool": {
            "used_bytes": buffer.bytes(),
            "limit_bytes": config.spool_max_bytes,
            "records": buffer.len(),
        },
        "last_record_ms_ago": telemetry.ago(&telemetry.last_record),
        "last_delivery_ms_ago": telemetry.ago(&telemetry.last_delivery),
        "last_failure_ms_ago": telemetry.ago(&telemetry.last_failure),
        "delivery_failures": telemetry.failures.load(Ordering::Relaxed),
    })
    .to_string()
}

/// Отправляет heartbeat каждые `config.heartbeat_interval`, первый — сразу. Ошибки не
/// влияют на приём и доставку логов; старый relay без маршрута отвечает 404.
pub async fn run(config: Config, buffer: Arc<Buffer>, stop: CancellationToken) {
    let client = Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("HTTP client");
    let url: Url = match config.relay_url.join("heartbeat") {
        Ok(url) => url,
        Err(error) => {
            warn!(%error, "heartbeat URL is invalid; heartbeat disabled");
            return;
        }
    };
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
            .bearer_auth(&config.relay_token)
            .header(CONTENT_TYPE, "application/json")
            .body(body(&config, &buffer))
            .send();
        let result = tokio::select! {
            _ = stop.cancelled() => return,
            result = request => result,
        };
        match result {
            Ok(response) if response.status() == StatusCode::NO_CONTENT => {
                if failing {
                    info!("heartbeat delivered to relay");
                }
                failing = false;
            }
            Ok(response) if response.status() == StatusCode::NOT_FOUND && !failing => {
                debug!("relay does not accept heartbeat");
                failing = true;
            }
            Ok(response) if !failing => {
                warn!(status = %response.status(), "relay rejected heartbeat");
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
