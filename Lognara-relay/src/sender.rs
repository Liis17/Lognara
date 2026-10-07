//! Доставляет сохранённые тела core; удаляет только после успешного ACK.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use reqwest::header::{CONTENT_ENCODING, CONTENT_TYPE};
use reqwest::{Client, StatusCode, Url};
use tokio::time::{self, Instant, MissedTickBehavior};
use tokio_util::sync::CancellationToken;
use tracing::{error, warn};

use crate::config::Config;
use crate::memory::{CODEC_WORKSPACE, Resources};
use crate::spool::Spool;

pub struct Sender {
    spool: Spool,
    core: Core,
    resources: Arc<Resources>,
    shutdown: CancellationToken,
    flush_interval: Duration,
    batch_size: usize,
    replay_limit: usize,
}

impl Sender {
    pub fn new(
        config: &Config,
        spool: Spool,
        shutdown: CancellationToken,
        resources: Arc<Resources>,
    ) -> Self {
        Self {
            spool,
            core: Core {
                client: Client::builder()
                    .timeout(Duration::from_secs(30))
                    .build()
                    .expect("HTTP client builds"),
                url: config.core_url.clone(),
                token: config.core_token.clone(),
            },
            resources,
            shutdown,
            flush_interval: config.flush_interval,
            batch_size: config.batch_size,
            replay_limit: config.sender_bytes().unwrap() - CODEC_WORKSPACE,
        }
    }

    pub async fn run(self) {
        let mut ticker = time::interval_at(Instant::now(), self.flush_interval);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            let flush = tokio::select! {
                _ = self.shutdown.cancelled() => break,
                _ = ticker.tick() => true,
                _ = self.spool.changed() => self.spool.events() >= self.batch_size as u64,
            };
            if flush {
                tokio::select! {
                    _ = self.shutdown.cancelled() => break,
                    _ = self.flush() => {},
                }
            }
        }
        let _ = time::timeout(Duration::from_secs(5), self.flush()).await;
        self.spool.drain().await;
        if !self.spool.is_empty() {
            warn!(
                batches = self.spool.len(),
                "confirmed batches remain in spool"
            );
        }
    }

    async fn flush(&self) {
        loop {
            let body = match self.spool.oldest_limited(self.replay_limit).await {
                Ok(body) => body,
                Err(error) => {
                    self.resources.set_ready(false);
                    error!(%error, "spool read failed; file retained, ingest paused");
                    return;
                }
            };
            self.resources.set_ready(self.spool.fits_replay_limit());
            let Some(body) = body else {
                return;
            };
            if !self.core.deliver(body.into()).await {
                return;
            }
            if let Err(error) = self.spool.remove_oldest().await {
                self.resources.set_ready(false);
                error!(%error, "spool ACK failed; ingest paused");
                return;
            }
        }
    }
}

struct Core {
    client: Client,
    url: Url,
    token: String,
}
impl Core {
    async fn deliver(&self, body: Bytes) -> bool {
        match self
            .client
            .post(self.url.clone())
            .bearer_auth(&self.token)
            .header(CONTENT_TYPE, "application/msgpack")
            .header(CONTENT_ENCODING, "zstd")
            .body(body)
            .send()
            .await
        {
            Ok(response) if response.status().is_success() => true,
            Ok(response) => {
                let status: StatusCode = response.status();
                warn!(%status, "core rejected batch; retained for retry");
                false
            }
            Err(error) => {
                warn!(%error, "core unavailable; batch retained");
                false
            }
        }
    }
}
