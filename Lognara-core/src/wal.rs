//! Последовательный WAL. Одна атомарно опубликованная запись на файл.
//! *.tmp — незавершённый хвост; *.wal всегда целиком проверяется при восстановлении.

use std::{
    collections::{BTreeMap, HashMap},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MAGIC: &[u8; 8] = b"LGWAL001";
const HEADER: usize = 80;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Position {
    pub batch_id: u64,
    /// Количество уже материализованных событий этой пачки.
    pub offset: u64,
}

#[derive(Debug, Clone)]
pub struct Receipt {
    pub batch_id: u64,
    pub first_sequence: u64,
    pub events: u64,
    pub received_at: i64,
    pub hash: [u8; 32],
    pub body_bytes: u64,
}

impl Receipt {
    pub fn end(&self) -> Position {
        Position {
            batch_id: self.batch_id,
            offset: self.events,
        }
    }

    pub fn last_sequence(&self) -> u64 {
        self.first_sequence + self.events.saturating_sub(1)
    }
}

pub struct Wal {
    dir: PathBuf,
    entries: BTreeMap<u64, Receipt>,
    hashes: HashMap<[u8; 32], u64>,
    next_batch: u64,
    next_sequence: u64,
    bytes: u64,
}

impl Wal {
    pub fn open(dir: &Path, next_batch: u64, next_sequence: u64) -> Result<Self> {
        fs::create_dir_all(dir)?;
        sync_dir(dir)?;
        if let Some(parent) = dir.parent() {
            sync_dir(parent)?;
        }
        let mut wal = Self {
            dir: dir.into(),
            entries: BTreeMap::new(),
            hashes: HashMap::new(),
            next_batch,
            next_sequence,
            bytes: 0,
        };
        let mut paths = fs::read_dir(dir)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<std::io::Result<Vec<_>>>()?;
        paths.sort();
        let mut previous: Option<Receipt> = None;
        for path in paths {
            if path.extension().is_some_and(|ext| ext == "tmp") {
                fs::remove_file(path)?;
                continue;
            }
            ensure!(
                path.extension().is_some_and(|ext| ext == "wal"),
                "unexpected file in WAL: {}",
                path.display()
            );
            let entry = read_header(&path)?;
            ensure!(
                path.file_stem().and_then(|v| v.to_str())
                    == Some(&format!("{:020}", entry.batch_id)),
                "WAL filename does not match header"
            );
            if let Some(last) = previous {
                ensure!(
                    entry.batch_id == last.batch_id + 1
                        && entry.first_sequence == last.first_sequence + last.events,
                    "non-contiguous WAL sequence"
                );
            }
            previous = Some(entry.clone());
            wal.next_batch = wal.next_batch.max(entry.batch_id + 1);
            wal.next_sequence = wal.next_sequence.max(entry.first_sequence + entry.events);
            wal.bytes += entry.body_bytes + HEADER as u64 + 4;
            ensure!(
                wal.hashes.insert(entry.hash, entry.batch_id).is_none(),
                "duplicate WAL batch"
            );
            wal.entries.insert(entry.batch_id, entry);
        }
        sync_dir(dir)?;
        Ok(wal)
    }

    /// Успех только после синхронизации файла, rename и родительского каталога.
    /// После любой ошибки caller останавливает запись до повторного восстановления.
    pub fn append(&mut self, body: &[u8], events: u64, received_at: i64) -> Result<Receipt> {
        ensure!(
            self.next_batch < i64::MAX as u64,
            "WAL batch sequence exhausted"
        );
        ensure!(
            events < i64::MAX as u64 - self.next_sequence,
            "event sequence exhausted"
        );
        let hash = Sha256::digest(body).into();
        ensure!(
            !self.hashes.contains_key(&hash),
            "caller must deduplicate before append"
        );
        let entry = Receipt {
            batch_id: self.next_batch,
            first_sequence: self.next_sequence,
            events,
            received_at,
            hash,
            body_bytes: body.len() as u64,
        };
        let mut header = Vec::with_capacity(HEADER);
        header.extend_from_slice(MAGIC);
        for number in [
            entry.batch_id,
            entry.first_sequence,
            events,
            received_at as u64,
            entry.body_bytes,
        ] {
            header.extend_from_slice(&number.to_le_bytes());
        }
        header.extend_from_slice(&hash);
        let mut checksum = crc32fast::Hasher::new();
        checksum.update(&header);
        checksum.update(body);
        let path = self.path(entry.batch_id);
        let temp = path.with_extension("tmp");
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(&header)?;
        file.write_all(body)?;
        file.write_all(&checksum.finalize().to_le_bytes())?;
        file.sync_all()?;
        failpoint("wal_before_rename");
        fs::rename(&temp, &path)?;
        sync_dir(&self.dir)?;
        failpoint("wal_after_sync");
        self.next_batch += 1;
        self.next_sequence += events;
        self.bytes += entry.body_bytes + HEADER as u64 + 4;
        self.hashes.insert(hash, entry.batch_id);
        self.entries.insert(entry.batch_id, entry.clone());
        Ok(entry)
    }

    pub fn find(&self, hash: &[u8; 32]) -> Option<&Receipt> {
        self.hashes.get(hash).and_then(|id| self.entries.get(id))
    }

    pub fn after(&self, position: Position) -> Option<Receipt> {
        self.entries
            .range(position.batch_id..)
            .find(|(_, entry)| entry.end() > position)
            .map(|(_, entry)| entry.clone())
    }

    pub fn through(&self, position: Position) -> Vec<Receipt> {
        self.entries
            .range(..=position.batch_id)
            .map(|(_, entry)| entry.clone())
            .collect()
    }

    pub fn read(&self, entry: &Receipt) -> Result<Vec<u8>> {
        // Дескриптор получен из проверенного WAL. Повторная проверка ловит порчу после старта.
        let path = self.path(entry.batch_id);
        read_header(&path)?;
        let bytes = fs::read(path)?;
        Ok(bytes[HEADER..bytes.len() - 4].to_vec())
    }

    /// Вызывать только после долговечной транзакции каталога с checkpoint и receipts.
    pub fn prune(&mut self, checkpoint: Position) -> Result<()> {
        let ids: Vec<_> = self
            .entries
            .iter()
            .filter(|(_, entry)| entry.end() <= checkpoint)
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            fs::remove_file(self.path(id))?;
            let entry = self.entries.remove(&id).expect("entry selected above");
            self.hashes.remove(&entry.hash);
            self.bytes -= entry.body_bytes + HEADER as u64 + 4;
        }
        sync_dir(&self.dir)?;
        Ok(())
    }

    pub fn bytes(&self) -> u64 {
        self.bytes
    }
    pub fn next_ids(&self) -> (u64, u64) {
        (self.next_batch, self.next_sequence)
    }
    fn path(&self, id: u64) -> PathBuf {
        self.dir.join(format!("{id:020}.wal"))
    }
}

fn read_header(path: &Path) -> Result<Receipt> {
    let mut file = File::open(path)?;
    let mut header = [0u8; HEADER];
    file.read_exact(&mut header)
        .with_context(|| format!("incomplete published WAL record {}", path.display()))?;
    ensure!(&header[..8] == MAGIC, "unsupported WAL format");
    let number = |start| u64::from_le_bytes(header[start..start + 8].try_into().unwrap());
    let length = number(40);
    ensure!(
        file.metadata()?.len() == HEADER as u64 + length + 4,
        "invalid WAL record length"
    );
    let mut checksum = crc32fast::Hasher::new();
    checksum.update(&header);
    let mut body_hash = Sha256::new();
    let mut left = length;
    let mut buffer = [0u8; 64 * 1024];
    while left > 0 {
        let size = left.min(buffer.len() as u64) as usize;
        file.read_exact(&mut buffer[..size])?;
        checksum.update(&buffer[..size]);
        body_hash.update(&buffer[..size]);
        left -= size as u64;
    }
    let mut expected = [0; 4];
    file.read_exact(&mut expected)?;
    ensure!(
        checksum.finalize() == u32::from_le_bytes(expected),
        "WAL checksum mismatch: {}",
        path.display()
    );
    let hash: [u8; 32] = header[48..80].try_into().unwrap();
    ensure!(
        body_hash.finalize().as_slice() == hash,
        "WAL body hash mismatch"
    );
    let entry = Receipt {
        batch_id: number(8),
        first_sequence: number(16),
        events: number(24),
        received_at: number(32) as i64,
        body_bytes: length,
        hash,
    };
    ensure!(
        entry.batch_id > 0
            && entry.batch_id < i64::MAX as u64
            && entry.first_sequence > 0
            && entry.first_sequence < i64::MAX as u64
            && entry.events < i64::MAX as u64 - entry.first_sequence,
        "invalid WAL sequence range"
    );
    Ok(entry)
}

pub fn sync_dir(path: &Path) -> std::io::Result<()> {
    File::open(path)?.sync_all()
}

/// Только opt-in сборки для subprocess crash-тестов; отсутствует в обычном binary.
pub(crate) fn failpoint(name: &str) {
    #[cfg(feature = "crash-tests")]
    if std::env::var("LOGNARA_CRASH_AT").as_deref() == Ok(name) {
        std::process::abort();
    }
    let _ = name;
}
