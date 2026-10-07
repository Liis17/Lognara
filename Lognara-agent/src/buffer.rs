//! Подтверждённые записи на диске и два независимых резерва моделей в RAM.

use std::collections::VecDeque;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use lognara_spool::{Entry, Error, Queue, wire_budget};
use serde::Serialize;
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};

use crate::config::{Config, INDEX_BYTES, INDEX_ENTRIES};
use crate::wire::{self, Batch, Record};

pub struct Buffer {
    queue: Mutex<Queue>,
    config: Config,
    template: Batch,
    events: AtomicU64,
    bytes: AtomicU64,
    available: AtomicBool,
    ready: AtomicBool,
    changed: Notify,
    pub input: Arc<Semaphore>,
    pub models: Arc<Semaphore>,
    jobs: Arc<Semaphore>,
    _metadata: OwnedSemaphorePermit,
}

pub struct Pending {
    pub body: Bytes,
    entries: Vec<Entry>,
    // Объекты уничтожаются до освобождения их резерва.
    _models: OwnedSemaphorePermit,
}

impl Buffer {
    pub async fn open(config: Config) -> Result<Arc<Self>, Error> {
        let init = config.clone();
        let queue = tokio::task::spawn_blocking(move || {
            Queue::open_limited(
                &init.spool_dir,
                init.spool_max_bytes,
                init.max_buffer as u64,
                INDEX_ENTRIES,
            )
        })
        .await
        .map_err(|e| io::Error::other(e.to_string()))??;
        let models = Arc::new(Semaphore::new(config.max_buffer_bytes));
        let metadata = models
            .clone()
            .acquire_many_owned((INDEX_BYTES + 4 * config.metadata_bytes()) as u32)
            .await
            .map_err(io::Error::other)?;
        let buffer = Arc::new(Self {
            queue: Mutex::new(queue),
            template: Batch {
                service: config.service.clone(),
                server: config.server.clone(),
                backend: config.backend.clone(),
                environment: config.environment.clone(),
                service_instance: config.service_instance.clone(),
                sent_at: wire::unix_nanos(),
                dropped: 0,
                records: Vec::new(),
            },
            models,
            _metadata: metadata,
            config,
            events: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            available: AtomicBool::new(false),
            ready: AtomicBool::new(true),
            changed: Notify::new(),
            input: Arc::new(Semaphore::new(1)),
            jobs: Arc::new(Semaphore::new(1)),
        });
        buffer.refresh(&buffer.queue.lock().unwrap());
        Ok(buffer)
    }

    fn refresh(&self, queue: &Queue) {
        self.events.store(queue.events(), Ordering::Release);
        self.bytes.store(
            queue.entries().iter().map(|e| e.size).sum(),
            Ordering::Release,
        );
        self.available.store(queue.available(), Ordering::Release);
    }
    pub fn admitted(&self) -> bool {
        self.ready.load(Ordering::Acquire) && self.available.load(Ordering::Acquire)
    }
    pub fn set_ready(&self, ready: bool) {
        self.ready.store(ready, Ordering::Release);
    }
    pub fn len(&self) -> u64 {
        self.events.load(Ordering::Acquire)
    }
    pub fn budget(&self) -> u32 {
        self.config.pipeline_bytes() as u32
    }
    pub fn record_limit(&self) -> usize {
        self.config.input_record_limit().min(self.config.max_buffer)
    }
    pub fn max_record_bytes(&self) -> usize {
        self.config.max_record_bytes
    }
    pub async fn full(&self) {
        self.changed.notified().await
    }

    /// Сохраняет ВСЕ части одного HTTP-запроса, не публикуя промежуточные части.
    pub fn push(&self, records: Vec<Record>) -> Result<(), Error> {
        if records.is_empty() {
            return Ok(());
        }
        let mut records: VecDeque<_> = records.into();
        let mut queue = self.queue.lock().unwrap();
        let result = queue.append_group(std::iter::from_fn(|| {
            if records.is_empty() {
                return None;
            }
            Some((|| {
                let mut batch = self.template.clone();
                let mut size = measured(&batch)? + 4; // array header расширяется на больших пачках
                let count = self
                    .config
                    .batch_size
                    .min(self.record_limit())
                    .min(records.len());
                batch.records = Vec::with_capacity(count);
                while batch.records.len() < count {
                    let added = measured(records.front().unwrap())?;
                    if size + added > self.raw_limit() {
                        break;
                    }
                    size += added;
                    batch.records.push(records.pop_front().unwrap());
                }
                if batch.records.is_empty() {
                    return Err(Error::TooLarge);
                }
                let body = pack(&batch, self.config.max_batch_bytes)?;
                wire_budget::validate(&body, self.model_limit()).map_err(|_| Error::TooLarge)?;
                Ok((body, batch.records.len() as u64))
            })())
        }));
        self.refresh(&queue);
        self.changed.notify_one();
        result
    }

    fn raw_limit(&self) -> usize {
        self.config.max_batch_bytes - (128 << 10)
    }
    fn model_limit(&self) -> usize {
        self.config.pipeline_bytes() - 2 * self.config.max_batch_bytes - 8192
    }

    /// Восстанавливает прежнее тело либо сохраняет новое до первой HTTP-попытки.
    pub fn prepare(
        &self,
        partial: bool,
        permit: OwnedSemaphorePermit,
    ) -> Result<Option<Pending>, Error> {
        let queue = self.queue.lock().unwrap();
        if let Some((body, entries)) = queue.pending(self.config.max_batch_bytes)? {
            if !entries.is_empty() {
                return Ok(Some(Pending {
                    body: body.into(),
                    entries,
                    _models: permit,
                }));
            }
            queue.clear_pending()?;
        }
        if queue.entries().is_empty()
            || (!partial
                && queue.events() < self.config.batch_size as u64
                && self.bytes.load(Ordering::Acquire) < self.raw_limit() as u64)
        {
            return Ok(None);
        }
        let mut batch: Option<Batch> = None;
        let mut entries = Vec::new();
        let mut raw_size = 0usize;
        let mut model_size = 0usize;
        for entry in queue.entries() {
            if let Some(batch) = &batch {
                if batch.records.len() as u64 + entry.events > self.config.batch_size as u64
                    || raw_size + entry.size as usize > self.raw_limit()
                {
                    break;
                }
            }
            let body = queue.read(entry, self.config.max_batch_bytes)?;
            let estimate =
                wire_budget::estimate(&body, self.model_limit()).map_err(|_| Error::TooLarge)?;
            if model_size + estimate > self.model_limit() {
                if batch.is_none() {
                    return Err(Error::TooLarge);
                }
                break;
            }
            let decoded: Batch = rmp_serde::from_slice(&body)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            drop(body);
            if decoded.records.len() as u64 != entry.events {
                return Err(io::Error::other("spool record count changed").into());
            }
            if let Some(current) = &mut batch {
                if !same_source(current, &decoded) {
                    break;
                }
                current.records.reserve_exact(decoded.records.len());
                current.records.extend(decoded.records);
            } else {
                batch = Some(decoded);
            }
            entries.push(entry.clone());
            raw_size += entry.size as usize;
            model_size += estimate;
            if batch.as_ref().unwrap().records.len() >= self.config.batch_size {
                break;
            }
        }
        let mut batch = batch.ok_or_else(|| io::Error::other("empty spool batch"))?;
        batch.sent_at = wire::unix_nanos();
        let packed = pack(&batch, self.config.max_batch_bytes)?;
        drop(batch);
        let mut encoder =
            zstd::stream::write::Encoder::new(Limited::new(self.config.max_batch_bytes), 3)?;
        encoder.window_log(23)?;
        encoder.write_all(&packed)?;
        drop(packed);
        let body = encoder.finish()?.bytes;
        queue.save_pending(&entries, &body, self.config.max_batch_bytes)?;
        Ok(Some(Pending {
            body: body.into(),
            entries,
            _models: permit,
        }))
    }

    pub fn ack(&self, pending: Pending) -> Result<(), Error> {
        let mut queue = self.queue.lock().unwrap();
        let result = queue
            .ack(&pending.entries)
            .and_then(|_| queue.clear_pending());
        self.refresh(&queue);
        result
    }

    // Worker владеет permit; отмена ожидающего future не освобождает его память.
    pub async fn prepare_async(self: &Arc<Self>, partial: bool) -> Result<Option<Pending>, Error> {
        let permit = self
            .models
            .clone()
            .acquire_many_owned(self.budget())
            .await
            .map_err(io::Error::other)?;
        let job = self
            .jobs
            .clone()
            .acquire_owned()
            .await
            .map_err(io::Error::other)?;
        let buffer = self.clone();
        tokio::task::spawn_blocking(move || {
            let _job = job;
            buffer.prepare(partial, permit)
        })
        .await
        .map_err(|e| io::Error::other(e.to_string()))?
    }
    pub async fn ack_async(self: &Arc<Self>, pending: Pending) -> Result<(), Error> {
        let job = self
            .jobs
            .clone()
            .acquire_owned()
            .await
            .map_err(io::Error::other)?;
        let buffer = self.clone();
        tokio::task::spawn_blocking(move || {
            let _job = job;
            buffer.ack(pending)
        })
        .await
        .map_err(|e| io::Error::other(e.to_string()))?
    }
    pub async fn drain(&self) {
        self.input.close();
        // Closed semaphore нельзя acquire: models и jobs служат барьером живых workers.
        let _models = self
            .models
            .clone()
            .acquire_many_owned(2 * self.budget())
            .await;
        let _jobs = self.jobs.clone().acquire_owned().await;
    }
}

fn same_source(a: &Batch, b: &Batch) -> bool {
    a.service == b.service
        && a.server == b.server
        && a.backend == b.backend
        && a.environment == b.environment
        && a.service_instance == b.service_instance
}
struct Limited {
    bytes: Vec<u8>,
    limit: usize,
}
impl Limited {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(limit),
            limit,
        }
    }
}
impl Write for Limited {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit - self.bytes.len() {
            return Err(io::Error::other("batch byte limit"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn pack(batch: &Batch, limit: usize) -> Result<Vec<u8>, Error> {
    let mut output = Limited::new(limit);
    batch
        .serialize(&mut rmp_serde::Serializer::new(&mut output).with_struct_map())
        .map_err(|_| Error::TooLarge)?;
    Ok(output.bytes)
}
struct Counter(usize);
impl Write for Counter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn measured(value: &impl Serialize) -> Result<usize, Error> {
    let mut counter = Counter(0);
    value
        .serialize(&mut rmp_serde::Serializer::new(&mut counter).with_struct_map())
        .map_err(|_| Error::TooLarge)?;
    Ok(counter.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::Payload;

    fn config(dir: &std::path::Path) -> Config {
        let mut c = Config::from_lookup(|key| match key {
            "LOGNARA_SERVICE" => Some("api".into()),
            "LOGNARA_SERVER" => Some("server".into()),
            "LOGNARA_BACKEND" => Some("backend".into()),
            "LOGNARA_RELAY_TOKEN" => Some("secret".into()),
            _ => None,
        })
        .unwrap();
        c.spool_dir = dir.to_path_buf();
        c.batch_size = 3;
        c
    }
    fn records(n: usize) -> Vec<Record> {
        (0..n)
            .map(|n| Record {
                received_at: n as i64 + 1,
                payload: Payload::Text(n.to_string()),
            })
            .collect()
    }
    async fn prepare(b: &Arc<Buffer>, partial: bool) -> Option<Pending> {
        b.prepare_async(partial).await.unwrap()
    }
    fn decode(p: &Pending) -> Batch {
        rmp_serde::from_slice(&zstd::decode_all(&p.body[..]).unwrap()).unwrap()
    }

    #[tokio::test]
    async fn combines_requests_replays_exact_bytes_and_only_acks_confirmed_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let c = config(dir.path());
        let b = Buffer::open(c.clone()).await.unwrap();
        b.push(records(1)).unwrap();
        assert!(prepare(&b, false).await.is_none());
        b.push(records(2)).unwrap();
        let p = prepare(&b, false).await.unwrap();
        assert_eq!(decode(&p).records.len(), 3);
        let bytes = p.body.clone();
        drop(p);
        drop(b);
        let b = Buffer::open(c).await.unwrap();
        let p = prepare(&b, false).await.unwrap();
        assert_eq!(p.body, bytes);
        assert_eq!(b.len(), 3);
        b.ack_async(p).await.unwrap();
        assert_eq!(b.len(), 0);
        assert!(prepare(&b, true).await.is_none());
    }

    #[tokio::test]
    async fn quota_failure_is_atomic_and_full_spool_can_still_deliver() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = config(dir.path());
        c.batch_size = 2;
        c.max_buffer = 3;
        let b = Buffer::open(c.clone()).await.unwrap();
        b.push(records(1)).unwrap();
        assert!(matches!(b.push(records(4)), Err(Error::Full)));
        assert_eq!(b.len(), 1);
        b.push(records(2)).unwrap();
        assert!(!b.admitted());
        let p = prepare(&b, true).await.unwrap();
        b.ack_async(p).await.unwrap();
        let p = prepare(&b, true).await.unwrap();
        b.ack_async(p).await.unwrap();
        drop(b);
        let b = Buffer::open(c).await.unwrap();
        assert_eq!(b.len(), 0);
    }

    #[tokio::test]
    async fn byte_pressure_sends_bounded_incompressible_packet_before_count_limit() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = config(dir.path());
        c.batch_size = 1000;
        let b = Buffer::open(c).await.unwrap();
        let mut value = vec![0u8; 2 << 20];
        let mut state = 1u64;
        for byte in &mut value {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            *byte = state as u8;
        }
        for _ in 0..4 {
            b.push(vec![Record {
                received_at: 1,
                payload: Payload::Binary(value.clone()),
            }])
            .unwrap();
        }
        let p = prepare(&b, false).await.unwrap();
        assert!(p.body.len() <= 8 << 20);
        let packed = zstd::decode_all(&p.body[..]).unwrap();
        assert!(packed.len() <= 8 << 20);
        assert_eq!(decode(&p).records.len(), 3);
        b.ack_async(p).await.unwrap();
        assert_eq!(b.len(), 1);
    }

    #[tokio::test]
    async fn detached_worker_retains_memory_reservation_until_objects_are_destroyed() {
        let dir = tempfile::tempdir().unwrap();
        let b = Buffer::open(config(dir.path())).await.unwrap();
        let permit = b
            .models
            .clone()
            .acquire_many_owned(b.budget())
            .await
            .unwrap();
        let (release, released) = std::sync::mpsc::channel();
        let (started, ready) = tokio::sync::oneshot::channel();
        let worker = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            started.send(()).unwrap();
            released.recv().unwrap();
        });
        ready.await.unwrap();
        drop(worker);
        assert_eq!(b.models.available_permits(), b.budget() as usize);
        release.send(()).unwrap();
        b.drain().await;
        assert!(b.models.available_permits() >= 2 * b.budget() as usize);
    }
}
