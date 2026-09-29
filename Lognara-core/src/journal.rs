//! Долговечный приём. Этот mutex сериализует назначение номеров, дедупликацию и ACK.
use std::{
    fs::{self, File, OpenOptions},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

use anyhow::Result;
use fs2::FileExt;
use sha2::{Digest, Sha256};
use tokio::sync::{Notify, Semaphore};

use crate::{
    config::Config,
    metrics::Metrics,
    model::now_nanos,
    wal::{Receipt, Wal},
    wire::{self, DecodeError},
};

pub struct Journal {
    pub config: Config,
    pub(crate) wal: Mutex<Wal>,
    pub healthy: AtomicBool,
    pub stopping: AtomicBool,
    pub wake: Notify,
    pub ingest_slots: Arc<Semaphore>,
    pub metrics: Arc<Metrics>,
    _lock: File,
}

#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    #[error(transparent)]
    Invalid(#[from] DecodeError),
    #[error("compressed batch exceeds the byte limit")]
    TooLarge,
    #[error("storage temporarily unavailable")]
    Unavailable,
}

impl Journal {
    pub fn open(config: Config) -> Result<Arc<Self>> {
        fs::create_dir_all(&config.data_dir)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(config.data_dir.join("core.lock"))?;
        lock.try_lock_exclusive()
            .map_err(|_| anyhow::anyhow!("data directory is already in use"))?;
        let wal = Wal::open(&config.data_dir.join("wal"), 1, 1)?;
        let metrics = Arc::new(Metrics::default());
        metrics.wal_bytes.store(wal.bytes(), Ordering::Relaxed);
        let slots = (config.ingest_memory_bytes
            / (config.max_body_bytes + config.max_decoded_bytes))
            .max(1);
        Ok(Arc::new(Self {
            config,
            wal: Mutex::new(wal),
            healthy: AtomicBool::new(true),
            stopping: AtomicBool::new(false),
            wake: Notify::new(),
            ingest_slots: Arc::new(Semaphore::new(slots)),
            metrics,
            _lock: lock,
        }))
    }

    pub fn ready(&self) -> bool {
        self.healthy.load(Ordering::Acquire) && !self.stopping.load(Ordering::Acquire)
    }

    /// Blocking: вызывается вне async executor, после резервирования ingest slot.
    pub fn accept(&self, body: &[u8]) -> Result<Receipt, IngestError> {
        let start = Instant::now();
        if body.len() > self.config.max_body_bytes {
            return Err(IngestError::TooLarge);
        }
        if !self.ready() {
            return Err(IngestError::Unavailable);
        }
        let hash: [u8; 32] = Sha256::digest(body).into();
        let mut wal = self.wal.lock().unwrap();
        if !self.ready() {
            return Err(IngestError::Unavailable);
        }
        if let Some(receipt) = wal.find(&hash) {
            self.metrics
                .duplicate_batches
                .fetch_add(1, Ordering::Relaxed);
            return Ok(receipt.clone());
        }
        if wal.bytes().saturating_add(body.len() as u64 + 84) > self.config.wal_max_bytes
            || fs2::available_space(&self.config.data_dir).unwrap_or(0)
                < self
                    .config
                    .disk_reserve_bytes
                    .saturating_add(body.len() as u64 + 84)
        {
            return Err(IngestError::Unavailable);
        }
        let batch = wire::decode(body, self.config.max_decoded_bytes)?;
        let count = batch.event_count() as u64;
        let agents_dropped = batch
            .groups
            .iter()
            .fold(0u64, |sum, group| sum.saturating_add(group.dropped));
        let received_at = now_nanos();
        let entry = wal.append(body, count, received_at).map_err(|error| {
            tracing::error!(%error, "WAL append failed; refusing further writes until restart");
            self.healthy.store(false, Ordering::Release);
            IngestError::Unavailable
        })?;
        self.metrics.wal_bytes.store(wal.bytes(), Ordering::Relaxed);
        self.metrics
            .accepted_batches
            .fetch_add(1, Ordering::Relaxed);
        self.metrics
            .accepted_events
            .fetch_add(count, Ordering::Relaxed);
        self.metrics
            .relay_dropped
            .fetch_add(batch.dropped, Ordering::Relaxed);
        self.metrics
            .agent_dropped
            .fetch_add(agents_dropped, Ordering::Relaxed);
        self.metrics
            .ack_duration_us
            .fetch_add(start.elapsed().as_micros() as u64, Ordering::Relaxed);
        self.wake.notify_one();
        Ok(entry)
    }
}
