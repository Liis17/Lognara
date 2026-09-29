use lognara_core::{api, config::Config, storage::Core};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let config = Config::from_env()?;
    let addr = config.listen_addr;
    let core = tokio::task::spawn_blocking(move || Core::open(config)).await??;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(address = %listener.local_addr()?, "core listening");
    axum::serve(listener, api::router(core.clone()))
        .with_graceful_shutdown(shutdown())
        .await?;
    tokio::task::spawn_blocking(move || core.shutdown()).await??;
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
