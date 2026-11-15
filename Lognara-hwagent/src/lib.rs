//! lognara-hwagent: метрики, процессы и логи Linux-хоста → durable spool → core.
//!
//! Очередь, файловые источники и отправку даёт `lognara_agent::Embedded`; пачка
//! нормализуется в формат core (`encoder`) и уходит прямо в core с токеном хоста.

pub mod config;
pub mod encoder;
pub mod heartbeat;

use std::io;
use std::sync::Arc;

use lognara_agent::Embedded;
use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::encoder::CoreEncoder;

/// Собирает и отправляет до отмены `shutdown`, затем делает финальную отправку.
pub async fn run(config: Config, shutdown: CancellationToken) -> io::Result<()> {
    let embedded = Arc::new(
        Embedded::start(
            config.agent.clone(),
            Arc::new(CoreEncoder::new(&config.token)),
        )
        .await?,
    );
    let stop = CancellationToken::new();
    let heartbeat = tokio::spawn({
        let embedded = embedded.clone();
        heartbeat::run(config.clone(), move || embedded.telemetry(), stop.clone())
    });
    shutdown.cancelled().await;
    stop.cancel();
    let _ = heartbeat.await;
    match Arc::into_inner(embedded) {
        Some(embedded) => embedded.finish().await,
        None => return Err(io::Error::other("collector still holds the queue")),
    }
    Ok(())
}
