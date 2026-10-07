//! Раздельные резервы приёма, накопленных моделей и отправки.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::OwnedSemaphorePermit;
use tokio::sync::Semaphore;

use crate::config::Config;

pub const MAX_BODY: usize = 64 << 20;
pub const MAX_DECODED: usize = 256 << 20;
pub const DEFAULT_MEMORY: usize = 1792 << 20;
pub const DEFAULT_MODEL: usize = 256 << 20;
pub const DEFAULT_BUFFER: usize = 256 << 20;
pub const CODEC_WORKSPACE: usize = 128 << 20;

pub async fn run_blocking<T: Send + 'static>(
    permit: OwnedSemaphorePermit,
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, tokio::task::JoinError> {
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work()
    })
    .await
}

pub struct Resources {
    pub slots: Arc<Semaphore>,
    pub buffered: Arc<Pool>,
    pub model_limit: usize,
    ready: AtomicBool,
    concurrency: usize,
}

impl Resources {
    pub fn new(config: &Config) -> Arc<Self> {
        Arc::new(Self {
            slots: Arc::new(Semaphore::new(config.max_ingest_concurrency)),
            buffered: Pool::new(config.max_buffer_bytes),
            model_limit: config.max_model_bytes,
            ready: AtomicBool::new(true),
            concurrency: config.max_ingest_concurrency,
        })
    }

    pub fn ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    pub fn set_ready(&self, ready: bool) {
        self.ready.store(ready, Ordering::Release);
    }

    /// После остановки HTTP ждём также работников отменённых запросов.
    pub async fn drain(&self) {
        self.set_ready(false);
        let _all = self
            .slots
            .clone()
            .acquire_many_owned(self.concurrency as u32)
            .await;
    }
}

#[derive(Debug)]
pub struct Pool {
    limit: usize,
    used: Mutex<usize>,
}

#[derive(Debug, PartialEq)]
pub enum ReserveError {
    TooLarge,
    Full,
}

impl Pool {
    pub fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            limit,
            used: Mutex::new(0),
        })
    }

    pub fn reserve(self: &Arc<Self>, bytes: usize) -> Result<Reservation, ReserveError> {
        if bytes > self.limit {
            return Err(ReserveError::TooLarge);
        }
        let mut used = self.used.lock().unwrap();
        if bytes > self.limit - *used {
            return Err(ReserveError::Full);
        }
        *used += bytes;
        Ok(Reservation {
            pool: self.clone(),
            bytes,
        })
    }

    pub fn used(&self) -> usize {
        *self.used.lock().unwrap()
    }
}

#[derive(Debug)]
pub struct Reservation {
    pool: Arc<Pool>,
    bytes: usize,
}

impl Drop for Reservation {
    fn drop(&mut self) {
        *self.pool.used.lock().unwrap() -= self.bytes;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reservations_release_capacity_and_distinguish_permanent_overflow() {
        let pool = Pool::new(100);
        let first = pool.reserve(60).unwrap();
        assert!(matches!(pool.reserve(50), Err(ReserveError::Full)));
        assert!(matches!(pool.reserve(101), Err(ReserveError::TooLarge)));
        drop(first);
        assert!(pool.reserve(100).is_ok());
        assert_eq!(pool.used(), 0);
    }

    #[tokio::test]
    async fn cancelled_awaiter_keeps_worker_reservation_until_completion() {
        let slots = Arc::new(Semaphore::new(1));
        let permit = slots.clone().try_acquire_owned().unwrap();
        let (started, begun) = tokio::sync::oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let request = tokio::spawn(run_blocking(permit, move || {
            started.send(()).unwrap();
            wait.recv().unwrap();
        }));
        begun.await.unwrap();
        request.abort();
        let _ = request.await;
        assert!(slots.clone().try_acquire_owned().is_err());
        release.send(()).unwrap();
        let permit = tokio::time::timeout(std::time::Duration::from_secs(1), slots.acquire_owned())
            .await
            .unwrap()
            .unwrap();
        drop(permit);
    }
}
