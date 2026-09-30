//! Отправка накопленных событий в lognara-core. Пока core недоступен, пачки ждут в spool.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use reqwest::header::{CONTENT_ENCODING, CONTENT_TYPE};
use reqwest::{Client, StatusCode, Url};
use tokio::time::{self, Instant, MissedTickBehavior};
use tokio_util::sync::CancellationToken;
use tracing::{error, warn};

use crate::buffer::Buffer;
use crate::config::Config;
use crate::core_wire::{self, CoreBatch};
use crate::spool::Spool;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Сколько после остановки ждать доставки накопленного.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

pub struct Sender {
    buffer: Arc<Buffer>,
    spool: Spool,
    core: Core,
    flush_interval: Duration,
    shutdown: CancellationToken,
    /// Уже закодированные части пачки, ещё не доставленные и не сохранённые в spool.
    pending: VecDeque<Pending>,
    body_limit: usize,
    decoded_limit: usize,
    model_limit: usize,
}

struct Pending {
    body: Bytes,
    events: usize,
}

impl Sender {
    pub fn new(
        config: &Config,
        buffer: Arc<Buffer>,
        spool: Spool,
        shutdown: CancellationToken,
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
            pending: VecDeque::new(),
            body_limit: config.core_max_body_bytes,
            decoded_limit: config.core_max_decoded_bytes,
            model_limit: config.core_max_model_bytes,
        }
    }

    /// Отправляет накопленное раз в `flush_interval` и сразу, как только набран
    /// `batch_size`. После остановки сохраняет недоставленное в spool и завершается.
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
            warn!("timed out delivering batches to core");
        }
        // Если ожидание прервано, недоставленная пачка и остаток памяти сохраняются на диск.
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

    /// Отправляет сначала пачки из spool, затем накопленное в памяти.
    /// При `partial = false` из памяти берётся только полная пачка.
    async fn flush(&mut self, partial: bool) {
        while let Some(body) = self.spool.oldest().await {
            if !self.core.deliver(body.into()).await {
                break;
            }
            self.spool.remove_oldest().await;
            // Пока разгружается бэклог, полные пачки из памяти встают в конец очереди.
            self.seal(false);
            self.save_pending().await;
        }

        self.seal(partial);
        while let Some(pending) = self.pending.front()
            && self.spool.is_empty()
        {
            if !self.core.deliver(pending.body.clone()).await {
                break;
            }
            self.pending.pop_front();
        }
        // Core недоступен или в spool остались старые пачки: новая ждёт своей очереди.
        self.save_pending().await;
    }

    /// Собирает пачку из памяти, если прежняя уже доставлена или сохранена.
    fn seal(&mut self, partial: bool) {
        if !self.pending.is_empty() {
            return;
        }
        if let Some((groups, _)) = self.buffer.take(partial) {
            let batch = CoreBatch {
                dropped: self.spool.take_dropped(),
                groups,
            };
            let (batches, dropped) = core_wire::encode_split(
                batch,
                self.body_limit,
                self.decoded_limit,
                self.model_limit,
            );
            self.spool.record_dropped(dropped);
            self.pending
                .extend(batches.into_iter().map(|batch| Pending {
                    body: batch.body.into(),
                    events: batch.events,
                }));
        }
    }

    async fn save_pending(&mut self) {
        while let Some(pending) = self.pending.front() {
            self.spool.push(&pending.body, pending.events as u64).await;
            self.pending.pop_front();
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
