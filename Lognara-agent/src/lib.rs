//! lognara-agent: принимает логи приложения по HTTP, копит их в оперативной памяти
//! и сжатыми пачками отправляет в lognara-relay.

pub mod config;
pub mod wire;

mod buffer;
mod ingest;
mod sender;

use std::io;
use std::sync::Arc;

use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing::error;

use crate::buffer::Buffer;
use crate::config::Config;
use crate::sender::Sender;

/// Принимает логи на `listener` и отправляет их в relay, пока не отменён `shutdown`.
/// После отмены дожидается финальной отправки накопленного.
pub async fn run(
    config: Config,
    listener: TcpListener,
    shutdown: CancellationToken,
) -> io::Result<()> {
    let buffer = Arc::new(Buffer::new(config.batch_size, config.max_buffer));
    // Отправка останавливается только после приёма, чтобы последние записи попали в финальную пачку.
    let stop_sending = CancellationToken::new();
    let sender = tokio::spawn(Sender::new(&config, buffer.clone(), stop_sending.clone()).run());

    let served = axum::serve(listener, ingest::router(buffer))
        .with_graceful_shutdown(shutdown.cancelled_owned())
        .await;

    stop_sending.cancel();
    if let Err(err) = sender.await {
        error!(error = %err, "sender task failed");
    }
    served
}
