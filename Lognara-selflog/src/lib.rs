//! Bounded, non-blocking capture; durable delivery runs on a dedicated thread.
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fmt::{self, Write},
    io::{self, Read},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tracing::{
    Event, Subscriber,
    field::{Field, Visit},
};
use tracing_subscriber::{Layer, layer::Context};

pub const MAX_BODY: usize = 256 << 10;
const MAX_TEXT: usize = 8192;
const MAX_FIELDS: usize = 24;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub core: bool,
    pub dock: bool,
    pub relay: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            core: true,
            dock: true,
            relay: true,
        }
    }
}
impl Settings {
    pub fn enabled(&self, service: &str) -> bool {
        match service {
            "lognara-core" => self.core,
            "lognara-dock" => self.dock,
            "lognara-relay" => self.relay,
            _ => false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub id: uuid::Uuid,
    pub timestamp: i64,
    pub service: String,
    pub server: String,
    pub level: String,
    pub target: String,
    pub message: String,
    pub fields: BTreeMap<String, String>,
}
impl Record {
    pub fn valid(&self) -> bool {
        matches!(
            self.service.as_str(),
            "lognara-core" | "lognara-dock" | "lognara-relay"
        ) && matches!(
            self.level.as_str(),
            "TRACE" | "DEBUG" | "INFO" | "WARN" | "ERROR"
        ) && self.id.get_version_num() == 7
            && self.server.len() <= 256
            && self.target.len() <= 256
            && self.message.len() <= MAX_TEXT
            && self.fields.len() <= MAX_FIELDS
            && self
                .fields
                .iter()
                .all(|(key, value)| key.len() <= 256 && value.len() <= MAX_TEXT)
    }
}

// Formatting itself is capped, including Debug implementations producing large strings.
struct Text(String);
impl fmt::Write for Text {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let mut end = text.len().min(MAX_TEXT.saturating_sub(self.0.len()));
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        self.0.push_str(&text[..end]);
        if end < text.len() || self.0.len() == MAX_TEXT {
            Err(fmt::Error)
        } else {
            Ok(())
        }
    }
}
#[derive(Default)]
struct Fields {
    message: String,
    values: BTreeMap<String, String>,
    bytes: usize,
}
impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if (self.values.len() >= MAX_FIELDS || self.bytes >= MAX_TEXT) && field.name() != "message"
        {
            return;
        }
        let mut text = Text(String::new());
        let _ = write!(text, "{value:?}");
        if field.name() == "message" {
            self.message = text.0;
        } else {
            let remaining = MAX_TEXT.saturating_sub(self.bytes);
            let mut end = text.0.len().min(remaining);
            while !text.0.is_char_boundary(end) {
                end -= 1;
            }
            text.0.truncate(end);
            self.bytes += text.0.len();
            self.values
                .insert(field.name().chars().take(256).collect(), text.0);
        }
    }
}

#[derive(Clone)]
pub struct Capture {
    tx: SyncSender<Record>,
    service: &'static str,
    server: String,
    enabled: Arc<AtomicBool>,
    dropped: Arc<AtomicU64>,
}
impl<S: Subscriber> Layer<S> for Capture {
    fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
        let meta = event.metadata();
        // Dependency logs (e.g. Tantivy commit messages) would amplify each ingest.
        if !meta.target().starts_with("lognara_") || !self.enabled.load(Ordering::Relaxed) {
            return;
        }
        let mut fields = Fields::default();
        event.record(&mut fields);
        let record = Record {
            id: uuid::Uuid::now_v7(),
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
                .min(i64::MAX as u128) as i64,
            service: self.service.into(),
            server: self.server.clone(),
            level: meta.level().to_string(),
            target: meta.target().chars().take(256).collect(),
            message: fields.message,
            fields: fields.values,
        };
        if self.tx.try_send(record).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

pub struct Collector {
    capture: Capture,
    rx: Receiver<Record>,
}
pub trait Destination: Send + 'static {
    fn settings(&mut self) -> io::Result<Settings>;
    /// Success only after durable acceptance. The same bytes are retried.
    fn send(&mut self, body: &[u8]) -> io::Result<()>;
}

fn batches(records: &[Record]) -> io::Result<Vec<(Vec<u8>, u64)>> {
    let body = serde_json::to_vec(records).map_err(io::Error::other)?;
    if body.len() <= MAX_BODY {
        return Ok(vec![(body, records.len() as u64)]);
    }
    if records.len() < 2 {
        return Err(io::Error::other("oversized self-log record"));
    }
    let (left, right) = records.split_at(records.len() / 2);
    let mut result = batches(left)?;
    result.extend(batches(right)?);
    Ok(result)
}

// Preserve rejected diagnostics before advancing the delivery queue. This archive
// is bounded separately and never deletes old records to make space.
fn archive_rejected(dir: &Path, entry: &lognara_spool::Entry, body: &[u8]) -> io::Result<()> {
    let archive = dir.join("rejected");
    std::fs::create_dir_all(&archive)?;
    std::fs::File::open(dir)?.sync_all()?;
    let name = format!("{:020}-{:020}.json", entry.seq, entry.part);
    match std::fs::File::open(archive.join(&name)) {
        Ok(file) => {
            file.sync_all()?;
            let mut saved = Vec::new();
            file.take(MAX_BODY as u64 + 1).read_to_end(&mut saved)?;
            if saved == body {
                std::fs::File::open(&archive)?.sync_all()?;
                return Ok(());
            }
            return Err(io::Error::other("conflicting rejected self-log record"));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let (mut bytes, mut count) = (0u64, 0usize);
    for file in std::fs::read_dir(&archive)? {
        let file = file?;
        if file.path().extension().is_some_and(|ext| ext == "json") {
            bytes = bytes.saturating_add(file.metadata()?.len());
            count += 1;
        }
    }
    if count >= 4096 || bytes.saturating_add(body.len() as u64) > (8 << 20) {
        return Err(io::Error::other("rejected self-log archive is full"));
    }
    lognara_spool::atomic_file(&archive, &name, body)
}
pub struct Worker {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
impl Worker {
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}
impl Collector {
    pub fn new(service: &'static str) -> Self {
        let (tx, rx) = mpsc::sync_channel(256);
        Self {
            capture: Capture {
                tx,
                service,
                server: std::env::var("HOSTNAME")
                    .or_else(|_| std::env::var("COMPUTERNAME"))
                    .unwrap_or_else(|_| service.into())
                    .chars()
                    .take(256)
                    .collect(),
                enabled: Arc::new(AtomicBool::new(true)),
                dropped: Arc::new(AtomicU64::new(0)),
            },
            rx,
        }
    }
    pub fn layer(&self) -> Capture {
        self.capture.clone()
    }
    pub fn start(self, dir: &Path, mut destination: impl Destination) -> io::Result<Worker> {
        let dir = dir.to_owned();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let thread = thread::Builder::new().name("lognara-selflog".into()).spawn(move || {
            tracing::subscriber::with_default(tracing::subscriber::NoSubscriber::default(), || {
                let (jobs, work) = mpsc::sync_channel(1);
                let (replies, results) = mpsc::channel();
                let sender = match thread::Builder::new().name("lognara-selflog-send".into()).spawn(move || {
                    tracing::subscriber::with_default(tracing::subscriber::NoSubscriber::default(), || {
                        while let Ok(job) = work.recv() {
                            let reply = match job {
                                Job::Settings => Reply::Settings(destination.settings()),
                                Job::Send(body) => Reply::Sent(destination.send(&body)),
                            };
                            if replies.send(reply).is_err() { break; }
                        }
                    });
                }) {
                    Ok(sender) => sender,
                    Err(error) => { eprintln!("self-log sender unavailable: {error}"); return; }
                };
                let mut queue: Option<lognara_spool::Queue> = None;
                let mut reopen = Instant::now();
                let mut poll = Instant::now();
                let mut retry = Instant::now();
                let cache = dir.join("settings.json");
                let mut settings = std::fs::read(&cache).ok().and_then(|bytes| serde_json::from_slice::<Settings>(&bytes).ok());
                if let Some(settings) = &settings {
                    self.capture.enabled.store(settings.enabled(self.capture.service), Ordering::Relaxed);
                }
                let mut busy = false;
                let mut sender_alive = true;
                let mut pending: Option<lognara_spool::Entry> = None;
                loop {
                    let stop = stopping.load(Ordering::Acquire);
                    let reply = match results.try_recv() {
                        Ok(reply) => Some(reply),
                        Err(mpsc::TryRecvError::Empty) => None,
                        Err(mpsc::TryRecvError::Disconnected) => {
                            if sender_alive {
                                eprintln!("self-log sender stopped unexpectedly; capture continues, queued logs require restart for delivery");
                                sender_alive = false;
                                busy = false;
                                pending = None;
                            }
                            None
                        }
                    };
                    if let Some(reply) = reply {
                        busy = false;
                        match reply {
                            Reply::Settings(Ok(current)) => {
                                self.capture.enabled.store(current.enabled(self.capture.service), Ordering::Relaxed);
                                if settings.as_ref() != Some(&current) {
                                    let saved = serde_json::to_vec(&current).map_err(io::Error::other)
                                        .and_then(|bytes| lognara_spool::atomic_file(&dir, "settings.json", &bytes));
                                    match saved {
                                        Ok(()) => settings = Some(current),
                                        Err(error) => eprintln!("self-log settings cache unavailable: {error}"),
                                    }
                                }
                            }
                            Reply::Settings(Err(_)) => {},
                            Reply::Sent(result) => {
                                let entry = pending.take().expect("one outstanding delivery");
                                let archived = if result.as_ref().is_err_and(|error| error.kind() == io::ErrorKind::InvalidData) {
                                    let saved = queue.as_ref().ok_or_else(|| io::Error::other("self-log queue unavailable"))
                                        .and_then(|queue| queue.read(&entry, MAX_BODY).map_err(io::Error::other))
                                        .and_then(|body| archive_rejected(&dir, &entry, &body));
                                    match saved {
                                        Ok(()) => {
                                            eprintln!("self-log batch permanently rejected; {} records retained in {}", entry.events, dir.join("rejected").display());
                                            true
                                        }
                                        Err(error) => { eprintln!("self-log rejection could not be archived; queue retained: {error}"); false }
                                    }
                                } else { false };
                                if result.is_ok() || archived {
                                    if let Some(active) = queue.as_mut()
                                        && let Err(error) = active.ack(&[entry]) {
                                        eprintln!("self-log acknowledgement failed; reopening queue: {error}");
                                        queue = None;
                                        reopen = Instant::now() + Duration::from_secs(5);
                                    }
                                } else { retry = Instant::now() + Duration::from_secs(5); }
                            }
                        }
                    }
                    if queue.is_none() && Instant::now() >= reopen {
                        match lognara_spool::Queue::open_limited(&dir, 32 << 20, 100_000, 4096) {
                            Ok(opened) => queue = Some(opened),
                            Err(error) => {
                                eprintln!("self-log queue unavailable: {error}");
                                reopen = Instant::now() + Duration::from_secs(5);
                            }
                        }
                    }
                    let mut records = Vec::new();
                    for _ in 0..16 {
                        match self.rx.try_recv() {
                            Ok(record) if self.capture.enabled.load(Ordering::Relaxed) => records.push(record),
                            Ok(_) => {},
                            Err(_) => break,
                        }
                    }
                    if !records.is_empty() {
                        let saved = batches(&records).ok().is_some_and(|parts| {
                            queue.as_mut().is_some_and(|queue| queue.append_group(parts.into_iter().map(Ok)).is_ok())
                        });
                        if !saved { self.capture.dropped.fetch_add(records.len() as u64, Ordering::Relaxed); }
                    }
                    let dropped = self.capture.dropped.swap(0, Ordering::Relaxed);
                    if dropped != 0 { eprintln!("self-log capture dropped {dropped} records (queue limit or disk error)"); }
                    if stop {
                        if records.is_empty() { break; }
                        continue;
                    }
                    if sender_alive && !busy && Instant::now() >= poll {
                        if jobs.send(Job::Settings).is_err() { break; }
                        busy = true;
                        poll = Instant::now() + Duration::from_secs(10);
                    } else if sender_alive && !busy && Instant::now() >= retry
                        && let Some(active) = queue.as_mut()
                        && let Some(entry) = active.entries().front().cloned() {
                            match active.read(&entry, MAX_BODY) {
                                Ok(body) => {
                                    if jobs.send(Job::Send(body)).is_err() { break; }
                                    pending = Some(entry);
                                    busy = true;
                                }
                                Err(error) => {
                                    eprintln!("self-log queue read failed; persisted files retained: {error}");
                                    queue = None;
                                    reopen = Instant::now() + Duration::from_secs(5);
                                    retry = reopen;
                                }
                            }
                    }
                    thread::sleep(Duration::from_millis(100));
                }
                drop(jobs);
                let _ = sender.join();
            });
        })?;
        Ok(Worker {
            stop,
            thread: Some(thread),
        })
    }
}

enum Job {
    Settings,
    Send(Vec<u8>),
}
enum Reply {
    Settings(io::Result<Settings>),
    Sent(io::Result<()>),
}

pub struct Remote {
    client: Option<reqwest::blocking::Client>,
    base: reqwest::Url,
    token: String,
    prefix: &'static str,
}
impl Remote {
    // Build blocking client in the worker, outside any async runtime.
    pub fn new(base: reqwest::Url, token: String, relay: bool) -> Self {
        Self {
            client: None,
            base,
            token,
            prefix: if relay {
                "self-logs"
            } else {
                "v1/admin/self-logs"
            },
        }
    }
    fn client(&mut self) -> io::Result<&reqwest::blocking::Client> {
        if self.client.is_none() {
            self.client = Some(
                reqwest::blocking::Client::builder()
                    .timeout(Duration::from_secs(3))
                    .redirect(reqwest::redirect::Policy::none())
                    .build()
                    .map_err(io::Error::other)?,
            );
        }
        Ok(self.client.as_ref().unwrap())
    }
}
impl Destination for Remote {
    fn settings(&mut self) -> io::Result<Settings> {
        let url = self
            .base
            .join(&format!("{}/settings", self.prefix))
            .map_err(io::Error::other)?;
        let token = self.token.clone();
        let response = self
            .client()?
            .get(url)
            .bearer_auth(token)
            .send()
            .and_then(|reply| reply.error_for_status())
            .map_err(io::Error::other)?;
        let mut bytes = Vec::new();
        response.take(4097).read_to_end(&mut bytes)?;
        if bytes.len() > 4096 {
            return Err(io::Error::other("oversized self-log settings"));
        }
        serde_json::from_slice(&bytes).map_err(io::Error::other)
    }
    fn send(&mut self, body: &[u8]) -> io::Result<()> {
        let url = self.base.join(self.prefix).map_err(io::Error::other)?;
        let token = self.token.clone();
        let response = self
            .client()?
            .post(url)
            .bearer_auth(token)
            .header("Content-Type", "application/json")
            .body(body.to_vec())
            .send()
            .map_err(io::Error::other)?;
        if matches!(
            response.status(),
            reqwest::StatusCode::BAD_REQUEST | reqwest::StatusCode::PAYLOAD_TOO_LARGE
        ) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "self-log batch permanently rejected",
            ));
        }
        if response.status() != reqwest::StatusCode::NO_CONTENT {
            return Err(io::Error::other("self-log delivery not acknowledged"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::prelude::*;
    #[test]
    fn formatting_stops_when_output_budget_is_exhausted() {
        struct Large(Arc<AtomicU64>);
        impl fmt::Debug for Large {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                for _ in 0..100_000 {
                    self.0.fetch_add(1, Ordering::Relaxed);
                    formatter.write_str("12345678")?;
                }
                Ok(())
            }
        }
        let writes = Arc::new(AtomicU64::new(0));
        let mut text = Text(String::new());
        assert!(write!(text, "{:?}", Large(writes.clone())).is_err());
        assert_eq!(text.0.len(), MAX_TEXT);
        assert_eq!(writes.load(Ordering::Relaxed), (MAX_TEXT / 8) as u64);
    }
    #[cfg(unix)]
    fn wait_until(mut predicate: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !predicate() {
            assert!(
                Instant::now() < deadline,
                "observable queue state timed out"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }
    #[cfg(unix)]
    fn groups(dir: &Path) -> usize {
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "group"))
            .count()
    }
    #[cfg(unix)]
    #[test]
    fn stalled_network_does_not_block_durable_capture() {
        struct Sink {
            started: mpsc::Sender<()>,
            release: Receiver<()>,
        }
        impl Destination for Sink {
            fn settings(&mut self) -> io::Result<Settings> {
                Ok(Settings::default())
            }
            fn send(&mut self, _: &[u8]) -> io::Result<()> {
                self.started.send(()).unwrap();
                self.release
                    .recv_timeout(Duration::from_secs(5))
                    .map_err(io::Error::other)?;
                Ok(())
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let collector = Collector::new("lognara-core");
        let capture = collector.layer();
        let subscriber = Arc::new(tracing_subscriber::registry().with(capture.clone()));
        tracing::subscriber::with_default(
            subscriber.clone(),
            || tracing::error!(target: "lognara_core", "initial"),
        );
        let (started, ready) = mpsc::channel();
        let (release, blocked) = mpsc::channel();
        let worker = collector
            .start(
                dir.path(),
                Sink {
                    started,
                    release: blocked,
                },
            )
            .unwrap();
        ready.recv_timeout(Duration::from_secs(5)).unwrap();
        tracing::subscriber::with_default(subscriber.clone(), || {
            for n in 0..64 {
                tracing::error!(target: "lognara_core", n, "during outage");
            }
        });
        wait_until(|| groups(dir.path()) >= 5);
        assert_eq!(capture.dropped.load(Ordering::Relaxed), 0);
        // Stop before releasing the in-flight call, so no further network call is scheduled.
        worker.stop.store(true, Ordering::Release);
        release.send(()).unwrap();
        worker.stop();
    }
    #[cfg(unix)]
    #[test]
    fn permanent_rejection_does_not_block_later_records() {
        struct Sink {
            sent: mpsc::Sender<Vec<u8>>,
            first: bool,
        }
        impl Destination for Sink {
            fn settings(&mut self) -> io::Result<Settings> {
                Ok(Settings::default())
            }
            fn send(&mut self, body: &[u8]) -> io::Result<()> {
                self.sent.send(body.to_vec()).unwrap();
                if std::mem::take(&mut self.first) {
                    Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "permanent rejection",
                    ))
                } else {
                    Ok(())
                }
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let collector = Collector::new("lognara-relay");
        let subscriber = Arc::new(tracing_subscriber::registry().with(collector.layer()));
        tracing::subscriber::with_default(
            subscriber.clone(),
            || tracing::warn!(target: "lognara_relay", "first"),
        );
        let (sent, received) = mpsc::channel();
        let worker = collector
            .start(dir.path(), Sink { sent, first: true })
            .unwrap();
        let first = received.recv_timeout(Duration::from_secs(5)).unwrap();
        tracing::subscriber::with_default(
            subscriber,
            || tracing::warn!(target: "lognara_relay", "second"),
        );
        let second = received.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_ne!(first, second);
        wait_until(|| groups(dir.path()) == 0);
        worker.stop();
        assert!(received.try_recv().is_err());
        let archived: Vec<_> = std::fs::read_dir(dir.path().join("rejected"))
            .unwrap()
            .collect();
        assert_eq!(archived.len(), 1);
        assert_eq!(
            std::fs::read(archived[0].as_ref().unwrap().path()).unwrap(),
            first
        );
    }
    #[cfg(unix)]
    #[test]
    fn rejected_archive_is_idempotent_and_never_evicts_records_at_its_limit() {
        let dir = tempfile::tempdir().unwrap();
        let mut queue =
            lognara_spool::Queue::open_limited(dir.path(), 32 << 20, 100_000, 4096).unwrap();
        let body = b"[]";
        queue.append_group([Ok((body.to_vec(), 1))]).unwrap();
        let first = queue.entries().front().unwrap().clone();
        archive_rejected(dir.path(), &first, body).unwrap();
        archive_rejected(dir.path(), &first, body).unwrap();
        assert_eq!(
            std::fs::read_dir(dir.path().join("rejected"))
                .unwrap()
                .count(),
            1
        );
        let filler = std::fs::File::create(dir.path().join("rejected/filler.json")).unwrap();
        filler.set_len(8 << 20).unwrap();
        queue.append_group([Ok((body.to_vec(), 1))]).unwrap();
        let second = queue.entries().back().unwrap();
        assert!(archive_rejected(dir.path(), second, body).is_err());
        assert_eq!(queue.events(), 2);
        assert_eq!(queue.read(&first, MAX_BODY).unwrap(), body);
    }
    #[cfg(unix)]
    #[test]
    fn sender_panic_preserves_capture_and_pending_delivery_for_restart() {
        struct Sink(mpsc::Sender<()>);
        impl Destination for Sink {
            fn settings(&mut self) -> io::Result<Settings> {
                Ok(Settings::default())
            }
            fn send(&mut self, _: &[u8]) -> io::Result<()> {
                self.0.send(()).unwrap();
                panic!("injected sender failure");
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let collector = Collector::new("lognara-core");
        let subscriber = Arc::new(tracing_subscriber::registry().with(collector.layer()));
        tracing::subscriber::with_default(
            subscriber.clone(),
            || tracing::error!(target: "lognara_core", "first"),
        );
        let (started, ready) = mpsc::channel();
        let worker = collector.start(dir.path(), Sink(started)).unwrap();
        ready.recv_timeout(Duration::from_secs(5)).unwrap();
        tracing::subscriber::with_default(
            subscriber,
            || tracing::error!(target: "lognara_core", "after sender failure"),
        );
        wait_until(|| groups(dir.path()) == 2);
        worker.stop();
        let queue =
            lognara_spool::Queue::open_limited(dir.path(), 32 << 20, 100_000, 4096).unwrap();
        assert_eq!(queue.events(), 2);
    }
    #[test]
    fn relay_remote_uses_sibling_endpoints_and_classifies_http_responses() {
        use std::{
            io::{BufRead, BufReader},
            net::TcpListener,
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1/batches", listener.local_addr().unwrap());
        let (sent, received) = mpsc::channel();
        let server = thread::spawn(move || {
            for (status, body) in [
                (200, r#"{"core":true,"dock":true,"relay":false}"#),
                (204, ""),
                (400, ""),
                (413, ""),
                (503, ""),
            ] {
                let (stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut input = BufReader::new(stream);
                let mut headers = String::new();
                loop {
                    let mut line = String::new();
                    input.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    assert!(!line.is_empty());
                    headers.push_str(&line);
                }
                let length: usize = headers
                    .lines()
                    .find_map(|line| {
                        line.to_lowercase()
                            .strip_prefix("content-length:")
                            .map(|n| n.trim().parse().unwrap())
                    })
                    .unwrap_or(0);
                let mut data = vec![0; length];
                input.read_exact(&mut data).unwrap();
                std::io::Write::write_all(input.get_mut(), format!("HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).unwrap();
                sent.send((headers, data)).unwrap();
            }
        });
        let mut remote = Remote::new(url.parse().unwrap(), "test-relay-token".into(), true);
        assert!(!remote.settings().unwrap().relay);
        remote.send(b"[]").unwrap();
        let bad = remote.send(b"[]").unwrap_err();
        let large = remote.send(b"[]").unwrap_err();
        let unavailable = remote.send(b"[]").unwrap_err();
        let calls: Vec<_> = (0..5)
            .map(|_| received.recv_timeout(Duration::from_secs(3)).unwrap())
            .collect();
        server.join().unwrap();
        assert!(
            calls[0]
                .0
                .starts_with("GET /v1/self-logs/settings HTTP/1.1")
        );
        for (headers, body) in &calls[1..] {
            assert!(headers.starts_with("POST /v1/self-logs HTTP/1.1"));
            assert_eq!(body, b"[]");
        }
        assert!(calls.iter().all(|(headers, _)| {
            headers
                .to_lowercase()
                .contains("authorization: bearer test-relay-token")
        }));
        assert_eq!(bad.kind(), io::ErrorKind::InvalidData);
        assert_eq!(large.kind(), io::ErrorKind::InvalidData);
        assert_ne!(unavailable.kind(), io::ErrorKind::InvalidData);
    }
    #[test]
    fn escaped_large_messages_split_without_loss() {
        let collector = Collector::new("lognara-core");
        tracing::subscriber::with_default(
            tracing_subscriber::registry().with(collector.layer()),
            || {
                for _ in 0..16 {
                    tracing::error!(target: "lognara_core", "{}", "\0".repeat(MAX_TEXT));
                }
            },
        );
        let records: Vec<_> = collector.rx.try_iter().collect();
        let parts = batches(&records).unwrap();
        assert!(parts.len() > 1);
        assert!(parts.iter().all(|(bytes, _)| bytes.len() <= MAX_BODY));
        assert_eq!(parts.iter().map(|(_, count)| count).sum::<u64>(), 16);
    }
    #[cfg(unix)]
    #[test]
    fn outage_restart_replays_identical_bytes_without_collecting_transport_logs() {
        struct Sink {
            sent: mpsc::Sender<Vec<u8>>,
            fail: bool,
        }
        impl Destination for Sink {
            fn settings(&mut self) -> io::Result<Settings> {
                Ok(Settings::default())
            }
            fn send(&mut self, body: &[u8]) -> io::Result<()> {
                tracing::error!(target: "lognara_selflog::transport", "must not recurse");
                self.sent.send(body.to_vec()).unwrap();
                if self.fail {
                    Err(io::Error::other("offline"))
                } else {
                    Ok(())
                }
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let collector = Collector::new("lognara-dock");
        tracing::subscriber::with_default(
            tracing_subscriber::registry().with(collector.layer()),
            || {
                tracing::error!(target: "lognara_dock", "core offline");
            },
        );
        let (sent, received) = mpsc::channel();
        let worker = collector
            .start(dir.path(), Sink { sent, fail: true })
            .unwrap();
        let first = received.recv_timeout(Duration::from_secs(5)).unwrap();
        worker.stop();
        let (sent, received) = mpsc::channel();
        let worker = Collector::new("lognara-dock")
            .start(dir.path(), Sink { sent, fail: false })
            .unwrap();
        assert_eq!(
            received.recv_timeout(Duration::from_secs(5)).unwrap(),
            first
        );
        wait_until(|| groups(dir.path()) == 0);
        worker.stop();
        assert!(received.try_recv().is_err());
        let queue =
            lognara_spool::Queue::open_limited(dir.path(), 32 << 20, 100_000, 4096).unwrap();
        assert!(queue.entries().is_empty());
    }
    #[test]
    fn capture_fields_disable_and_exclude_dependency_events() {
        let collector = Collector::new("lognara-core");
        let subscriber = tracing_subscriber::registry().with(collector.layer());
        tracing::subscriber::with_default(subscriber, || {
            tracing::error!(target: "lognara_core::pattern_catalog", error = %"storage: disk full", "catalog failed");
            tracing::info!(target: "tantivy::indexer", "commit");
            collector.capture.enabled.store(false, Ordering::Relaxed);
            tracing::error!(target: "lognara_core", "disabled");
        });
        let record = collector.rx.try_recv().unwrap();
        assert_eq!(record.message, "catalog failed");
        assert_eq!(record.fields["error"], "storage: disk full");
        assert!(record.valid());
        assert!(collector.rx.try_recv().is_err());
    }
    #[test]
    fn large_fields_and_full_channel_are_bounded() {
        let collector = Collector::new("lognara-relay");
        tracing::subscriber::with_default(
            tracing_subscriber::registry().with(collector.layer()),
            || {
                for _ in 0..300 {
                    tracing::warn!(target: "lognara_relay", error = %"я".repeat(MAX_TEXT), "failed");
                }
            },
        );
        assert_eq!(collector.capture.dropped.load(Ordering::Relaxed), 44);
        assert!(collector.rx.try_iter().all(|record| record.valid()));
    }
}
