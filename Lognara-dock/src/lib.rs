//! lognara-dock: веб-интерфейс Lognara за nginx. Проверяет вход по htpasswd и passkey,
//! хранит сессии в SQLite и показывает логи core в реальном времени.

pub mod api;
pub mod config;

use std::io;

use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use crate::config::Config;

/// Обслуживает HTTP на `listener`, пока не отменён `shutdown`.
pub async fn run(
    _config: Config,
    listener: TcpListener,
    shutdown: CancellationToken,
) -> io::Result<()> {
    axum::serve(listener, api::router())
        .with_graceful_shutdown(shutdown.cancelled_owned())
        .await
}
