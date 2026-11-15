use std::process::ExitCode;

use lognara_hwagent::config::Config;
use tokio::signal::unix::{SignalKind, signal};
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();
    let config = match Config::from_env() {
        Ok(config) => config,
        Err(error) => {
            error!(%error, "invalid configuration");
            return ExitCode::FAILURE;
        }
    };
    let shutdown = CancellationToken::new();
    let (Ok(mut term), Ok(mut int)) = (
        signal(SignalKind::terminate()),
        signal(SignalKind::interrupt()),
    ) else {
        error!("signal handlers are unavailable");
        return ExitCode::FAILURE;
    };
    tokio::spawn({
        let shutdown = shutdown.clone();
        async move {
            tokio::select! {
                _ = term.recv() => {}
                _ = int.recv() => {}
            }
            info!("shutdown requested");
            shutdown.cancel();
        }
    });
    info!(server = %config.agent.server, "lognara-hwagent started");
    match lognara_hwagent::run(config, shutdown).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            error!(%error, "lognara-hwagent failed");
            ExitCode::FAILURE
        }
    }
}
