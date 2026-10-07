//! Дисковая очередь пачек, которые не удалось отправить в core.
//!
//! Каждая пачка лежит в своём файле `{seq:020}-{events}.batch` с телом запроса в core как есть.
//! По `seq` пачки уходят от старых к новым, `events` нужен для учёта потерь при вытеснении.
//! Ошибки диска во время работы пишутся в лог, а отправка продолжается.

use std::collections::VecDeque;
use std::io;
use std::path::{Path, PathBuf};

use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{error, warn};

pub struct Spool {
    dir: PathBuf,
    max_bytes: u64,
    files: VecDeque<Entry>,
    /// Суммарный размер файлов в очереди.
    bytes: u64,
    next_seq: u64,
    /// Потерянные relay события, ещё не учтённые в пачке для core.
    dropped: u64,
    /// Такие файлы нельзя вытеснять по дисковой квоте до исправления причины.
    replay_limit: usize,
}

#[derive(Clone, Copy)]
struct Entry {
    seq: u64,
    events: u64,
    size: u64,
}

#[derive(Debug)]
pub struct ReadTooLarge {
    pub size: u64,
    pub limit: usize,
}

impl Spool {
    /// Открывает каталог: создаёт его при необходимости, удаляет недописанные `*.tmp`
    /// и восстанавливает очередь из пачек, оставшихся с прошлого запуска.
    pub async fn open(dir: &Path, max_bytes: u64) -> io::Result<Self> {
        fs::create_dir_all(dir).await?;
        let mut files = Vec::new();
        let mut entries = fs::read_dir(dir).await?;
        while let Some(item) = entries.next_entry().await? {
            let path = item.path();
            if path.extension().is_some_and(|extension| extension == "tmp") {
                fs::remove_file(&path).await?;
            } else if let Some((seq, events)) = parse_name(&path) {
                let size = item.metadata().await?.len();
                files.push(Entry { seq, events, size });
            }
        }
        files.sort_by_key(|entry| entry.seq);

        Ok(Self {
            dir: dir.to_owned(),
            max_bytes,
            next_seq: files.last().map_or(0, |entry| entry.seq + 1),
            bytes: files.iter().map(|entry| entry.size).sum(),
            files: files.into(),
            dropped: 0,
            replay_limit: usize::MAX,
        })
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Сколько пачек ждёт отправки.
    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn set_replay_limit(&mut self, limit: usize) {
        self.replay_limit = limit;
    }

    pub fn fits_replay_limit(&self) -> bool {
        self.files
            .iter()
            .all(|entry| entry.size <= self.replay_limit as u64)
    }

    /// Дописывает пачку в конец очереди. Если места не хватает, сначала удаляет
    /// самые старые пачки. Потерянные события учитываются в `dropped`.
    pub async fn push(&mut self, body: &[u8], events: u64) {
        let size = body.len() as u64;
        while self.bytes.saturating_add(size) > self.max_bytes {
            let Some(entry) = self.files.front().copied() else {
                break;
            };
            if let Ok(metadata) = fs::metadata(self.path(entry)).await {
                self.update_oldest_size(metadata.len());
            }
            // Временно превышаем дисковую квоту, чтобы сохранить уже принятые
            // данные, не удаляя файл, который заблокировал новый приём.
            if self.files.front().unwrap().size > self.replay_limit as u64 {
                break;
            }
            let evicted = self.pop().await.unwrap();
            self.dropped += evicted.events;
            warn!(
                events = evicted.events,
                "spool is full, dropped oldest batch"
            );
        }

        let entry = Entry {
            seq: self.next_seq,
            events,
            size,
        };
        match self.write(entry, body).await {
            Ok(()) => {
                self.next_seq += 1;
                self.bytes += size;
                self.files.push_back(entry);
            }
            Err(err) => {
                self.dropped += events;
                error!(error = %err, events, "failed to save batch to spool, batch is lost");
            }
        }
    }

    /// Тело самой старой пачки. Пачку, которую не удалось прочитать, пропускает.
    pub async fn oldest(&mut self) -> Option<Vec<u8>> {
        self.oldest_limited(usize::MAX).await.ok().flatten()
    }

    /// Проверяет размер до выделения; слишком большой файл остаётся на диске.
    pub async fn oldest_limited(&mut self, limit: usize) -> Result<Option<Vec<u8>>, ReadTooLarge> {
        while let Some(entry) = self.files.front().copied() {
            let read = async {
                let mut file = fs::File::open(self.path(entry)).await?;
                let size = file.metadata().await?.len();
                let Some(capacity) = usize::try_from(size).ok().and_then(|n| n.checked_add(1))
                else {
                    return Ok::<_, io::Error>(Err(ReadTooLarge { size, limit }));
                };
                if size > limit as u64 {
                    return Ok(Err(ReadTooLarge { size, limit }));
                }
                let mut body = vec![0; capacity];
                let mut length = 0;
                while length < capacity {
                    let count = file.read(&mut body[length..]).await?;
                    if count == 0 {
                        break;
                    }
                    length += count;
                }
                if length as u64 > size {
                    // Файл вырос после metadata: не читаем растущий файл без лимита.
                    return Ok(Err(ReadTooLarge {
                        size: length as u64,
                        limit,
                    }));
                }
                body.truncate(length);
                Ok(Ok(body))
            }
            .await;
            match read {
                Ok(Ok(body)) => {
                    self.update_oldest_size(body.len() as u64);
                    return Ok(Some(body));
                }
                Ok(Err(error)) => {
                    // Размер мог измениться на диске после open; защита от
                    // вытеснения должна учитывать и обнаруженный новый размер.
                    self.update_oldest_size(error.size);
                    return Err(error);
                }
                Err(err) => {
                    self.dropped += entry.events;
                    error!(error = %err, events = entry.events, "failed to read batch from spool, batch is lost");
                    self.pop().await;
                }
            }
        }
        Ok(None)
    }

    /// Удаляет самую старую пачку, например после доставки.
    pub async fn remove_oldest(&mut self) {
        self.pop().await;
    }

    /// Забирает число потерянных событий, чтобы сообщить его core.
    pub fn take_dropped(&mut self) -> u64 {
        std::mem::take(&mut self.dropped)
    }

    /// Учитывает события, которые невозможно отправить из-за лимитов core.
    pub fn record_dropped(&mut self, events: u64) {
        self.dropped = self.dropped.saturating_add(events);
    }

    async fn pop(&mut self) -> Option<Entry> {
        let entry = self.files.pop_front()?;
        self.bytes -= entry.size;
        if let Err(err) = fs::remove_file(self.path(entry)).await {
            error!(error = %err, "failed to remove batch from spool");
        }
        Some(entry)
    }

    fn update_oldest_size(&mut self, size: u64) {
        let entry = self.files.front_mut().unwrap();
        self.bytes = self.bytes.saturating_sub(entry.size).saturating_add(size);
        entry.size = size;
    }

    /// Пишет через временный файл, чтобы после падения не осталось обрезанной пачки.
    async fn write(&self, entry: Entry, body: &[u8]) -> io::Result<()> {
        let path = self.path(entry);
        let tmp = path.with_extension("tmp");
        let mut file = fs::File::create(&tmp).await?;
        file.write_all(body).await?;
        file.sync_all().await?;
        fs::rename(&tmp, &path).await
    }

    fn path(&self, entry: Entry) -> PathBuf {
        self.dir
            .join(format!("{:020}-{}.batch", entry.seq, entry.events))
    }
}

/// Разбирает имя файла `{seq}-{events}.batch`.
fn parse_name(path: &Path) -> Option<(u64, u64)> {
    if path.extension()? != "batch" {
        return None;
    }
    let (seq, events) = path.file_stem()?.to_str()?.split_once('-')?;
    Some((seq.parse().ok()?, events.parse().ok()?))
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn oversized_replay_is_retained_until_it_fits_the_reserve() {
        let directory = tempfile::tempdir().unwrap();
        let mut spool = super::Spool::open(directory.path(), 1000).await.unwrap();
        spool.push(b"large", 1).await;
        let error = spool.oldest_limited(4).await.unwrap_err();
        assert_eq!(error.size, 5);
        assert_eq!(spool.len(), 1);
        assert_eq!(spool.take_dropped(), 0);
        assert_eq!(spool.oldest_limited(5).await.unwrap().unwrap(), b"large");
    }

    #[tokio::test]
    async fn oversized_file_behind_a_small_head_survives_quota_and_shutdown_saves() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("00000000000000000000-1.batch"), b"small").unwrap();
        let protected = dir.path().join("00000000000000000001-7.batch");
        std::fs::write(&protected, b"oversized").unwrap();
        let mut spool = Spool::open(dir.path(), 6).await.unwrap();
        spool.set_replay_limit(5);
        assert!(!spool.fits_replay_limit());
        spool.push(b"saved", 2).await;
        assert_eq!(std::fs::read(&protected).unwrap(), b"oversized");
        assert_eq!(spool.len(), 2);
        assert_eq!(spool.take_dropped(), 1);
        assert_eq!(spool.oldest_limited(5).await.unwrap_err().size, 9);
        spool.push(b"last", 3).await;
        assert_eq!(spool.len(), 3);
        std::fs::write(&protected, b"fixed").unwrap();
        assert_eq!(spool.oldest_limited(5).await.unwrap().unwrap(), b"fixed");
        assert!(spool.fits_replay_limit());
        assert_eq!(drain(&mut spool).await, [&b"fixed"[..], b"saved", b"last"]);
    }

    #[tokio::test]
    async fn quota_eviction_checks_a_file_that_grew_since_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("00000000000000000000-7.batch");
        std::fs::write(&path, b"small").unwrap();
        let mut spool = Spool::open(dir.path(), 6).await.unwrap();
        spool.set_replay_limit(5);
        std::fs::write(&path, b"oversized").unwrap();
        spool.push(b"saved", 2).await;
        assert_eq!(std::fs::read(&path).unwrap(), b"oversized");
        assert!(!spool.fits_replay_limit());
        assert_eq!(spool.take_dropped(), 0);
        assert_eq!(spool.len(), 2);
    }
    use super::*;

    const UNLIMITED: u64 = u64::MAX;

    async fn drain(spool: &mut Spool) -> Vec<Vec<u8>> {
        let mut bodies = Vec::new();
        while let Some(body) = spool.oldest().await {
            bodies.push(body);
            spool.remove_oldest().await;
        }
        bodies
    }

    fn files(dir: &Path) -> Vec<String> {
        let mut names: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|item| item.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    #[tokio::test]
    async fn returns_batches_oldest_first() {
        let dir = tempfile::tempdir().unwrap();
        let mut spool = Spool::open(dir.path(), UNLIMITED).await.unwrap();

        spool.push(b"first", 1).await;
        spool.push(b"second", 2).await;
        spool.push(b"third", 3).await;

        assert_eq!(spool.len(), 3);
        assert_eq!(
            drain(&mut spool).await,
            [&b"first"[..], b"second", b"third"]
        );
        assert!(spool.is_empty());
        assert!(files(dir.path()).is_empty());
    }

    #[tokio::test]
    async fn restores_queue_after_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let mut spool = Spool::open(dir.path(), UNLIMITED).await.unwrap();
        spool.push(b"first", 1).await;
        spool.push(b"second", 2).await;
        drop(spool);
        std::fs::write(dir.path().join("00000000000000000002-5.tmp"), b"cut").unwrap();
        std::fs::write(dir.path().join("notes.txt"), b"not a batch").unwrap();

        let mut spool = Spool::open(dir.path(), UNLIMITED).await.unwrap();
        spool.push(b"third", 3).await;

        assert_eq!(
            files(dir.path()),
            [
                "00000000000000000000-1.batch",
                "00000000000000000001-2.batch",
                "00000000000000000002-3.batch",
                "notes.txt",
            ]
        );
        assert_eq!(
            drain(&mut spool).await,
            [&b"first"[..], b"second", b"third"]
        );
    }

    #[tokio::test]
    async fn evicts_oldest_batches_when_full() {
        let dir = tempfile::tempdir().unwrap();
        let mut spool = Spool::open(dir.path(), 10).await.unwrap();

        spool.push(b"aaaa", 1).await;
        spool.push(b"bbbb", 2).await;
        spool.push(b"cccc", 4).await;

        assert_eq!(spool.take_dropped(), 1);
        assert_eq!(spool.take_dropped(), 0);
        assert_eq!(drain(&mut spool).await, [&b"bbbb"[..], b"cccc"]);
    }

    #[tokio::test]
    async fn skips_unreadable_batch() {
        let dir = tempfile::tempdir().unwrap();
        let mut spool = Spool::open(dir.path(), UNLIMITED).await.unwrap();
        spool.push(b"lost", 3).await;
        spool.push(b"kept", 1).await;
        std::fs::remove_file(dir.path().join("00000000000000000000-3.batch")).unwrap();

        assert_eq!(drain(&mut spool).await, [&b"kept"[..]]);
        assert_eq!(spool.take_dropped(), 3);
    }
}
