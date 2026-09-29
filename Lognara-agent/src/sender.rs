//! Отправка накопленных записей в lognara-relay.

use std::sync::Arc;
use std::time::Duration;

use reqwest::header::{CONTENT_ENCODING, CONTENT_TYPE};
use reqwest::{Client, StatusCode, Url};
use tokio::time::{self, Instant, MissedTickBehavior};
use tokio_util::sync::CancellationToken;
use tracing::{error, warn};

use crate::buffer::Buffer;
use crate::config::Config;
use crate::wire::{self, Batch, Record};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const MIN_BACKOFF: Duration = Duration::from_millis(100);
const MAX_BACKOFF: Duration = Duration::from_secs(10);
/// Сколько после остановки ждать доставки остатка.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

pub struct Sender {
    buffer: Arc<Buffer>,
    relay: Relay,
    /// Поля идентификации источника, общие для всех пачек.
    template: Batch,
    flush_interval: Duration,
    shutdown: CancellationToken,
    /// Пачка, доставку которой прервала остановка агента.
    pending: Option<Batch>,
}

impl Sender {
    pub fn new(config: &Config, buffer: Arc<Buffer>, shutdown: CancellationToken) -> Self {
        let client = Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .expect("HTTP client without TLS always builds");
        Self {
            buffer,
            relay: Relay {
                client,
                url: config.relay_url.clone(),
            },
            template: Batch {
                service: config.service.clone(),
                server: config.server.clone(),
                backend: config.backend.clone(),
                environment: config.environment.clone(),
                service_instance: config.service_instance.clone(),
                sent_at: 0,
                dropped: 0,
                records: Vec::new(),
            },
            flush_interval: config.flush_interval,
            shutdown,
            pending: None,
        }
    }

    /// Отправляет пачку, как только набран `batch_size`, и всё накопленное
    /// раз в `flush_interval`. После остановки отправляет остаток и завершается.
    pub async fn run(mut self) {
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

        if time::timeout(SHUTDOWN_TIMEOUT, self.flush(true))
            .await
            .is_err()
        {
            warn!("timed out delivering remaining records to relay");
        }
        let lost = self.buffer.len() + self.pending.as_ref().map_or(0, |batch| batch.records.len());
        if lost > 0 {
            warn!(lost, "records were not delivered to relay before shutdown");
        }
    }

    /// Отправляет пачки по одной, пока они есть; при `partial = false` только полные.
    async fn flush(&mut self, partial: bool) {
        loop {
            if self.pending.is_none() {
                self.pending = self
                    .buffer
                    .take(partial)
                    .map(|(records, dropped)| self.batch(records, dropped));
            }
            let Some(batch) = self.pending.as_mut() else {
                return;
            };
            if !self.relay.deliver(batch, &self.shutdown).await {
                return;
            }
            self.pending = None;
        }
    }

    fn batch(&self, records: Vec<Record>, dropped: u64) -> Batch {
        if dropped > 0 {
            warn!(dropped, "buffer overflowed, oldest records were dropped");
        }
        Batch {
            records,
            dropped,
            ..self.template.clone()
        }
    }
}

struct Relay {
    client: Client,
    url: Url,
}

impl Relay {
    /// Доставляет пачку, повторяя попытки при недоступности relay.
    /// Возвращает `false`, только если попытки прервала остановка агента.
    async fn deliver(&self, batch: &mut Batch, shutdown: &CancellationToken) -> bool {
        let mut backoff = MIN_BACKOFF;
        loop {
            batch.sent_at = wire::unix_nanos();
            match self.post(wire::encode(batch)).await {
                Ok(status) if status.is_success() => return true,
                Ok(status) if is_permanent(status) => {
                    // Повтор не поможет, а застрявшая пачка остановила бы всю отправку.
                    error!(%status, records = batch.records.len(), "relay rejected batch, dropping it");
                    return true;
                }
                Ok(status) => warn!(%status, "relay is unavailable"),
                Err(err) => warn!(error = %err, "relay is unavailable"),
            }

            tokio::select! {
                _ = time::sleep(backoff) => {}
                _ = shutdown.cancelled() => return false,
            }
            backoff = (backoff * 2).min(MAX_BACKOFF);
        }
    }

    async fn post(&self, body: Vec<u8>) -> reqwest::Result<StatusCode> {
        let response = self
            .client
            .post(self.url.clone())
            .header(CONTENT_TYPE, "application/msgpack")
            .header(CONTENT_ENCODING, "zstd")
            .body(body)
            .send()
            .await?;
        Ok(response.status())
    }
}

fn is_permanent(status: StatusCode) -> bool {
    status.is_client_error()
        && status != StatusCode::REQUEST_TIMEOUT
        && status != StatusCode::TOO_MANY_REQUESTS
}
