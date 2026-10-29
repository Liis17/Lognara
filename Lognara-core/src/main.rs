use lognara_core::{api, config::Config, storage::Core};
use tracing_subscriber::prelude::*;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let collector = lognara_selflog::Collector::new("lognara-core");
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .with(collector.layer())
        .init();
    let config = Config::from_env()?;
    let addr = config.listen_addr;
    let core = tokio::task::spawn_blocking(move || Core::open(config)).await??;
    let log_dir = core.journal.config.data_dir.join("self-log-spool");
    let logs = collector.start(&log_dir, lognara_core::self_logs::Local(core.clone()))?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(address = %listener.local_addr()?, "core listening");
    axum::serve(listener, api::router(core.clone()))
        .with_graceful_shutdown(shutdown())
        .await?;
    let result = tokio::task::spawn_blocking(move || core.shutdown()).await?;
    tokio::task::spawn_blocking(move || logs.stop()).await?;
    result?;
    Ok(())
}

async fn shutdown() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler");
        tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = term.recv() => {} }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}
