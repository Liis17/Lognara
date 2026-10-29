use std::process::ExitCode;
use tracing_subscriber::prelude::*;

use lognara_relay::config::Config;
use lognara_relay::spool::Spool;
use tokio::net::TcpListener;
use tokio::signal::unix::{SignalKind, signal};
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

#[tokio::main]
async fn main() -> ExitCode {
    let collector = lognara_selflog::Collector::new("lognara-relay");
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with(tracing_subscriber::fmt::layer().with_target(false))
        .with(collector.layer())
        .init();

    let config = match Config::from_env() {
        Ok(config) => config,
        Err(err) => {
            error!("{err}");
            return ExitCode::FAILURE;
        }
    };
    let logs = match collector.start(
        &config.spool_dir.join("self-log-spool"),
        lognara_selflog::Remote::new(config.core_url.clone(), config.core_token.clone(), true),
    ) {
        Ok(logs) => Some(logs),
        Err(error) => {
            eprintln!("self-log worker unavailable: {error}");
            None
        }
    };
    let spool = match Spool::open(&config.spool_dir, config.spool_max_bytes).await {
        Ok(spool) => spool,
        Err(err) => {
            error!("failed to open spool {}: {err}", config.spool_dir.display());
            return ExitCode::FAILURE;
        }
    };
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
        core = %config.core_url,
        spool = %config.spool_dir.display(),
        spooled_batches = spool.len(),
        "lognara-relay started"
    );
    let result = match lognara_relay::run(config, listener, spool, shutdown).await {
        Ok(()) => {
            info!("lognara-relay stopped");
            ExitCode::SUCCESS
        }
        Err(err) => {
            error!("server failed: {err}");
            ExitCode::FAILURE
        }
    };
    if let Some(logs) = logs {
        let _ = tokio::task::spawn_blocking(move || logs.stop()).await;
    }
    result
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
