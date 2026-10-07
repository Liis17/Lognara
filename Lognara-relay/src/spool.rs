//! Подтверждённые тела запросов core: атомарные группы, без вытеснения.

use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use lognara_spool::Queue;
use tokio::sync::{Notify, Semaphore};

pub use lognara_spool::Error;

#[derive(Clone)]
pub struct Spool {
    state: Arc<State>,
    replay_limit: usize,
}

struct State {
    queue: Mutex<Queue>,
    events: AtomicU64,
    len: AtomicUsize,
    available: AtomicBool,
    max_size: AtomicU64,
    changed: Notify,
    reads: Arc<Semaphore>,
}

impl State {
    fn refresh(&self, queue: &Queue) {
        self.events.store(queue.events(), Ordering::Release);
        self.len.store(queue.entries().len(), Ordering::Release);
        self.available.store(queue.available(), Ordering::Release);
        self.max_size.store(
            queue.entries().iter().map(|e| e.size).max().unwrap_or(0),
            Ordering::Release,
        );
    }
}

impl Spool {
    pub async fn open(dir: &Path, max_bytes: u64) -> io::Result<Self> {
        let dir = dir.to_owned();
        let queue = tokio::task::spawn_blocking(move || Queue::open(&dir, max_bytes, 100_000))
            .await
            .map_err(io::Error::other)?
            .map_err(io::Error::other)?;
        let state = Arc::new(State {
            queue: Mutex::new(queue),
            events: AtomicU64::new(0),
            len: AtomicUsize::new(0),
            available: AtomicBool::new(false),
            max_size: AtomicU64::new(0),
            changed: Notify::new(),
            reads: Arc::new(Semaphore::new(1)),
        });
        state.refresh(&state.queue.lock().unwrap());
        Ok(Self {
            state,
            replay_limit: usize::MAX,
        })
    }

    pub fn set_max_events(&self, limit: usize) {
        let mut queue = self.state.queue.lock().unwrap();
        queue.set_max_events(limit as u64);
        self.state.refresh(&queue);
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn len(&self) -> usize {
        self.state.len.load(Ordering::Acquire)
    }
    pub fn events(&self) -> u64 {
        self.state.events.load(Ordering::Acquire)
    }
    pub fn available(&self) -> bool {
        self.state.available.load(Ordering::Acquire)
    }
    pub fn set_replay_limit(&mut self, limit: usize) {
        self.replay_limit = limit;
    }
    pub fn fits_replay_limit(&self) -> bool {
        self.state.max_size.load(Ordering::Acquire) <= self.replay_limit as u64
    }
    pub async fn changed(&self) {
        self.state.changed.notified().await;
    }

    /// Только в blocking-работнике: кодирует и сохраняет по одной части.
    pub fn append_group(
        &self,
        parts: impl IntoIterator<Item = Result<(Vec<u8>, u64), Error>>,
    ) -> Result<(), Error> {
        let mut queue = self.state.queue.lock().unwrap();
        let result = queue.append_group(parts);
        self.state.refresh(&queue);
        self.state.changed.notify_one();
        result
    }

    pub async fn push(&mut self, body: &[u8], events: u64) -> Result<(), Error> {
        let body = body.to_vec();
        let spool = self.clone();
        tokio::task::spawn_blocking(move || spool.append_group([Ok((body, events))]))
            .await
            .map_err(|e| Error::Io(io::Error::other(e)))?
    }

    pub async fn oldest(&self) -> Result<Option<Vec<u8>>, Error> {
        self.oldest_limited(self.replay_limit).await
    }
    pub async fn oldest_limited(&self, limit: usize) -> Result<Option<Vec<u8>>, Error> {
        let state = self.state.clone();
        let permit = state.reads.clone().acquire_owned().await.unwrap();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let queue = state.queue.lock().unwrap();
            queue
                .entries()
                .front()
                .map(|entry| queue.read(entry, limit))
                .transpose()
        })
        .await
        .map_err(|e| Error::Io(io::Error::other(e)))?
    }
    pub async fn remove_oldest(&self) -> Result<(), Error> {
        let state = self.state.clone();
        let permit = state.reads.clone().acquire_owned().await.unwrap();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let mut queue = state.queue.lock().unwrap();
            if let Some(entry) = queue.entries().front().cloned() {
                queue.ack(&[entry])?;
            }
            state.refresh(&queue);
            Ok(())
        })
        .await
        .map_err(|e| Error::Io(io::Error::other(e)))?
    }

    pub async fn drain(&self) {
        let _permit = self.state.reads.clone().acquire_owned().await.unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn quota_and_read_errors_preserve_confirmed_files() {
        let directory = tempfile::tempdir().unwrap();
        let mut spool = Spool::open(directory.path(), 5).await.unwrap();
        spool.push(b"first", 1).await.unwrap();
        assert!(matches!(spool.push(b"next", 1).await, Err(Error::Full)));
        assert!(matches!(
            spool.oldest_limited(4).await,
            Err(Error::TooLarge)
        ));
        drop(spool);
        let spool = Spool::open(directory.path(), 5).await.unwrap();
        assert_eq!(spool.oldest().await.unwrap().unwrap(), b"first");
        assert_eq!(spool.len(), 1);
    }
}
