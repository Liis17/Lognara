//! Состояние core для страницы инфраструктуры: версия, публикация и место на диске.
use std::{fs, io, path::Path, sync::atomic::Ordering::Relaxed};

use serde::Serialize;

use crate::{
    model::{format_timestamp, now_nanos},
    storage::Core,
};

#[derive(Serialize)]
pub struct Status {
    pub version: &'static str,
    pub as_of: String,
    pub ready: bool,
    pub visible_sequence: u64,
    /// Lag публикации: от приёма пачки до видимости в поиске, а не задержка доставки.
    pub visibility_lag_ms: u64,
    pub wal_bytes: u64,
    pub wal_limit_bytes: u64,
    pub storage: Storage,
}

#[derive(Serialize)]
pub struct Storage {
    pub segments: usize,
    pub parquet_bytes: u64,
    pub index_bytes: u64,
    /// Самое раннее время события в закрытых и активном сегментах.
    pub oldest_event_at: Option<String>,
    pub retention_seconds: u64,
    pub disk_total_bytes: u64,
    pub disk_free_bytes: u64,
}

/// Снимок удерживает сегменты от удаления retention, пока считаются их размеры.
pub fn status(core: &Core) -> io::Result<Status> {
    let snapshot = core.snapshot();
    let config = &core.journal.config;
    let metrics = &core.journal.metrics;
    let (mut parquet_bytes, mut index_bytes) = (0, 0);
    for segment in &snapshot.closed {
        parquet_bytes += size(&segment.path.join("logs.parquet"))?;
        index_bytes += size(&segment.path.join("index"))?;
    }
    if let Some(active) = &snapshot.active {
        index_bytes += size(&config.data_dir.join(&active.meta.path).join("index"))?;
    }
    let oldest = snapshot
        .closed
        .iter()
        .map(|segment| &segment.meta)
        .chain(snapshot.active.as_ref().map(|active| &active.meta))
        .filter(|meta| meta.rows > 0)
        .map(|meta| meta.min_timestamp)
        .min();
    Ok(Status {
        version: env!("CARGO_PKG_VERSION"),
        as_of: format_timestamp(now_nanos()),
        ready: core.journal.ready(),
        visible_sequence: metrics.visible_sequence.load(Relaxed),
        visibility_lag_ms: metrics.visibility_lag_ms.load(Relaxed),
        wal_bytes: metrics.wal_bytes.load(Relaxed),
        wal_limit_bytes: config.wal_max_bytes,
        storage: Storage {
            segments: snapshot.closed.len(),
            parquet_bytes,
            index_bytes,
            oldest_event_at: oldest.map(format_timestamp),
            retention_seconds: config.retention.as_secs(),
            disk_total_bytes: fs2::total_space(&config.data_dir)?,
            disk_free_bytes: fs2::available_space(&config.data_dir)?,
        },
    })
}

/// Размер файла или каталога; индекс может перестраиваться, исчезнувшие файлы пропускаются.
fn size(path: &Path) -> io::Result<u64> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    if !metadata.is_dir() {
        return Ok(metadata.len());
    }
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    entries
        .into_iter()
        .try_fold(0, |sum, entry| Ok(sum + size(&entry?.path())?))
}
