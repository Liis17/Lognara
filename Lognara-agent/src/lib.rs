//! lognara-agent: HTTP, файлы и потоки программы → durable spool → relay.

pub mod config;
pub mod framing;
pub mod output;
pub mod sources;
pub mod wire;

mod buffer;
mod files;
mod heartbeat;
mod ingest;
mod process;
mod sender;

pub use buffer::BodyEncoder;
pub use heartbeat::TelemetrySnapshot;
pub use lognara_spool::Error as SpoolError;
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
use crate::wire::Record;

/// Очередь, файловые источники и отправка без HTTP-приёма и heartbeat агента.
/// Отправка идёт на `config.relay_url` с `config.relay_token`; тело готовит `encoder`.
pub struct Embedded {
    buffer: Arc<Buffer>,
    files: JoinHandle<()>,
    sender: JoinHandle<()>,
    stop_sources: CancellationToken,
    stop_sending: CancellationToken,
    _cancel_sources: DropGuard,
    _cancel_sender: DropGuard,
}

/// Запись собственных сборщиков в общую durable-очередь.
#[derive(Clone)]
pub struct Collector {
    buffer: Arc<Buffer>,
}

impl Embedded {
    pub async fn start(config: Config, encoder: Arc<dyn BodyEncoder>) -> io::Result<Self> {
        config.sources.validate(false)?;
        config.validate().map_err(io::Error::other)?;
        let buffer = Buffer::open_with(config.clone(), encoder)
            .await
            .map_err(io::Error::other)?;
        let stop_sources = CancellationToken::new();
        let files = files::Files::open(&config).await?;
        let files = tokio::spawn(files.run(buffer.clone(), stop_sources.clone()));
        let stop_sending = CancellationToken::new();
        let sender = tokio::spawn(Sender::new(&config, buffer.clone(), stop_sending.clone()).run());
        Ok(Self {
            buffer,
            files,
            sender,
            _cancel_sources: stop_sources.clone().drop_guard(),
            _cancel_sender: stop_sending.clone().drop_guard(),
            stop_sources,
            stop_sending,
        })
    }

    pub fn collector(&self) -> Collector {
        Collector {
            buffer: self.buffer.clone(),
        }
    }

    pub fn telemetry(&self) -> TelemetrySnapshot {
        TelemetrySnapshot::of(&self.buffer)
    }

    /// Останавливает файлы, выполняет финальную отправку и ждёт дисковых работников.
    /// Сборщики `Collector` должны быть остановлены до вызова.
    pub async fn finish(self) {
        self.stop_sources.cancel();
        if let Err(error) = self.files.await {
            error!(error = %error, "file collector task failed");
        }
        self.stop_sending.cancel();
        if let Err(error) = self.sender.await {
            error!(error = %error, "sender task failed");
        }
        self.buffer.drain().await;
    }
}

impl Collector {
    /// Атомарно сохраняет записи под общим admission источников. `Full` - очередь
    /// переполнена или отправка приостановлена; повтор или потеря решает вызывающий.
    /// Каждая запись не больше `max_record_bytes`.
    pub async fn save(&self, records: Vec<Record>) -> Result<(), SpoolError> {
        if records.is_empty() {
            return Ok(());
        }
        let admission = self.buffer.wait_source().await?;
        self.buffer.save_source(records, admission).await
    }

    pub fn max_record_bytes(&self) -> usize {
        self.buffer.source_record_limit()
    }

    /// Очередь заполнена на 75%: сборщики с собственной позицией ждут доставки.
    pub fn pressure(&self) -> bool {
        self.buffer.file_pressure()
    }
}

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
    heartbeat: JoinHandle<()>,
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
        let heartbeat = tokio::spawn(heartbeat::run(
            config.clone(),
            buffer.clone(),
            stop_sending.clone(),
        ));
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
            heartbeat,
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
        if let Err(error) = self.heartbeat.await {
            error!(error = %error, "heartbeat task failed");
        }
        self.buffer.drain().await;
        served
    }
}
