//! Файловый источник: durable append предшествует checkpoint.
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::buffer::Buffer;
use crate::config::Config;
use crate::framing::{Continuation, Framed, Framer};
use crate::sources::{MAX_FILES, MAX_OPEN_FILES, Source, SourceKind, StartAt, validate_pattern};

const STATE_LIMIT: u64 = 2 << 20;
const QUIET: Duration = Duration::from_secs(5);

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Fingerprint {
    length: usize,
    hash: [u8; 32],
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Position {
    source: String,
    path: String,
    device: u64,
    inode: u64,
    prefix: Fingerprint,
    tail: Fingerprint,
    offset: u64,
    continuation: Option<Continuation>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Checkpoint {
    version: u32,
    files: Vec<Position>,
}

struct Tracked {
    position: Position,
    file: Option<File>,
    size: u64,
    modified: Option<std::time::SystemTime>,
    changed: Instant,
    rotated: bool,
    seen: bool,
}

pub(crate) struct Files {
    sources: Vec<Source>,
    tracked: Vec<Tracked>,
    spool: PathBuf,
    dirty: bool,
    next: usize,
    scan: Instant,
    warning: Option<Instant>,
    initialized: bool,
}

impl Files {
    pub(crate) async fn open(config: &Config) -> io::Result<Self> {
        let spool = config.spool_dir.clone();
        let sources = config
            .sources
            .sources
            .iter()
            .filter(|s| s.kind == SourceKind::File)
            .cloned()
            .collect();
        tokio::task::spawn_blocking(move || Self::load(spool, sources))
            .await
            .map_err(io::Error::other)?
    }

    fn load(spool: PathBuf, sources: Vec<Source>) -> io::Result<Self> {
        let spool = fs::canonicalize(spool)?;
        let path = spool.join("sources.checkpoint");
        let checkpoint = match File::open(path) {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(STATE_LIMIT + 1).read_to_end(&mut bytes)?;
                if bytes.len() as u64 > STATE_LIMIT {
                    return Err(invalid("checkpoint exceeds 2 MiB"));
                }
                let state: Checkpoint = serde_json::from_slice(&bytes).map_err(invalid)?;
                if state.version != 1 || state.files.len() > MAX_FILES {
                    return Err(invalid("unsupported checkpoint version or file count"));
                }
                let mut identities = BTreeSet::new();
                for position in &state.files {
                    if !Path::new(&position.path).is_absolute()
                        || position.path.len() > 4096
                        || position.source.len() > 64
                        || position.prefix.length > 1024
                        || position.tail.length > 256
                        || position.tail.length as u64 > position.offset
                        || !identities.insert((position.device, position.inode))
                    {
                        return Err(invalid("invalid file checkpoint"));
                    }
                }
                state
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Checkpoint {
                version: 1,
                files: Vec::new(),
            },
            Err(error) => return Err(error),
        };
        let tracked = checkpoint
            .files
            .into_iter()
            .map(|position| Tracked {
                size: position.offset,
                position,
                file: None,
                modified: None,
                changed: Instant::now(),
                rotated: false,
                seen: false,
            })
            .collect();
        Ok(Self {
            sources,
            tracked,
            spool,
            dirty: false,
            next: 0,
            scan: Instant::now() - Duration::from_secs(1),
            warning: None,
            initialized: false,
        })
    }

    pub(crate) async fn run(mut self, buffer: Arc<Buffer>, stop: CancellationToken) {
        if self.sources.is_empty() {
            return;
        }
        loop {
            if stop.is_cancelled() {
                break;
            }
            if let Ok(admission) = buffer.admit_source() {
                let writer = buffer.clone();
                let cancel = stop.clone();
                let task = tokio::task::spawn_blocking(move || {
                    let _admission = admission;
                    if let Err(error) = self.tick(&writer, &cancel) {
                        self.report(&error.to_string());
                    }
                    self
                });
                match task.await {
                    Ok(state) => self = state,
                    Err(error) => {
                        warn!(error = %error, "file collector failed; source files retained");
                        break;
                    }
                }
            }
            tokio::select! { _ = stop.cancelled() => break, _ = tokio::time::sleep(Duration::from_millis(100)) => {} }
        }
    }

    fn tick(&mut self, buffer: &Buffer, stop: &CancellationToken) -> io::Result<()> {
        // При отказе checkpoint не добавляем уже сохранённую порцию второй раз.
        if self.dirty {
            self.save()?;
        }
        if self.scan.elapsed() >= Duration::from_secs(1) {
            self.discover()?;
            self.scan = Instant::now();
            if self.dirty {
                self.save()?;
            }
        }
        if self.tracked.is_empty() || buffer.file_pressure() || stop.is_cancelled() {
            return Ok(());
        }
        self.next %= self.tracked.len();
        let index = self.next;
        self.next += 1;
        self.read(index, buffer, stop)
    }

    fn discover(&mut self) -> io::Result<()> {
        let mut directories = BTreeSet::new();
        for source in &self.sources {
            for pattern in &source.include {
                directories.insert(validate_pattern(pattern)?);
            }
        }
        for entry in &mut self.tracked {
            entry.seen = false;
            if let Some(parent) = Path::new(&entry.position.path).parent() {
                directories.insert(parent.to_path_buf());
            }
        }
        for directory in directories {
            let items = match fs::read_dir(&directory) {
                Ok(items) => items,
                Err(error) => {
                    self.report(&format!("cannot scan {}: {error}", directory.display()));
                    self.preserve_directory(&directory);
                    continue;
                }
            };
            for item in items {
                let path = match item {
                    Ok(item) => item.path(),
                    Err(error) => {
                        self.report(&error.to_string());
                        self.preserve_directory(&directory);
                        continue;
                    }
                };
                if compressed(&path) {
                    continue;
                }
                let metadata = match fs::metadata(&path) {
                    Ok(m) => m,
                    Err(error) => {
                        self.report(&error.to_string());
                        self.preserve_directory(&directory);
                        continue;
                    }
                };
                let matches: Vec<_> = self
                    .sources
                    .iter()
                    .enumerate()
                    .filter(|(_, source)| matches_source(source, &path))
                    .map(|(index, _)| index)
                    .collect();
                let existing = self.tracked.iter().position(|entry| {
                    entry.position.inode == metadata.ino()
                        && entry.position.device == metadata.dev()
                });
                // После reboot st_dev может поменяться; inode+содержимое проверяем ниже.
                let mut recovery = existing.or_else(|| {
                    self.tracked.iter().position(|entry| {
                        entry.position.inode == metadata.ino()
                            && (entry.position.path == path.to_string_lossy()
                                || self.sources.iter().any(|s| s.id == entry.position.source))
                    })
                });
                if matches.is_empty() && recovery.is_none() {
                    continue;
                }
                if !metadata.is_file() {
                    self.report(&format!("not a regular log file: {}", path.display()));
                    continue;
                }
                let canonical = match fs::canonicalize(&path) {
                    Ok(p) => p,
                    Err(error) => {
                        self.report(&error.to_string());
                        continue;
                    }
                };
                if canonical.starts_with(&self.spool) {
                    continue;
                }
                let Some(name) = canonical.to_str().filter(|name| name.len() <= 4096) else {
                    self.report("log path is not UTF-8 or exceeds 4096 bytes");
                    continue;
                };
                if existing.is_none()
                    && let Some(index) = recovery
                {
                    if !self.make_room() {
                        recovery = None;
                    } else {
                        let position = &self.tracked[index].position;
                        let verified = open_regular(&canonical)
                            .and_then(|mut file| fingerprint(&mut file, 0, position.prefix.length))
                            .is_ok_and(|fingerprint| {
                                fingerprint.hash == position.prefix.hash
                                    && fingerprint.length == position.prefix.length
                            });
                        if !verified {
                            recovery = None;
                        }
                    }
                }
                if matches.len() > 1 {
                    self.report(&format!(
                        "file matches multiple sources; first definition wins: {}",
                        path.display()
                    ));
                }
                if let Some(index) = recovery {
                    let same_owner = matches.first().is_none_or(|&owner| {
                        self.sources[owner].id == self.tracked[index].position.source
                    });
                    if !same_owner {
                        self.report("one physical log file is already owned by another source");
                    }
                    self.tracked[index].seen = true;
                    let old = &self.tracked[index].position;
                    let old_path_alive = fs::metadata(&old.path)
                        .is_ok_and(|m| m.ino() == metadata.ino() && m.dev() == metadata.dev());
                    if !old_path_alive || old.device != metadata.dev() {
                        self.tracked[index].rotated |= old.path != name;
                        self.tracked[index].position.path = name.to_owned();
                        self.tracked[index].position.device = metadata.dev();
                        self.dirty = true;
                    }
                    continue;
                }
                let Some(&owner) = matches.first() else {
                    continue;
                };
                if self.tracked.len() == MAX_FILES {
                    self.report("256 tracked-file limit reached; new files are waiting");
                    continue;
                }
                if !self.make_room() {
                    self.report("64 open-file limit reached during discovery; files are waiting");
                    continue;
                }
                let mut file = match open_regular(&canonical) {
                    Ok(file) => file,
                    Err(error) => {
                        self.report(&error.to_string());
                        continue;
                    }
                };
                let metadata = file.metadata()?;
                // Защита от замены пути между metadata и open.
                if self.tracked.iter().any(|e| {
                    e.position.device == metadata.dev() && e.position.inode == metadata.ino()
                }) {
                    continue;
                }
                let offset = if !self.initialized && self.sources[owner].start_at == StartAt::End {
                    metadata.len()
                } else {
                    0
                };
                let prefix = fingerprint(&mut file, 0, metadata.len().min(1024) as usize)?;
                let tail = fingerprint(
                    &mut file,
                    offset.saturating_sub(256),
                    offset.min(256) as usize,
                )?;
                self.tracked.push(Tracked {
                    position: Position {
                        source: self.sources[owner].id.clone(),
                        path: name.to_owned(),
                        device: metadata.dev(),
                        inode: metadata.ino(),
                        prefix,
                        tail,
                        offset,
                        continuation: None,
                    },
                    file: None,
                    size: metadata.len(),
                    modified: metadata.modified().ok(),
                    changed: Instant::now(),
                    rotated: false,
                    seen: true,
                });
                self.dirty = true;
            }
        }
        let mut missing = false;
        self.initialized = true;
        self.tracked.retain(|entry| {
            let retired =
                !entry.seen && entry.file.is_none() && entry.position.offset >= entry.size;
            missing |= !entry.seen;
            self.dirty |= retired;
            !retired
        });
        if missing {
            self.report(
                "a tracked log file is missing; keep rotations uncompressed until collected",
            );
        }
        Ok(())
    }

    // Ошибка обхода не доказывает удаление файла: сохраняем позиции до удачного scan.
    fn preserve_directory(&mut self, directory: &Path) {
        for entry in &mut self.tracked {
            if Path::new(&entry.position.path).parent() == Some(directory) {
                entry.seen = true;
            }
        }
    }

    fn read(&mut self, index: usize, buffer: &Buffer, stop: &CancellationToken) -> io::Result<()> {
        let Some(source) = self
            .sources
            .iter()
            .find(|s| s.id == self.tracked[index].position.source)
            .cloned()
        else {
            return Ok(());
        };
        // Checkpoint восстанавливает ротации вне include, но не отменяет явный exclude.
        if excluded(&source, Path::new(&self.tracked[index].position.path)) {
            self.tracked[index].file = None;
            return Ok(());
        }
        if self.tracked[index].file.is_none() {
            if !self.make_room() {
                self.report("64 open-file limit reached; files are waiting");
                return Ok(());
            }
            self.tracked[index].file =
                Some(open_regular(Path::new(&self.tracked[index].position.path))?);
        }
        let mut file = self.tracked[index].file.take().unwrap();
        let result = self.read_open(index, &source, &mut file, buffer, stop);
        let position = &self.tracked[index].position;
        let reachable = fs::metadata(&position.path).is_ok_and(|metadata| {
            metadata.ino() == position.inode && metadata.dev() == position.device
        });
        if self.tracked[index].changed.elapsed() < QUIET
            || (!reachable && position.offset < self.tracked[index].size)
        {
            self.tracked[index].file = Some(file);
        }
        result
    }

    /// Round-robin чтение сохраняет справедливость при числе файлов > FD-лимита.
    /// Доступный по пути файл можно открыть снова; недоступный старый inode удерживаем.
    fn make_room(&mut self) -> bool {
        if self
            .tracked
            .iter()
            .filter(|entry| entry.file.is_some())
            .count()
            < MAX_OPEN_FILES
        {
            return true;
        }
        if let Some(entry) = self.tracked.iter_mut().find(|entry| {
            entry.file.is_some()
                && fs::metadata(&entry.position.path).is_ok_and(|metadata| {
                    metadata.ino() == entry.position.inode
                        && metadata.dev() == entry.position.device
                })
        }) {
            entry.file = None;
            true
        } else {
            false
        }
    }

    fn read_open(
        &mut self,
        index: usize,
        source: &Source,
        file: &mut File,
        buffer: &Buffer,
        stop: &CancellationToken,
    ) -> io::Result<()> {
        let metadata = file.metadata()?;
        let old = &self.tracked[index].position;
        let prefix = fingerprint(file, 0, old.prefix.length.min(metadata.len() as usize))?;
        let tail = fingerprint(
            file,
            old.offset.saturating_sub(old.tail.length as u64),
            old.tail.length,
        )?;
        let reset = metadata.ino() != old.inode
            || metadata.len() < old.offset
            || prefix.hash != old.prefix.hash
            || tail.hash != old.tail.hash;
        if reset {
            self.report("log file was truncated/replaced; resuming from the beginning");
            let entry = &mut self.tracked[index];
            entry.position.offset = 0;
            entry.position.continuation = None;
            entry.position.device = metadata.dev();
            entry.position.inode = metadata.ino();
            entry.position.prefix = fingerprint(file, 0, metadata.len().min(1024) as usize)?;
            entry.position.tail = fingerprint(file, 0, 0)?;
            self.dirty = true;
        } else if old.prefix.length == 0 && metadata.len() > 0 {
            self.tracked[index].position.prefix =
                fingerprint(file, 0, metadata.len().min(1024) as usize)?;
            self.dirty = true;
        }
        let entry = &mut self.tracked[index];
        if entry.size != metadata.len() || entry.modified != metadata.modified().ok() {
            entry.changed = Instant::now();
        }
        entry.size = metadata.len();
        entry.modified = metadata.modified().ok();
        if !fs::metadata(&entry.position.path)
            .is_ok_and(|m| m.ino() == metadata.ino() && m.dev() == metadata.dev())
        {
            entry.rotated = true;
        }
        let mut framer = Framer::new(
            source,
            buffer.source_record_limit(),
            entry.position.offset,
            entry.position.continuation.clone(),
        )?;
        file.seek(SeekFrom::Start(entry.position.offset))?;
        let mut chunk = [0u8; 4096];
        let mut remaining = buffer.source_record_limit() + 4096;
        while remaining > 0 && !stop.is_cancelled() && !buffer.file_pressure() {
            let count = file.read(&mut chunk[..remaining.min(4096)])?;
            if count == 0 {
                let frames = if self.tracked[index].rotated
                    && self.tracked[index].changed.elapsed() >= QUIET
                {
                    framer.finish_all()
                } else if source.multiline.as_ref().is_some_and(|m| {
                    self.tracked[index].changed.elapsed()
                        >= Duration::from_millis(m.flush_timeout_ms)
                }) {
                    framer.idle().into_iter().collect()
                } else {
                    Vec::new()
                };
                self.persist(index, source, file, frames, buffer)?;
                break;
            }
            remaining -= count;
            let frames = framer.feed(&chunk[..count]);
            self.persist(index, source, file, frames, buffer)?;
            // fingerprint меняет позицию дескриптора; возвращаемся к концу прочитанной порции.
            file.seek(SeekFrom::Start(framer_position(
                &framer,
                &self.tracked[index].position,
            )))?;
        }
        if self.dirty {
            self.save()?;
        }
        Ok(())
    }

    fn persist(
        &mut self,
        index: usize,
        source: &Source,
        file: &mut File,
        frames: Vec<Framed>,
        buffer: &Buffer,
    ) -> io::Result<()> {
        let mut frames = frames.into_iter();
        loop {
            let group: Vec<_> = frames
                .by_ref()
                .take(32.min(buffer.record_limit()).max(1))
                .collect();
            let Some(last) = group.last() else {
                break;
            };
            let offset = last.end;
            let continuation = last.continuation.clone();
            if group.iter().any(|r| r.invalid_json) {
                self.report("invalid NDJSON preserved as text");
            }
            let path = self.tracked[index].position.path.clone();
            let tail = fingerprint(file, offset.saturating_sub(256), offset.min(256) as usize)?;
            let records = group
                .into_iter()
                .map(|frame| frame.into_record(source, Some(&path)))
                .collect();
            buffer.push(records).map_err(io::Error::other)?;
            // In-memory позиция обновляется сразу после успешного durable append.
            self.tracked[index].position.offset = offset;
            self.tracked[index].position.continuation = continuation;
            self.tracked[index].position.tail = tail;
            self.dirty = true;
            self.save()?;
        }
        Ok(())
    }

    fn save(&mut self) -> io::Result<()> {
        let checkpoint = Checkpoint {
            version: 1,
            files: self
                .tracked
                .iter()
                .map(|entry| entry.position.clone())
                .collect(),
        };
        let tmp = self.spool.join("sources.checkpoint.tmp");
        let mut file = File::create(&tmp)?;
        serde_json::to_writer(&mut file, &checkpoint).map_err(io::Error::other)?;
        file.flush()?;
        file.sync_all()?;
        fs::rename(tmp, self.spool.join("sources.checkpoint"))?;
        File::open(&self.spool)?.sync_all()?;
        self.dirty = false;
        Ok(())
    }

    fn report(&mut self, message: &str) {
        if self
            .warning
            .is_none_or(|time| time.elapsed() >= Duration::from_secs(10))
        {
            warn!(message, "file source diagnostic");
            self.warning = Some(Instant::now());
        }
    }
}

fn framer_position(framer: &Framer, position: &Position) -> u64 {
    position.offset + framer.pending_bytes() as u64
}

fn matches_source(source: &Source, path: &Path) -> bool {
    source
        .include
        .iter()
        .any(|p| glob::Pattern::new(p).is_ok_and(|p| p.matches_path(path)))
        && !excluded(source, path)
}

fn excluded(source: &Source, path: &Path) -> bool {
    source.exclude.iter().any(|pattern| {
        if glob::Pattern::new(pattern).is_ok_and(|p| p.matches_path(path)) {
            return true;
        }
        // Позиции используют canonical path, а настройки могут содержать alias каталога.
        let pattern = Path::new(pattern);
        pattern
            .parent()
            .and_then(|parent| fs::canonicalize(parent).ok())
            .zip(pattern.file_name())
            .is_some_and(|(parent, name)| {
                glob::Pattern::new(&parent.join(name).to_string_lossy())
                    .is_ok_and(|p| p.matches_path(path))
            })
    })
}

fn compressed(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| matches!(ext.to_str(), Some("gz" | "zst" | "xz" | "bz2" | "zip")))
}

fn open_regular(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(invalid("source is not a regular file"));
    }
    Ok(file)
}

fn fingerprint(file: &mut File, offset: u64, length: usize) -> io::Result<Fingerprint> {
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = [0u8; 1024];
    let mut read = 0;
    while read < length {
        let count = file.read(&mut bytes[read..length])?;
        if count == 0 {
            break;
        }
        read += count;
    }
    Ok(Fingerprint {
        length: read,
        hash: Sha256::digest(&bytes[..read]).into(),
    })
}

fn invalid(message: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}
