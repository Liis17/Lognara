//! lognara-relay: принимает пачки логов от lognara-agent, разбирает записи в события,
//! группирует их по источнику и сжатыми пачками отправляет в lognara-core.

pub mod agent_wire;
pub mod config;
pub mod core_wire;
pub mod model_budget;
pub mod normalize;
pub mod spool;

mod ingest;
mod memory;
mod sender;
mod wire_budget;

use std::io;

use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing::error;

use crate::config::Config;
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
    let sender = tokio::spawn(
        Sender::new(
            &config,
            spool.clone(),
            stop_sending.clone(),
            resources.clone(),
        )
        .run(),
    );

    let served = axum::serve(
        listener,
        ingest::router(spool, &config.relay_token, resources.clone()),
    )
    .with_graceful_shutdown(shutdown.cancelled_owned())
    .await;

    resources.drain().await;
    stop_sending.cancel();
    if let Err(err) = sender.await {
        error!(error = %err, "sender task failed");
    }
    served
}
