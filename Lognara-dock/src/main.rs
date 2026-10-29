use std::process::ExitCode;
use tracing_subscriber::prelude::*;

use lognara_dock::api::App;
use lognara_dock::config::Config;
use tokio::net::TcpListener;
use tokio::signal::unix::{SignalKind, signal};
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

#[tokio::main]
async fn main() -> ExitCode {
    let collector = lognara_selflog::Collector::new("lognara-dock");
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
        &config.data_dir.join("self-log-spool"),
        lognara_selflog::Remote::new(
            config.core_url.clone(),
            config.core_admin_token.clone(),
            false,
        ),
    ) {
        Ok(logs) => Some(logs),
        Err(error) => {
            eprintln!("self-log worker unavailable: {error}");
            None
        }
    };
    let app = match tokio::task::spawn_blocking({
        let config = config.clone();
        move || App::open(config)
    })
    .await
    .expect("startup task")
    {
        Ok(app) => app,
        Err(err) => {
            error!("{err}");
            if let Some(logs) = logs {
                let _ = tokio::task::spawn_blocking(move || logs.stop()).await;
            }
            return ExitCode::FAILURE;
        }
    };
    let listener = match TcpListener::bind(config.listen_addr).await {
        Ok(listener) => listener,
        Err(err) => {
            error!("failed to listen on {}: {err}", config.listen_addr);
            if let Some(logs) = logs {
                let _ = tokio::task::spawn_blocking(move || logs.stop()).await;
            }
            return ExitCode::FAILURE;
        }
    };

    let shutdown = CancellationToken::new();
    tokio::spawn(cancel_on_signal(shutdown.clone()));

    info!(
        listen = %config.listen_addr,
        public_url = %config.public_url,
        core = %config.core_url,
        htpasswd = %config.htpasswd_file.display(),
        data = %config.data_dir.display(),
        "lognara-dock started"
    );
    let result = match lognara_dock::run(app, listener, shutdown).await {
        Ok(()) => {
            info!("lognara-dock stopped");
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
