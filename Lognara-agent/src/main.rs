use std::process::ExitCode;

use lognara_agent::config::Config;
use tokio::net::TcpListener;
use tokio::signal::unix::{SignalKind, signal};
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt().with_target(false).init();

    let mut config = match Config::from_env() {
        Ok(config) => config,
        Err(err) => {
            error!("{err}");
            return ExitCode::FAILURE;
        }
    };
    let sources_path = std::env::var_os("LOGNARA_SOURCES_FILE")
        .filter(|path| !path.is_empty())
        .map(std::path::PathBuf::from);
    if let Err(err) = config.load_sources(sources_path.as_deref(), false) {
        error!("LOGNARA_SOURCES_FILE: {err}");
        return ExitCode::FAILURE;
    }
    let listener = match TcpListener::bind(config.listen_addr).await {
        Ok(listener) => listener,
        Err(err) => {
            error!("failed to listen on {}: {err}", config.listen_addr);
            return ExitCode::FAILURE;
        }
    };

    let shutdown = CancellationToken::new();
    tokio::spawn(cancel_on_signal(shutdown.clone()));

    info!(
        listen = %config.listen_addr,
        relay = %config.relay_url,
        service = %config.service,
        "lognara-agent started"
    );
    match lognara_agent::run(config, listener, shutdown).await {
        Ok(()) => {
            info!("lognara-agent stopped");
            ExitCode::SUCCESS
        }
        Err(err) => {
            error!("server failed: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Отменяет `shutdown` по SIGTERM (остановка контейнера) или SIGINT.
async fn cancel_on_signal(shutdown: CancellationToken) {
    let mut terminate = signal(SignalKind::terminate()).expect("failed to install SIGTERM handler");
    tokio::select! {
        _ = terminate.recv() => {}
        _ = tokio::signal::ctrl_c() => {}
    }
    info!("shutting down");
    shutdown.cancel();
}
