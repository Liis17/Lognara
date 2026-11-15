//! lognara-hwagent: метрики, процессы и логи Linux-хоста → durable spool → core.
//!
//! Очередь, файловые источники и отправку даёт `lognara_agent::Embedded`; пачка
//! нормализуется в формат core (`encoder`) и уходит прямо в core с токеном хоста.

pub mod config;
pub mod encoder;
pub mod heartbeat;
pub mod journald;
pub mod metrics;
pub mod processes;

use std::io;
use std::sync::Arc;

use lognara_agent::Embedded;
use tokio::task::JoinSet;
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
    let mut tasks = JoinSet::new();
    tasks.spawn({
        let embedded = embedded.clone();
        heartbeat::run(config.clone(), move || embedded.telemetry(), stop.clone())
    });
    if config.host.metrics.enabled {
        tasks.spawn(metrics::run(
            config.root.clone(),
            config.host.metrics.clone(),
            embedded.collector(),
            stop.clone(),
        ));
    }
    if config.host.processes.enabled {
        tasks.spawn(processes::run(
            config.root.clone(),
            config.host.processes.clone(),
            config.state_dir.join("executables.json"),
            embedded.collector(),
            stop.clone(),
        ));
    }
    if config.host.journald.enabled {
        tasks.spawn(journald::run(
            config.host.journald.clone(),
            config.state_dir.join("journal.cursor"),
            embedded.collector(),
            stop.clone(),
        ));
    }
    shutdown.cancelled().await;
    // Сборщики останавливаются до финальной отправки: всё сохранённое уйдёт или останется в spool.
    stop.cancel();
    while tasks.join_next().await.is_some() {}
    match Arc::into_inner(embedded) {
        Some(embedded) => embedded.finish().await,
        None => return Err(io::Error::other("collector still holds the queue")),
    }
    Ok(())
}
