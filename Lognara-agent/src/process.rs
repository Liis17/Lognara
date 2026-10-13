//! Запуск foreground-программы; чтение pipe независимо от диска и консоли.
use std::ffi::OsString;
use std::io::{self, IsTerminal};
use std::os::unix::process::ExitStatusExt;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::net::TcpListener;
use tokio::process::Command;
use tokio::signal::unix::{Signal, SignalKind, signal};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::Runtime;
use crate::buffer::Buffer;
use crate::config::Config;
use crate::framing::{Framed, Framer};
use crate::output::Console;
use crate::sources::{Source, SourceKind};

const READ_BYTES: usize = 64 << 10;
const RAW_SLOTS: usize = 8;

#[derive(Default)]
struct Losses {
    bytes: AtomicU64,
    events: AtomicU64,
    fragments: AtomicU64,
    gaps: AtomicU64,
    chunks: AtomicU64,
    generation: AtomicU64,
    cutoff: AtomicU64,
}

impl Losses {
    fn bytes(&self, count: usize) {
        self.bytes.fetch_add(count as u64, Ordering::Relaxed);
    }
    fn report(&self, source: &str, console: &Console) {
        let bytes = self.bytes.load(Ordering::Relaxed);
        let events = self.events.load(Ordering::Relaxed);
        let fragments = self.fragments.load(Ordering::Relaxed);
        let gaps = self.gaps.load(Ordering::Relaxed);
        let chunks = self.chunks.load(Ordering::Relaxed);
        let cutoff = self.cutoff.load(Ordering::Relaxed);
        let echo_bytes = console.dropped_bytes();
        if bytes + events + fragments + gaps + chunks + cutoff + echo_bytes > 0 {
            warn!(
                source,
                dropped_bytes = bytes,
                dropped_events = events,
                dropped_fragments = fragments,
                stream_gaps = gaps,
                dropped_raw_chunks = chunks,
                shutdown_cutoffs = cutoff,
                echo_dropped_bytes = echo_bytes,
                "stream collection losses; unread bytes at shutdown cutoff are unknown"
            );
        }
    }
}

struct Raw {
    bytes: Vec<u8>,
    offset: u64,
    generation: u64,
}

struct Capture {
    reader: JoinHandle<()>,
    processor: JoinHandle<()>,
    losses: Arc<Losses>,
    console: Console,
    source: String,
}

impl Capture {
    fn start<R: AsyncRead + Unpin + Send + 'static>(
        reader: R,
        source: Source,
        buffer: Arc<Buffer>,
        stop: CancellationToken,
        console: Console,
    ) -> io::Result<Self> {
        let framer = Framer::new(&source, buffer.source_record_limit(), 0, None)?;
        let losses = Arc::new(Losses::default());
        let (sender, receiver) = mpsc::channel(RAW_SLOTS);
        let reader = tokio::spawn(read_pipe(
            reader,
            sender,
            losses.clone(),
            console.clone(),
            stop,
            source.id.clone(),
        ));
        let processor = tokio::spawn(collect(
            receiver,
            framer,
            source.clone(),
            buffer,
            losses.clone(),
        ));
        Ok(Self {
            reader,
            processor,
            losses,
            console,
            source: source.id,
        })
    }

    async fn finish(mut self, deadline: Instant, stop: &CancellationToken) {
        if tokio::time::timeout_at(deadline, &mut self.reader)
            .await
            .is_err()
        {
            self.losses.cutoff.fetch_add(1, Ordering::Relaxed);
            stop.cancel();
            let _ = self.reader.await;
        }
        if let Err(error) = self.processor.await {
            warn!(error = %error, "stream processor failed");
        }
        self.console.drain().await;
        self.losses.report(&self.source, &self.console);
    }
}

async fn read_pipe<R: AsyncRead + Unpin>(
    mut reader: R,
    sender: mpsc::Sender<Raw>,
    losses: Arc<Losses>,
    console: Console,
    stop: CancellationToken,
    source: String,
) {
    let mut chunk = [0u8; READ_BYTES];
    let mut offset = 0;
    let mut report = Instant::now();
    loop {
        let result = tokio::select! { _ = stop.cancelled() => break, result = reader.read(&mut chunk) => result };
        let count = match result {
            Ok(0) => break,
            Ok(count) => count,
            Err(error) => {
                warn!(error = %error, "pipe read failed; retaining reader while program is alive");
                tokio::select! { _ = stop.cancelled() => break, _ = tokio::time::sleep(Duration::from_millis(100)) => continue }
            }
        };
        console.echo(&chunk[..count]);
        let raw = Raw {
            bytes: chunk[..count].to_vec(),
            offset,
            generation: losses.generation.load(Ordering::Relaxed),
        };
        if sender.try_send(raw).is_err() {
            losses.bytes(count);
            losses.chunks.fetch_add(1, Ordering::Relaxed);
            losses.generation.fetch_add(1, Ordering::Relaxed);
        }
        offset += count as u64;
        if report.elapsed() >= Duration::from_secs(10) {
            losses.report(&source, &console);
            report = Instant::now();
        }
    }
}

async fn collect(
    mut receiver: mpsc::Receiver<Raw>,
    mut framer: Framer,
    source: Source,
    buffer: Arc<Buffer>,
    losses: Arc<Losses>,
) {
    let mut generation = 0;
    let mut position = 0;
    let mut skip_tail = false;
    let mut last_data = Instant::now();
    let mut invalid_warning = Instant::now() - Duration::from_secs(10);
    loop {
        let timeout = source
            .multiline
            .as_ref()
            .map_or(1000, |m| m.flush_timeout_ms);
        let raw = tokio::select! {
            raw = receiver.recv() => raw,
            _ = tokio::time::sleep(Duration::from_millis(timeout)) => {
                if last_data.elapsed() >= Duration::from_millis(timeout) && let Some(frame) = framer.idle()
                    && !save_frames(vec![frame], &source, &buffer, &losses, &mut invalid_warning).await {
                        losses.bytes(framer.pending_bytes());
                        framer = Framer::new(&source, buffer.source_record_limit(), position, None).expect("validated source"); skip_tail = true;
                }
                continue;
            }
        };
        let Some(raw) = raw else {
            break;
        };
        if raw.offset != position || raw.generation != generation {
            losses.bytes(framer.pending_bytes());
            losses.gaps.fetch_add(1, Ordering::Relaxed);
            framer = Framer::new(&source, buffer.source_record_limit(), raw.offset, None)
                .expect("validated source");
            skip_tail = true;
        }
        generation = raw.generation;
        last_data = Instant::now();
        for chunk in raw.bytes.chunks(4096) {
            let mut bytes = chunk;
            if skip_tail {
                if let Some(end) = bytes.iter().position(|&byte| byte == b'\n') {
                    losses.bytes(end + 1);
                    position = position.max(raw.offset) + (end + 1) as u64;
                    bytes = &bytes[end + 1..];
                    skip_tail = false;
                    framer = Framer::new(&source, buffer.source_record_limit(), position, None)
                        .expect("validated source");
                } else {
                    losses.bytes(bytes.len());
                    position = position.max(raw.offset) + bytes.len() as u64;
                    continue;
                }
            }
            let frames = framer.feed(bytes);
            position = position.max(raw.offset) + bytes.len() as u64;
            if !save_frames(frames, &source, &buffer, &losses, &mut invalid_warning).await {
                losses.bytes(framer.pending_bytes());
                losses.gaps.fetch_add(1, Ordering::Relaxed);
                framer = Framer::new(&source, buffer.source_record_limit(), position, None)
                    .expect("validated source");
                skip_tail = true;
            }
        }
        position = raw.offset + raw.bytes.len() as u64;
    }
    if losses.generation.load(Ordering::Relaxed) != generation || skip_tail {
        if losses.generation.load(Ordering::Relaxed) != generation {
            losses.gaps.fetch_add(1, Ordering::Relaxed);
        }
        losses.bytes(framer.pending_bytes());
    } else {
        let frames = framer.finish_all();
        let _ = save_frames(frames, &source, &buffer, &losses, &mut invalid_warning).await;
    }
}

async fn save_frames(
    frames: Vec<Framed>,
    source: &Source,
    buffer: &Arc<Buffer>,
    losses: &Losses,
    invalid_warning: &mut Instant,
) -> bool {
    let mut frames = frames.into_iter();
    loop {
        let group: Vec<_> = frames
            .by_ref()
            .take(32.min(buffer.record_limit()).max(1))
            .collect();
        if group.is_empty() {
            return true;
        }
        let events = group
            .iter()
            .filter(|frame| frame.fragment == 0 && !frame.continued)
            .count();
        let fragments = group.len() - events;
        let bytes: u64 = group.iter().map(|frame| frame.end - frame.offset).sum();
        if group.iter().any(|frame| frame.invalid_json)
            && invalid_warning.elapsed() >= Duration::from_secs(10)
        {
            warn!(source = source.id, "invalid NDJSON preserved as text");
            *invalid_warning = Instant::now();
        }
        let admission = tokio::time::timeout(Duration::from_millis(100), buffer.wait_source())
            .await
            .unwrap_or(Err(lognara_spool::Error::Full));
        let result = if let Ok(admission) = admission {
            buffer
                .save_source(
                    group
                        .into_iter()
                        .map(|frame| frame.into_record(source, None))
                        .collect(),
                    admission,
                )
                .await
        } else {
            admission.map(|_| ())
        };
        if let Err(error) = result {
            losses.bytes.fetch_add(bytes, Ordering::Relaxed);
            losses.events.fetch_add(events as u64, Ordering::Relaxed);
            losses
                .fragments
                .fetch_add(fragments as u64, Ordering::Relaxed);
            for frame in frames {
                losses
                    .bytes
                    .fetch_add(frame.end - frame.offset, Ordering::Relaxed);
                if frame.fragment == 0 && !frame.continued {
                    losses.events.fetch_add(1, Ordering::Relaxed);
                } else {
                    losses.fragments.fetch_add(1, Ordering::Relaxed);
                }
            }
            // Full является ожидаемой перегрузкой; остальные ошибки также не закрывают pipe.
            if !matches!(error, lognara_spool::Error::Full) {
                warn!(error = %error, "stream append failed; reader continues");
            }
            return false;
        }
    }
}

struct Signals(Vec<(i32, Signal)>);
impl Signals {
    fn new() -> io::Result<Self> {
        [
            libc::SIGTERM,
            libc::SIGINT,
            libc::SIGQUIT,
            libc::SIGHUP,
            libc::SIGUSR1,
            libc::SIGUSR2,
        ]
        .into_iter()
        .map(|number| signal(SignalKind::from_raw(number)).map(|stream| (number, stream)))
        .collect::<io::Result<Vec<_>>>()
        .map(Self)
    }
    fn poll(&mut self, context: &mut Context<'_>) -> Poll<i32> {
        for (number, stream) in &mut self.0 {
            if stream.poll_recv(context).is_ready() {
                return Poll::Ready(*number);
            }
        }
        Poll::Pending
    }
    async fn next(&mut self) -> i32 {
        std::future::poll_fn(|context| self.poll(context)).await
    }
    fn pending(&mut self) -> Option<i32> {
        match self.poll(&mut Context::from_waker(Waker::noop())) {
            Poll::Ready(number) => Some(number),
            Poll::Pending => None,
        }
    }
}

struct Group(i32);
impl Group {
    fn send(&self, number: i32, group: bool) {
        // SAFETY: kill receives a numeric PID/PGID and no pointers.
        unsafe {
            libc::kill(if group { -self.0 } else { self.0 }, number);
        }
    }
}
impl Drop for Group {
    fn drop(&mut self) {
        if self.0 > 0 {
            self.send(libc::SIGKILL, true);
        }
    }
}

/// Запускает одну программу без shell; возвращает её код после сохранения логов.
pub async fn run_program(
    mut config: Config,
    listener: TcpListener,
    arguments: Vec<OsString>,
    diagnostics: Option<Console>,
) -> io::Result<u8> {
    if arguments.is_empty() || io::stdin().is_terminal() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "run requires a foreground command and non-TTY stdin",
        ));
    }
    for kind in [SourceKind::Stdout, SourceKind::Stderr] {
        if config.sources.stream(kind).is_none() {
            config.sources.sources.push(Source::stream(kind));
        }
    }
    config.sources.validate(true)?;
    let mut signals = Signals::new()?;
    let stdout = Console::stdout()?;
    let stderr = Console::stderr()?;
    let runtime = Runtime::start(config.clone(), listener, CancellationToken::new()).await?;
    stdout.reserve(runtime.buffer.source_memory.clone());
    stderr.reserve(runtime.buffer.source_memory.clone());
    if let Some(console) = &diagnostics {
        console.reserve(runtime.buffer.source_memory.clone());
    }
    if let Some(number) = signals.pending() {
        runtime.finish().await?;
        return Ok((128 + number) as u8);
    }
    let mut command = Command::new(&arguments[0]);
    command
        .args(&arguments[1..])
        .env_remove("LOGNARA_RELAY_TOKEN")
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .kill_on_drop(true);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            error!(error = %error, "failed to start program");
            runtime.finish().await?;
            return Ok(match error.kind() {
                io::ErrorKind::NotFound => 127,
                io::ErrorKind::PermissionDenied => 126,
                _ => 1,
            });
        }
    };
    let mut group = Group(child.id().unwrap() as i32);
    info!(
        pid = group.0,
        "program started; stdout/stderr capture enabled"
    );
    let pipe_stop = CancellationToken::new();
    let out = Capture::start(
        child.stdout.take().unwrap(),
        config.sources.stream(SourceKind::Stdout).unwrap().clone(),
        runtime.buffer.clone(),
        pipe_stop.clone(),
        stdout,
    )?;
    let err = Capture::start(
        child.stderr.take().unwrap(),
        config.sources.stream(SourceKind::Stderr).unwrap().clone(),
        runtime.buffer.clone(),
        pipe_stop.clone(),
        stderr,
    )?;
    let mut deadline = None;
    let status = loop {
        tokio::select! {
            result = child.wait() => break result?,
            number = signals.next() => {
                group.send(number, false);
                if matches!(number, libc::SIGTERM | libc::SIGINT | libc::SIGQUIT) && deadline.is_none() { deadline = Some(Instant::now() + Duration::from_secs(10)); }
            }
            _ = async { if let Some(deadline) = deadline { tokio::time::sleep_until(deadline).await; } else { std::future::pending::<()>().await; } } => {
                group.send(libc::SIGKILL, true); deadline = None;
            }
        }
    };
    group.send(libc::SIGTERM, true);
    let drain_deadline = Instant::now() + Duration::from_secs(2);
    tokio::join!(
        out.finish(drain_deadline, &pipe_stop),
        err.finish(drain_deadline, &pipe_stop)
    );
    group.send(libc::SIGKILL, true);
    group.0 = 0;
    if let Err(error) = runtime.finish().await {
        warn!(error = %error, "agent shutdown failed; program exit status retained");
    }
    Ok(status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0)) as u8)
}
