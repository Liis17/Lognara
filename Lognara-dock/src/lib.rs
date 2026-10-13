//! lognara-dock: веб-интерфейс Lognara за nginx. Проверяет вход по htpasswd и passkey,
//! хранит сессии в SQLite и показывает логи core в реальном времени.

pub mod account;
pub mod api;
pub mod auth;
pub mod config;
pub mod htpasswd;
pub mod passkey;
pub mod session;
pub mod store;

use std::io;

use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use crate::api::AppState;

/// Обслуживает HTTP на `listener`, пока не отменён `shutdown`.
pub async fn run(
    app: AppState,
    listener: TcpListener,
    shutdown: CancellationToken,
) -> io::Result<()> {
    axum::serve(listener, api::router(app))
        .with_graceful_shutdown(shutdown.cancelled_owned())
        .await
}
