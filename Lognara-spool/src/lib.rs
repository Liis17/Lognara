//! Атомарная файловая очередь. Синхронные операции выполняются в blocking-работнике.

pub mod wire_budget;

use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub enum Error {
    Full,
    TooLarge,
    Io(io::Error),
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Full => f.write_str("spool quota is full"),
            Self::TooLarge => f.write_str("item exceeds spool limit"),
            Self::Io(error) => error.fmt(f),
        }
    }
}
impl std::error::Error for Error {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub seq: u64,
    pub part: u64,
    pub events: u64,
    pub size: u64,
    path: PathBuf,
}

pub struct Queue {
    dir: PathBuf,
    files: VecDeque<Entry>,
    bytes: u64,
    events: u64,
    max_bytes: u64,
    max_events: u64,
    max_entries: usize,
    next_seq: u64,
    // Каталог принадлежит одному процессу, в том числе во время восстановления.
    _lock: File,
}

impl Queue {
    pub fn open(dir: &Path, max_bytes: u64, max_events: u64) -> Result<Self, Error> {
        Self::open_limited(dir, max_bytes, max_events, 100_000)
    }

    pub fn open_limited(
        dir: &Path,
        max_bytes: u64,
        max_events: u64,
        max_entries: usize,
    ) -> Result<Self, Error> {
        let max_entries = max_entries.min(100_000);
        fs::create_dir_all(dir)?;
        let dir = fs::canonicalize(dir)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(".lock"))?;
        lock.try_lock().map_err(io::Error::other)?;
        for ancestor in dir.ancestors() {
            sync_dir(ancestor)?;
        }
        let mut files = Vec::new();
        for item in fs::read_dir(&dir)? {
            let item = item?;
            let path = item.path();
            if path.extension().is_some_and(|ext| ext == "tmp") {
                remove(&path)?;
            } else if let Some((seq, events)) = parse_part(&path) {
                files.push(Entry {
                    seq,
                    part: 0,
                    events,
                    size: item.metadata()?.len(),
                    path: PathBuf::from(item.file_name()),
                });
            } else if path.extension().is_some_and(|ext| ext == "group") {
                if path.file_stem().is_none_or(|name| name.len() > 20) {
                    return Err(invalid("invalid spool group name").into());
                }
                let seq = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| invalid("invalid spool group"))?;
                for part in fs::read_dir(&path)? {
                    let part = part?;
                    let part_path = part.path();
                    let (number, events) =
                        parse_part(&part_path).ok_or_else(|| invalid("invalid spool part"))?;
                    files.push(Entry {
                        seq,
                        part: number,
                        events,
                        size: part.metadata()?.len(),
                        path: path.strip_prefix(&dir).unwrap().join(part.file_name()),
                    });
                    if files.len() > max_entries {
                        return Err(Error::TooLarge);
                    }
                }
            }
            if files.len() > max_entries {
                return Err(Error::TooLarge);
            }
        }
        sync_dir(&dir)?;
        files.sort_by_key(|entry| (entry.seq, entry.part));
        let persisted = match fs::read_to_string(dir.join(".next")) {
            Ok(text) => text
                .parse::<u64>()
                .map_err(|_| invalid("invalid spool sequence"))?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => 0,
            Err(error) => return Err(error.into()),
        };
        let next_seq = files.last().map_or(Ok(persisted), |entry| {
            entry
                .seq
                .checked_add(1)
                .map(|n| n.max(persisted))
                .ok_or_else(|| invalid("spool sequence overflow"))
        })?;
        let bytes = files.iter().try_fold(0u64, |sum, entry| {
            sum.checked_add(entry.size)
                .ok_or_else(|| invalid("spool size overflow"))
        })?;
        let events = files.iter().try_fold(0u64, |sum, entry| {
            sum.checked_add(entry.events)
                .ok_or_else(|| invalid("spool count overflow"))
        })?;
        let queue = Self {
            dir,
            files: files.into(),
            bytes,
            events,
            max_bytes,
            max_events,
            max_entries,
            next_seq,
            _lock: lock,
        };
        atomic_file(&queue.dir, ".next", queue.next_seq.to_string().as_bytes())?;
        Ok(queue)
    }

    pub fn entries(&self) -> &VecDeque<Entry> {
        &self.files
    }
    pub fn events(&self) -> u64 {
        self.events
    }

    pub fn set_max_events(&mut self, limit: u64) {
        self.max_events = limit;
    }
    pub fn available(&self) -> bool {
        self.bytes < self.max_bytes
            && self.events < self.max_events
            && self.files.len() < self.max_entries
    }

    /// Все части публикуются одним rename; ошибка любой части отменяет всю группу.
    pub fn append_group(
        &mut self,
        parts: impl IntoIterator<Item = Result<(Vec<u8>, u64), Error>>,
    ) -> Result<(), Error> {
        let seq = self.next_seq;
        self.next_seq = seq
            .checked_add(1)
            .ok_or_else(|| invalid("spool sequence overflow"))?;
        atomic_file(&self.dir, ".next", self.next_seq.to_string().as_bytes())?;
        let tmp = self.dir.join(format!("{seq:020}.tmp"));
        let published = self.dir.join(format!("{seq:020}.group"));
        fs::create_dir(&tmp)?;
        let mut entries = Vec::new();
        let mut bytes = 0u64;
        let mut events = 0u64;
        let result = (|| {
            for part in parts {
                let (body, count) = part?;
                bytes = bytes
                    .checked_add(body.len() as u64)
                    .ok_or(Error::TooLarge)?;
                events = events.checked_add(count).ok_or(Error::TooLarge)?;
                if bytes > self.max_bytes || events > self.max_events {
                    return Err(Error::Full);
                }
                if bytes > self.max_bytes.saturating_sub(self.bytes)
                    || events > self.max_events.saturating_sub(self.events)
                    || self.files.len() + entries.len() >= self.max_entries
                {
                    return Err(Error::Full);
                }
                let number = entries.len() as u64;
                let name = format!("{number:020}-{count}.batch");
                let mut file = File::create(tmp.join(&name))?;
                file.write_all(&body)?;
                file.sync_all()?;
                entries.push(Entry {
                    seq,
                    part: number,
                    events: count,
                    size: body.len() as u64,
                    path: PathBuf::from(format!("{seq:020}.group")).join(name),
                });
            }
            if entries.is_empty() {
                return Err(invalid("empty spool group").into());
            }
            sync_dir(&tmp)?;
            fs::rename(&tmp, &published)?;
            // Даже при ошибке fsync опубликованные данные остаются в очереди.
            self.bytes += bytes;
            self.events += events;
            self.files.extend(entries.drain(..));
            sync_dir(&self.dir)?;
            Ok(())
        })();
        if result.is_err() && tmp.exists() {
            let _ = fs::remove_dir_all(&tmp);
        }
        result
    }

    pub fn read(&self, entry: &Entry, limit: usize) -> Result<Vec<u8>, Error> {
        let mut file = File::open(self.dir.join(&entry.path))?;
        let size = file.metadata()?.len();
        if size > limit as u64 {
            return Err(Error::TooLarge);
        }
        if size != entry.size {
            return Err(invalid("spool file size changed").into());
        }
        let capacity = usize::try_from(size)
            .ok()
            .and_then(|n| n.checked_add(1))
            .ok_or(Error::TooLarge)?;
        let mut body = vec![0; capacity];
        let mut read = 0;
        while read < capacity {
            let n = file.read(&mut body[read..])?;
            if n == 0 {
                break;
            }
            read += n;
        }
        if read as u64 != size {
            return Err(invalid("spool file changed while reading").into());
        }
        body.truncate(read);
        Ok(body)
    }

    /// Вызывается только после downstream ACK. При ошибке файл не пропускается.
    pub fn ack(&mut self, entries: &[Entry]) -> Result<(), Error> {
        for expected in entries {
            if self.files.front() != Some(expected) {
                return Err(invalid("out of order spool ACK").into());
            }
            let path = self.dir.join(&expected.path);
            fs::remove_file(&path)?;
            let parent = path.parent().unwrap();
            sync_dir(parent)?;
            self.files.pop_front();
            self.bytes -= expected.size;
            self.events -= expected.events;
            if parent != self.dir && fs::read_dir(parent)?.next().is_none() {
                fs::remove_dir(parent)?;
                sync_dir(&self.dir)?;
            }
        }
        Ok(())
    }

    /// Отдельный служебный резерв: полная квота очереди не мешает отправке.
    pub fn save_pending(&self, entries: &[Entry], body: &[u8], limit: usize) -> Result<(), Error> {
        if body.len() > limit {
            return Err(Error::TooLarge);
        }
        if entries.is_empty() || entries.len() > self.files.len() {
            return Err(invalid("empty pending batch").into());
        }
        let tmp = self.dir.join("pending.tmp");
        if tmp.exists() {
            fs::remove_dir_all(&tmp)?;
        }
        fs::create_dir(&tmp)?;
        let mut file = File::create(tmp.join("body"))?;
        file.write_all(body)?;
        file.sync_all()?;
        let mut ids = File::create(tmp.join("entries"))?;
        for entry in entries {
            ids.write_all(&entry.seq.to_le_bytes())?;
            ids.write_all(&entry.part.to_le_bytes())?;
        }
        ids.sync_all()?;
        sync_dir(&tmp)?;
        let pending = self.dir.join("pending");
        if pending.exists() {
            return Err(invalid("pending batch already exists").into());
        }
        fs::rename(tmp, pending)?;
        sync_dir(&self.dir)?;
        Ok(())
    }

    pub fn pending(&self, limit: usize) -> Result<Option<(Vec<u8>, Vec<Entry>)>, Error> {
        let dir = self.dir.join("pending");
        if !dir.exists() {
            return Ok(None);
        }
        let size = fs::metadata(dir.join("body"))?.len();
        if size > limit as u64 {
            return Err(Error::TooLarge);
        }
        let id_size = fs::metadata(dir.join("entries"))?.len();
        if id_size == 0 || id_size % 16 != 0 || id_size / 16 > self.max_entries as u64 {
            return Err(Error::TooLarge);
        }
        let mut ids = File::open(dir.join("entries"))?;
        let mut entries = Vec::new();
        let mut position = 0;
        let mut last_id = (0, 0);
        for _ in 0..id_size / 16 {
            let mut id = [0; 16];
            ids.read_exact(&mut id)?;
            let seq = u64::from_le_bytes(id[..8].try_into().unwrap());
            let part = u64::from_le_bytes(id[8..].try_into().unwrap());
            last_id = (seq, part);
            if let Some(entry) = self.files.get(position) {
                if (entry.seq, entry.part) == last_id {
                    entries.push(entry.clone());
                    position += 1;
                } else if (entry.seq, entry.part) < last_id {
                    return Err(invalid("pending entries do not match queue").into());
                }
            }
        }
        if entries.is_empty()
            && self
                .files
                .front()
                .is_some_and(|e| (e.seq, e.part) <= last_id)
        {
            return Err(invalid("missing pending entries").into());
        }
        let mut body = vec![0; size as usize];
        File::open(dir.join("body"))?.read_exact(&mut body)?;
        Ok(Some((body, entries)))
    }

    pub fn clear_pending(&self) -> Result<(), Error> {
        let pending = self.dir.join("pending");
        if pending.exists() {
            let removed = self.dir.join("pending-acked.tmp");
            fs::rename(pending, &removed)?;
            sync_dir(&self.dir)?;
            fs::remove_dir_all(removed)?;
            sync_dir(&self.dir)?;
        }
        Ok(())
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn sync_dir(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}
fn remove(path: &Path) -> io::Result<()> {
    if path.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}
fn atomic_file(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    let tmp = dir.join(format!("{name}.tmp"));
    let mut file = File::create(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(tmp, dir.join(name))?;
    sync_dir(dir)
}
fn parse_part(path: &Path) -> Option<(u64, u64)> {
    if path.file_name()?.len() > 64 || path.extension()? != "batch" {
        return None;
    }
    let (seq, events) = path.file_stem()?.to_str()?.split_once('-')?;
    Some((seq.parse().ok()?, events.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quota_rejection_retains_confirmed_data_after_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let mut queue = Queue::open(dir.path(), 6, 100).unwrap();
        queue.append_group([Ok((b"first".to_vec(), 1))]).unwrap();
        assert!(matches!(
            queue.append_group([Ok((b"second".to_vec(), 1))]),
            Err(Error::Full)
        ));
        drop(queue);
        let queue = Queue::open(dir.path(), 6, 100).unwrap();
        assert_eq!(queue.read(&queue.entries()[0], 100).unwrap(), b"first");
    }

    #[test]
    fn failed_last_part_never_publishes_first_part() {
        let dir = tempfile::tempdir().unwrap();
        let mut queue = Queue::open(dir.path(), 100, 100).unwrap();
        let result = queue.append_group([Ok((b"first".to_vec(), 1)), Err(Error::TooLarge)]);
        assert!(matches!(result, Err(Error::TooLarge)));
        drop(queue);
        assert!(
            Queue::open(dir.path(), 100, 100)
                .unwrap()
                .entries()
                .is_empty()
        );
    }

    #[test]
    fn unreadable_and_oversized_files_are_not_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let mut queue = Queue::open(dir.path(), 100, 100).unwrap();
        queue
            .append_group([Ok((b"first".to_vec(), 1)), Ok((b"second".to_vec(), 1))])
            .unwrap();
        let entry = queue.entries()[0].clone();
        assert!(matches!(queue.read(&entry, 4), Err(Error::TooLarge)));
        fs::rename(
            queue.dir.join(&entry.path),
            queue.dir.join(entry.path.with_extension("hidden")),
        )
        .unwrap();
        assert!(matches!(queue.read(&entry, 100), Err(Error::Io(_))));
        assert_eq!(queue.entries().len(), 2);
    }

    #[test]
    fn pending_replay_survives_partial_ack_and_restart_with_exact_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let mut queue = Queue::open(dir.path(), 10, 100).unwrap();
        queue
            .append_group([Ok((b"one".to_vec(), 1)), Ok((b"two".to_vec(), 1))])
            .unwrap();
        let entries: Vec<_> = queue.entries().iter().cloned().collect();
        queue
            .save_pending(&entries, b"stable transport", 32)
            .unwrap();
        queue.ack(&entries[..1]).unwrap();
        drop(queue);
        let mut queue = Queue::open(dir.path(), 10, 100).unwrap();
        let (body, remaining) = queue.pending(32).unwrap().unwrap();
        assert_eq!(body, b"stable transport");
        assert_eq!(remaining, entries[1..]);
        queue.ack(&remaining).unwrap();
        drop(queue);
        let queue = Queue::open(dir.path(), 10, 100).unwrap();
        assert!(queue.pending(32).unwrap().unwrap().1.is_empty());
        queue.clear_pending().unwrap();
        assert!(queue.pending(32).unwrap().is_none());
    }

    #[test]
    fn full_queue_can_prepare_pending_without_evicting_data() {
        let dir = tempfile::tempdir().unwrap();
        let mut queue = Queue::open(dir.path(), 3, 1).unwrap();
        queue.append_group([Ok((b"one".to_vec(), 1))]).unwrap();
        queue
            .save_pending(&[queue.entries()[0].clone()], b"body", 4)
            .unwrap();
        assert!(!queue.available());
        assert_eq!(queue.pending(4).unwrap().unwrap().0, b"body");
    }

    #[test]
    fn legacy_batches_and_incomplete_transactions_recover_in_order() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("00000000000000000004-2.batch"), b"legacy").unwrap();
        fs::create_dir(dir.path().join("00000000000000000005.tmp")).unwrap();
        fs::write(
            dir.path().join("00000000000000000005.tmp/0-1.batch"),
            b"unconfirmed",
        )
        .unwrap();
        let mut queue = Queue::open(dir.path(), 100, 100).unwrap();
        queue.append_group([Ok((b"new".to_vec(), 1))]).unwrap();
        assert_eq!(queue.read(&queue.entries()[0], 100).unwrap(), b"legacy");
        assert_eq!(queue.read(&queue.entries()[1], 100).unwrap(), b"new");
        assert_eq!(queue.events(), 3);
    }

    #[test]
    fn exclusive_directory_lock_prevents_competing_writers() {
        let dir = tempfile::tempdir().unwrap();
        let _queue = Queue::open(dir.path(), 100, 100).unwrap();
        assert!(Queue::open(dir.path(), 100, 100).is_err());
    }

    #[test]
    fn crash_worker() {
        let Ok(dir) = std::env::var("LOGNARA_TEST_CRASH_DIR") else {
            return;
        };
        let phase = std::env::var("LOGNARA_TEST_CRASH_PHASE").unwrap();
        let dir = PathBuf::from(dir);
        let mut queue = Queue::open(&dir, 100, 100).unwrap();
        if phase == "before-publication" {
            let mut first = true;
            let _ = queue.append_group(std::iter::from_fn(|| {
                if first {
                    first = false;
                    Some(Ok((b"unconfirmed".to_vec(), 1)))
                } else {
                    fs::write(dir.join("ready"), b"ready").unwrap();
                    loop {
                        std::thread::park();
                    }
                }
            }));
        } else {
            queue
                .append_group([Ok((b"durable-before-response".to_vec(), 1))])
                .unwrap();
            fs::write(dir.join("ready"), b"ready").unwrap();
            loop {
                std::thread::park();
            }
        }
    }

    #[test]
    fn sigkill_before_publication_and_after_commit_recovers_atomic_groups() {
        for phase in ["before-publication", "after-commit"] {
            let dir = tempfile::tempdir().unwrap();
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "tests::crash_worker", "--nocapture"])
                .env("LOGNARA_TEST_CRASH_DIR", dir.path())
                .env("LOGNARA_TEST_CRASH_PHASE", phase)
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !dir.path().join("ready").exists() {
                if std::time::Instant::now() > deadline {
                    child.kill().unwrap();
                    child.wait().unwrap();
                    panic!("crash worker not ready");
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            child.kill().unwrap();
            child.wait().unwrap();
            let queue = Queue::open(dir.path(), 100, 100).unwrap();
            if phase == "before-publication" {
                assert!(queue.entries().is_empty());
            } else {
                assert_eq!(
                    queue.read(&queue.entries()[0], 100).unwrap(),
                    b"durable-before-response"
                );
            }
        }
    }
    #[test]
    fn write_and_ack_errors_keep_previously_accepted_queue() {
        let dir = tempfile::tempdir().unwrap();
        let mut queue = Queue::open(dir.path(), 100, 100).unwrap();
        queue
            .append_group([Ok((b"confirmed".to_vec(), 1))])
            .unwrap();
        fs::create_dir(dir.path().join(".next.tmp")).unwrap();
        assert!(matches!(
            queue.append_group([Ok((b"new".to_vec(), 1))]),
            Err(Error::Io(_))
        ));
        assert_eq!(queue.events(), 1);
        let entry = queue.entries()[0].clone();
        fs::rename(
            queue.dir.join(&entry.path),
            queue.dir.join(entry.path.with_extension("hidden")),
        )
        .unwrap();
        assert!(matches!(queue.ack(&[entry]), Err(Error::Io(_))));
        assert_eq!(queue.events(), 1);
    }
    #[test]
    fn metadata_quota_is_enforced_without_losing_existing_files() {
        let dir = tempfile::tempdir().unwrap();
        let mut queue = Queue::open_limited(dir.path(), 100, 100, 2).unwrap();
        queue
            .append_group([Ok((b"one".to_vec(), 1)), Ok((b"two".to_vec(), 1))])
            .unwrap();
        assert!(!queue.available());
        assert!(matches!(
            queue.append_group([Ok((b"three".to_vec(), 1))]),
            Err(Error::Full)
        ));
        drop(queue);
        assert_eq!(
            Queue::open_limited(dir.path(), 100, 100, 2)
                .unwrap()
                .events(),
            2
        );
    }
}
