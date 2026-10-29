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
        Ok(())
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
        if self.values.len() >= MAX_FIELDS && field.name() != "message" {
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
            // Includes transport/runtime diagnostics: never collect delivery's own logs.
            tracing::subscriber::with_default(tracing::subscriber::NoSubscriber::default(), || {
                let mut queue = match lognara_spool::Queue::open_limited(&dir, 32 << 20, 100_000, 4096) {
                    Ok(queue) => queue,
                    Err(error) => { eprintln!("self-log queue unavailable: {error}"); return; }
                };
                let mut poll = Instant::now();
                let mut retry = Instant::now();
                let cache = dir.join("settings.json");
                let mut settings = std::fs::read(&cache).ok().and_then(|bytes| serde_json::from_slice::<Settings>(&bytes).ok());
                if let Some(settings) = &settings {
                    self.capture.enabled.store(settings.enabled(self.capture.service), Ordering::Relaxed);
                }
                loop {
                    let stop = stopping.load(Ordering::Acquire);
                    if !stop && Instant::now() >= poll {
                        if let Ok(current) = destination.settings() {
                            self.capture.enabled.store(current.enabled(self.capture.service), Ordering::Relaxed);
                            if settings.as_ref() != Some(&current) {
                                if let Ok(bytes) = serde_json::to_vec(&current) {
                                    let _ = lognara_spool::atomic_file(&dir, "settings.json", &bytes);
                                }
                                settings = Some(current);
                            }
                        }
                        poll = Instant::now() + Duration::from_secs(10);
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
                        match serde_json::to_vec(&records) {
                            Ok(body) if body.len() <= MAX_BODY => {
                                if queue.append_group([Ok((body, records.len() as u64))]).is_err() {
                                    self.capture.dropped.fetch_add(records.len() as u64, Ordering::Relaxed);
                                }
                            }
                            _ => { self.capture.dropped.fetch_add(records.len() as u64, Ordering::Relaxed); }
                        }
                    }
                    let dropped = self.capture.dropped.swap(0, Ordering::Relaxed);
                    if dropped != 0 { eprintln!("self-log capture dropped {dropped} records (queue limit or disk error)"); }
                    if stop {
                        if records.is_empty() { break; }
                        continue;
                    }
                    if Instant::now() >= retry
                        && let Some(entry) = queue.entries().front().cloned() {
                            let sent = queue.read(&entry, MAX_BODY).map_err(io::Error::other)
                                .and_then(|body| destination.send(&body));
                            if sent.is_ok() {
                                if let Err(error) = queue.ack(&[entry]) { eprintln!("self-log acknowledgement failed: {error}"); break; }
                            } else { retry = Instant::now() + Duration::from_secs(5); }
                    }
                    thread::sleep(Duration::from_millis(100));
                }
            });
        })?;
        Ok(Worker {
            stop,
            thread: Some(thread),
        })
    }
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
                "v1/self-logs"
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
