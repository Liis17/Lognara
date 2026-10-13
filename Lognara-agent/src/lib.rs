//! lognara-agent: HTTP, файлы и потоки программы → durable spool → relay.

pub mod config;
pub mod framing;
pub mod output;
pub mod sources;
pub mod wire;

mod buffer;
mod files;
mod ingest;
mod process;
mod sender;

pub use process::run_program;

use std::io;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_util::sync::{CancellationToken, DropGuard};
use tracing::error;

use crate::buffer::Buffer;
use crate::config::Config;
use crate::sender::Sender;

/// Принимает HTTP и файлы до отмены shutdown; затем дожидается сохранения и отправки.
pub async fn run(
    config: Config,
    listener: TcpListener,
    shutdown: CancellationToken,
) -> io::Result<()> {
    config.sources.validate(false)?;
    let mut runtime = Runtime::start(config, listener, shutdown).await?;
    let result = runtime.wait().await;
    runtime.finish().await?;
    result
}

struct Runtime {
    buffer: Arc<Buffer>,
    server: Option<JoinHandle<io::Result<()>>>,
    files: JoinHandle<()>,
    sender: JoinHandle<()>,
    shutdown: CancellationToken,
    stop_sending: CancellationToken,
    _cancel_sources: DropGuard,
    _cancel_sender: DropGuard,
}

impl Runtime {
    async fn start(
        config: Config,
        listener: TcpListener,
        shutdown: CancellationToken,
    ) -> io::Result<Self> {
        config.validate().map_err(io::Error::other)?;
        let buffer = Buffer::open(config.clone())
            .await
            .map_err(io::Error::other)?;
        let files = files::Files::open(&config).await?;
        let file_task = tokio::spawn(files.run(buffer.clone(), shutdown.clone()));
        let stop_sending = CancellationToken::new();
        let sender = tokio::spawn(Sender::new(&config, buffer.clone(), stop_sending.clone()).run());
        let app = ingest::router(buffer.clone());
        let cancellation = shutdown.clone();
        let server = tokio::spawn(async move {
            let result = axum::serve(listener, app)
                .with_graceful_shutdown(cancellation.cancelled_owned())
                .await;
            if let Err(error) = &result {
                error!(error = %error, "HTTP server failed");
            }
            result
        });
        Ok(Self {
            buffer,
            server: Some(server),
            files: file_task,
            sender,
            _cancel_sources: shutdown.clone().drop_guard(),
            _cancel_sender: stop_sending.clone().drop_guard(),
            shutdown,
            stop_sending,
        })
    }

    async fn wait(&mut self) -> io::Result<()> {
        match self.server.take() {
            Some(server) => server.await.map_err(io::Error::other)?,
            None => Ok(()),
        }
    }

    async fn finish(mut self) -> io::Result<()> {
        self.shutdown.cancel();
        let served = self.wait().await;
        if let Err(error) = self.files.await {
            error!(error = %error, "file collector task failed");
        }
        self.stop_sending.cancel();
        if let Err(error) = self.sender.await {
            error!(error = %error, "sender task failed");
        }
        self.buffer.drain().await;
        served
    }
}
