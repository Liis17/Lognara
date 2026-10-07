//! Повторяет сохранённые байты до подтверждения relay; ничего не отбрасывает.

use std::sync::Arc;
use std::time::Duration;

use reqwest::header::{CONTENT_ENCODING, CONTENT_TYPE};
use reqwest::{Client, Url};
use tokio::time::{self, Instant, MissedTickBehavior};
use tokio_util::sync::CancellationToken;
use tracing::{error, warn};

use crate::buffer::{Buffer, Pending};
use crate::config::Config;

pub struct Sender {
    buffer: Arc<Buffer>,
    client: Client,
    url: Url,
    token: String,
    flush_interval: Duration,
    shutdown: CancellationToken,
    pending: Option<Pending>,
}
impl Sender {
    pub fn new(config: &Config, buffer: Arc<Buffer>, shutdown: CancellationToken) -> Self {
        Self {
            buffer,
            client: Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .expect("HTTP client"),
            url: config.relay_url.clone(),
            token: config.relay_token.clone(),
            flush_interval: config.flush_interval,
            shutdown,
            pending: None,
        }
    }
    pub async fn run(mut self) {
        if self.buffer.len() > 0 {
            self.flush(true).await;
        }
        let mut ticker =
            time::interval_at(Instant::now() + self.flush_interval, self.flush_interval);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = self.shutdown.cancelled() => break,
                _ = ticker.tick() => self.flush(true).await,
                _ = self.buffer.full() => self.flush(false).await,
            }
        }
        self.shutdown = CancellationToken::new();
        // Отмена HTTP/worker безопасна: исходные записи и pending уже на диске.
        let _ = time::timeout(Duration::from_secs(5), self.flush(true)).await;
        self.pending = None;
        if self.buffer.len() > 0 {
            warn!(
                queued = self.buffer.len(),
                "agent stopped with durable backlog"
            );
        }
    }
    async fn flush(&mut self, partial: bool) {
        let mut backoff = Duration::from_millis(100);
        loop {
            if self.pending.is_none() {
                match self.buffer.prepare_async(partial).await {
                    Ok(pending) => self.pending = pending,
                    Err(e) => {
                        self.buffer.set_ready(false);
                        error!(error = %e, "agent spool read failed; data retained");
                        return;
                    }
                }
            }
            let Some(pending) = &self.pending else {
                return;
            };
            let request = self
                .client
                .post(self.url.clone())
                .bearer_auth(&self.token)
                .header(CONTENT_TYPE, "application/msgpack")
                .header(CONTENT_ENCODING, "zstd")
                .body(pending.body.clone())
                .send();
            let response = tokio::select! {
                result = request => result,
                _ = self.shutdown.cancelled() => return,
            };
            match response {
                Ok(response) if response.status().is_success() => {
                    let pending = self.pending.take().unwrap();
                    if let Err(e) = self.buffer.ack_async(pending).await {
                        self.buffer.set_ready(false);
                        error!(error = %e, "agent spool ACK failed; delivery paused");
                        return;
                    }
                    self.buffer.set_ready(true);
                    backoff = Duration::from_millis(100);
                    continue;
                }
                Ok(response) => {
                    if response.status().is_client_error() {
                        self.buffer.set_ready(false);
                    }
                    warn!(status = %response.status(), "relay rejected batch; exact bytes retained");
                }
                Err(e) => warn!(error = %e, "relay unavailable; exact bytes retained"),
            }
            tokio::select! {
                _ = time::sleep(backoff) => {},
                _ = self.shutdown.cancelled() => return,
            }
            backoff = (backoff * 2).min(Duration::from_secs(10));
        }
    }
}
