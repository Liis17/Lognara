//! lognara-relay: принимает пачки логов от lognara-agent, разбирает записи в события,
//! группирует их по источнику и сжатыми пачками отправляет в lognara-core.

pub mod agent_wire;
pub mod config;
pub mod core_wire;
pub mod model_budget;
pub mod normalize;
pub mod spool;

mod agents;
mod heartbeat;
mod ingest;
mod memory;
mod sender;
mod wire_budget;

use std::io;
use std::sync::Arc;

use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing::error;

use crate::agents::{AgentSync, Agents, SYNC_INTERVAL};
use crate::config::Config;
use crate::heartbeat::Telemetry;
use crate::sender::Sender;
use crate::spool::Spool;

/// Принимает пачки агентов на `listener` и отправляет события в core, пока не отменён
/// `shutdown`. После отмены доставляет или сохраняет в `spool` всё накопленное.
pub async fn run(
    config: Config,
    listener: TcpListener,
    mut spool: Spool,
    shutdown: CancellationToken,
) -> io::Result<()> {
    config
        .validate_memory()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    let resources = memory::Resources::new(&config);
    let replay_limit = config.sender_bytes().unwrap() - memory::CODEC_WORKSPACE;
    spool.set_replay_limit(replay_limit);
    resources.set_ready(spool.fits_replay_limit());
    spool.set_max_events(config.max_buffer);
    // Отправка останавливается только после приёма, чтобы последние события не потерялись.
    let stop_sending = CancellationToken::new();
    let telemetry = Arc::new(Telemetry::default());
    let sender = tokio::spawn(
        Sender::new(
            &config,
            spool.clone(),
            stop_sending.clone(),
            resources.clone(),
            telemetry.clone(),
        )
        .run(),
    );

    // Приём начинается с кеша и одной синхронизации; дальше список обновляется в фоне.
    let agents = Arc::new(Agents::default());
    let mut agent_sync = AgentSync::new(
        &config.core_url,
        &config.core_token,
        spool.dir(),
        agents.clone(),
    );
    agent_sync.load_cache().await;
    tokio::select! {
        _ = shutdown.cancelled() => {}
        _ = agent_sync.sync() => {}
    }
    let stop_agents = shutdown.child_token();
    let agent_task = tokio::spawn(agent_sync.run(SYNC_INTERVAL, stop_agents.clone()));
    let heartbeat_task = tokio::spawn({
        let (telemetry, agents, spool, config) = (
            telemetry.clone(),
            agents.clone(),
            spool.clone(),
            config.clone(),
        );
        let stop = stop_agents.clone();
        async move { heartbeat::run(telemetry, agents, spool, &config, stop).await }
    });

    let router = ingest::router(spool, agents, resources.clone(), telemetry);
    let served = axum::serve(listener, router)
        .with_graceful_shutdown(shutdown.cancelled_owned())
        .await;

    stop_agents.cancel();
    if let Err(err) = agent_task.await {
        error!(error = %err, "agent sync task failed");
    }
    if let Err(err) = heartbeat_task.await {
        error!(error = %err, "heartbeat task failed");
    }
    resources.drain().await;
    stop_sending.cancel();
    if let Err(err) = sender.await {
        error!(error = %err, "sender task failed");
    }
    served
}
