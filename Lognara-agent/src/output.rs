//! Неблокирующая копия в консоль: ОС может остановить только выделенный thread.
use std::io::{self, Write};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use tokio::sync::OwnedSemaphorePermit;

const CHUNK: usize = 4096;
const SLOTS: usize = 64;

#[derive(Default)]
struct State {
    pending: AtomicUsize,
    dropped: AtomicU64,
    // Permit живёт дольше Buffer, если консоль заблокировала worker.
    memory: Mutex<Option<Arc<OwnedSemaphorePermit>>>,
}

#[derive(Clone)]
pub struct Console {
    sender: mpsc::SyncSender<Vec<u8>>,
    state: Arc<State>,
}

impl Console {
    pub fn stderr() -> io::Result<Self> {
        Self::new(Box::new(io::stderr()))
    }
    pub(crate) fn stdout() -> io::Result<Self> {
        Self::new(Box::new(io::stdout()))
    }

    fn new(mut writer: Box<dyn Write + Send>) -> io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel::<Vec<u8>>(SLOTS);
        let state = Arc::new(State::default());
        let output = state.clone();
        std::thread::Builder::new()
            .name("lognara-console".into())
            .spawn(move || {
                for bytes in receiver {
                    if writer
                        .write_all(&bytes)
                        .and_then(|()| writer.flush())
                        .is_err()
                    {
                        output
                            .dropped
                            .fetch_add(bytes.len() as u64, Ordering::Relaxed);
                    }
                    output.pending.fetch_sub(1, Ordering::Release);
                }
                let _ = writer.flush();
            })?;
        Ok(Self { sender, state })
    }

    pub(crate) fn reserve(&self, memory: Arc<OwnedSemaphorePermit>) {
        *self.state.memory.lock().unwrap() = Some(memory);
    }

    pub(crate) fn echo(&self, bytes: &[u8]) {
        for part in bytes.chunks(CHUNK) {
            self.state.pending.fetch_add(1, Ordering::Relaxed);
            if self.sender.try_send(part.to_vec()).is_err() {
                self.state.pending.fetch_sub(1, Ordering::Release);
                self.state
                    .dropped
                    .fetch_add(part.len() as u64, Ordering::Relaxed);
            }
        }
    }

    pub fn dropped_bytes(&self) -> u64 {
        self.state.dropped.load(Ordering::Relaxed)
    }

    pub async fn drain(&self) {
        let _ = tokio::time::timeout(Duration::from_millis(200), async {
            while self.state.pending.load(Ordering::Acquire) > 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;
    }
}

impl Write for Console {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.echo(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
