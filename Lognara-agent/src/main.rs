use std::process::ExitCode;

use lognara_agent::config::Config;
use tokio::net::TcpListener;
use tokio::signal::unix::{SignalKind, signal};
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

#[tokio::main]
async fn main() -> ExitCode {
    let diagnostics = match lognara_agent::output::Console::stderr() {
        Ok(output) => output,
        Err(error) => {
            eprintln!("cannot create diagnostic output: {error}");
            return ExitCode::FAILURE;
        }
    };
    let writer = diagnostics.clone();
    tracing_subscriber::fmt()
        .with_target(false)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .init();
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    let capture = !arguments.is_empty();
    if capture && (arguments.len() < 3 || arguments[0] != "run" || arguments[1] != "--") {
        error!("usage: lognara-agent [run -- PROGRAM [ARGUMENTS...]]");
        diagnostics.drain().await;
        return ExitCode::from(2);
    }
    let code = start(arguments, diagnostics.clone()).await;
    diagnostics.drain().await;
    code
}

async fn start(
    arguments: Vec<std::ffi::OsString>,
    diagnostics: lognara_agent::output::Console,
) -> ExitCode {
    let capture = !arguments.is_empty();

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
    if let Err(err) = config.load_sources(sources_path.as_deref(), capture) {
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

    if capture {
        return match lognara_agent::run_program(
            config,
            listener,
            arguments.into_iter().skip(2).collect(),
            Some(diagnostics),
        )
        .await
        {
            Ok(code) => ExitCode::from(code),
            Err(error) => {
                error!(error = %error, "program runner failed");
                ExitCode::FAILURE
            }
        };
    }
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
