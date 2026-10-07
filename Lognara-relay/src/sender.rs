//! Отправка накопленных событий в lognara-core. Пока core недоступен, пачки ждут в spool.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use reqwest::header::{CONTENT_ENCODING, CONTENT_TYPE};
use reqwest::{Client, StatusCode, Url};
use tokio::task::JoinHandle;
use tokio::time::{self, Instant, MissedTickBehavior};
use tokio_util::sync::CancellationToken;
use tracing::{error, warn};

use crate::buffer::Buffer;
use crate::config::Config;
use crate::core_wire::{BatchEncoder, EncodedBatch};
use crate::memory::{CODEC_WORKSPACE, Reservation, Resources};
use crate::spool::Spool;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

pub struct Sender {
    buffer: Arc<Buffer>,
    spool: Spool,
    core: Core,
    flush_interval: Duration,
    shutdown: CancellationToken,
    resources: Arc<Resources>,
    pending: Option<Pending>,
    encoding: Option<Encoding>,
    // Handle принадлежит Sender, а не отменяемому future flush.
    job: Option<JoinHandle<Encoded>>,
    body_limit: usize,
    decoded_limit: usize,
    model_limit: usize,
    replay_limit: usize,
}

struct Pending {
    body: Bytes,
    events: usize,
}

struct Encoding {
    encoder: BatchEncoder,
    // Поля уничтожаются по порядку: сначала модели, затем резервы.
    _reservations: Vec<Reservation>,
}

struct Encoded {
    encoding: Option<Encoding>,
    part: Option<EncodedBatch>,
    dropped: u64,
}

impl Sender {
    pub fn new(
        config: &Config,
        buffer: Arc<Buffer>,
        spool: Spool,
        shutdown: CancellationToken,
        resources: Arc<Resources>,
    ) -> Self {
        let client = Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .expect("HTTP client with bundled rustls always builds");
        Self {
            buffer,
            spool,
            core: Core {
                client,
                url: config.core_url.clone(),
                token: config.core_token.clone(),
            },
            flush_interval: config.flush_interval,
            shutdown,
            resources,
            pending: None,
            encoding: None,
            job: None,
            body_limit: config.core_max_body_bytes,
            decoded_limit: config.core_max_decoded_bytes,
            model_limit: config.core_max_model_bytes,
            replay_limit: config.sender_bytes().unwrap() - CODEC_WORKSPACE,
        }
    }

    pub async fn run(mut self) {
        let mut ticker =
            time::interval_at(Instant::now() + self.flush_interval, self.flush_interval);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let shutdown = self.shutdown.clone();
        loop {
            let partial = tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = ticker.tick() => true,
                _ = self.buffer.full() => false,
            };
            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = self.flush(partial) => {},
            }
        }
        if time::timeout(SHUTDOWN_TIMEOUT, self.flush(true))
            .await
            .is_err()
        {
            warn!("timed out delivering batches to core");
        }
        // Дожидаемся уже запущенного кодирования и сохраняем каждый остаток.
        self.save_pending().await;
        self.seal(true);
        self.save_pending().await;
        if !self.spool.is_empty() {
            warn!(
                batches = self.spool.len(),
                "batches are kept in spool until core is available"
            );
        }
    }

    async fn flush(&mut self, partial: bool) {
        loop {
            let body = match self.spool.oldest_limited(self.replay_limit).await {
                Ok(body) => {
                    self.resources.set_ready(true);
                    body
                }
                Err(error) => {
                    self.resources.set_ready(false);
                    error!(
                        size = error.size,
                        limit = error.limit,
                        "spool batch exceeds sender memory reserve; file retained, ingest stopped"
                    );
                    break;
                }
            };
            let Some(body) = body else {
                break;
            };
            if !self.core.deliver(body.into()).await {
                break;
            }
            self.spool.remove_oldest().await;
            self.seal(false);
            self.save_pending().await;
        }
        self.seal(partial);
        while self.spool.is_empty() && self.next_part().await {
            let pending = self.pending.as_ref().unwrap();
            if !self.core.deliver(pending.body.clone()).await {
                break;
            }
            self.pending = None;
        }
        self.save_pending().await;
    }

    fn seal(&mut self, partial: bool) {
        if self.pending.is_some() || self.encoding.is_some() || self.job.is_some() {
            return;
        }
        if let Some(mut taken) = self.buffer.take(partial) {
            tracing::debug!(events = taken.len, "sealing relay batch");
            taken.batch.dropped = self.spool.take_dropped();
            self.encoding = Some(Encoding {
                encoder: BatchEncoder::new(
                    taken.batch,
                    self.body_limit,
                    self.decoded_limit,
                    self.model_limit,
                ),
                _reservations: taken.reservations,
            });
        }
    }

    async fn next_part(&mut self) -> bool {
        if self.pending.is_some() {
            return true;
        }
        if self.job.is_none() {
            let Some(mut encoding) = self.encoding.take() else {
                return false;
            };
            self.job = Some(tokio::task::spawn_blocking(move || {
                let part = encoding.encoder.next();
                let dropped = encoding.encoder.take_dropped();
                let encoding = if encoding.encoder.finished() {
                    None
                } else {
                    Some(encoding)
                };
                Encoded {
                    encoding,
                    part,
                    dropped,
                }
            }));
        }
        let result = self.job.as_mut().unwrap().await;
        self.job = None;
        match result {
            Ok(encoded) => {
                self.encoding = encoded.encoding;
                self.spool.record_dropped(encoded.dropped);
                self.pending = encoded.part.map(|part| Pending {
                    body: part.body.into(),
                    events: part.events,
                });
                self.pending.is_some()
            }
            Err(error) => {
                self.resources.stop();
                error!(%error, "encoding worker failed; ingest stopped");
                false
            }
        }
    }

    async fn save_pending(&mut self) {
        while self.next_part().await {
            let pending = self.pending.as_ref().unwrap();
            self.spool.push(&pending.body, pending.events as u64).await;
            self.pending = None;
        }
    }
}

struct Core {
    client: Client,
    url: Url,
    token: String,
}

impl Core {
    /// Отправляет пачку. Возвращает `false`, если core недоступен и пачку нужно повторить.
    async fn deliver(&self, body: Bytes) -> bool {
        match self.post(body).await {
            Ok(status) if status.is_success() => true,
            Ok(status) if is_permanent(status) => {
                // Повтор не поможет, а застрявшая пачка остановила бы всю очередь.
                error!(%status, "core rejected batch, dropping it");
                true
            }
            Ok(status) => {
                warn!(%status, "core is unavailable");
                false
            }
            Err(err) => {
                warn!(error = %err, "core is unavailable");
                false
            }
        }
    }

    async fn post(&self, body: Bytes) -> reqwest::Result<StatusCode> {
        let response = self
            .client
            .post(self.url.clone())
            .bearer_auth(&self.token)
            .header(CONTENT_TYPE, "application/msgpack")
            .header(CONTENT_ENCODING, "zstd")
            .body(body)
            .send()
            .await?;
        Ok(response.status())
    }
}

/// Ошибки клиента окончательны, кроме таймаута, лимита запросов и доступа:
/// неверный токен исправляют конфигурацией, и пачки за это время терять нельзя.
fn is_permanent(status: StatusCode) -> bool {
    status.is_client_error()
        && !matches!(
            status,
            StatusCode::REQUEST_TIMEOUT
                | StatusCode::TOO_MANY_REQUESTS
                | StatusCode::UNAUTHORIZED
                | StatusCode::FORBIDDEN
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_unfixable_client_errors_are_permanent() {
        for status in [StatusCode::BAD_REQUEST, StatusCode::PAYLOAD_TOO_LARGE] {
            assert!(is_permanent(status), "{status}");
        }
        for status in [
            StatusCode::UNAUTHORIZED,
            StatusCode::FORBIDDEN,
            StatusCode::REQUEST_TIMEOUT,
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::INTERNAL_SERVER_ERROR,
            StatusCode::SERVICE_UNAVAILABLE,
        ] {
            assert!(!is_permanent(status), "{status}");
        }
    }
}
