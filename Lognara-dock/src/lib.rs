//! lognara-dock: веб-интерфейс Lognara за nginx. Проверяет вход по htpasswd и passkey,
//! хранит сессии в SQLite и показывает логи core в реальном времени.

pub mod access;
pub mod account;
pub mod api;
pub mod auth;
pub mod config;
pub mod core_client;
pub mod htpasswd;
pub mod infra;
pub mod issues;
pub mod live;
pub mod passkey;
pub mod patterns;
pub mod query;
pub mod self_logs;
pub mod series;
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
    let stop = app.shutdown.clone();
    tokio::spawn(issues::watch(app.clone()));
    axum::serve(listener, api::router(app))
        .with_graceful_shutdown(async move {
            shutdown.cancelled().await;
            // SSE-потоки бесконечны: без отмены graceful shutdown ждал бы их вечно.
            stop.cancel();
        })
        .await
}
