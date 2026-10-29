//! Слияние закрытых сегментов логов одного окна времени в один сегмент.
//!
//! Поток `lognara-compaction` собирает новый сегмент из неизменных источников снимка,
//! не блокируя материализацию; материализатор атомарно заменяет источники в каталоге.
use crate::{
    catalog::SegmentMeta,
    columns, index,
    model::now_nanos,
    storage::{Core, DiskSegment, sync_ancestors, write_manifest},
    wal::{failpoint, sync_dir},
};
use anyhow::{Result, ensure};
use std::{
    fs,
    sync::{
        Arc,
        atomic::Ordering,
        mpsc::{self, RecvTimeoutError},
    },
    thread::{self, JoinHandle},
    time::Duration,
};
use time::OffsetDateTime;
use uuid::Uuid;

/// Верхняя граница строк слитого сегмента: защищает высокий поток от гигантских индексов.
pub const MAX_COMPACT_ROWS: usize = 20_000_000;
/// Уровень ZSTD слитого Parquet: запись медленнее, чтение почти без изменений.
const COMPACT_ZSTD_LEVEL: i32 = 9;

/// Запрос материализатору заменить источники готовым сегментом; ответ true - заменено.
pub(crate) struct Request {
    pub merged: DiskSegment,
    pub sources: Vec<Arc<DiskSegment>>,
    pub reply: mpsc::Sender<bool>,
}

/// Подряд идущие сегменты самого старого готового окна; None - сливать нечего.
///
/// Окно сегмента определяет `max_received_at`: он не убывает по sequence, поэтому
/// сегменты окна идут подряд. Окно готово, когда с его конца прошло `quiet` и активный
/// сегмент в него не попадает. `processed` - sequence, обработанный каталогом паттернов:
/// необработанные строки не сливаются, чтобы его проход не получил весь слитый сегмент.
pub fn select(
    closed: &[Arc<DiskSegment>],
    active: Option<&SegmentMeta>,
    window: i64,
    quiet: i64,
    now: i64,
    processed: u64,
    max_rows: usize,
) -> Option<Vec<Arc<DiskSegment>>> {
    let mut segments: Vec<&Arc<DiskSegment>> = closed.iter().collect();
    segments.sort_by_key(|segment| segment.meta.first_sequence);
    let key = |meta: &SegmentMeta| meta.max_received_at.div_euclid(window);
    let active_key = active.map(key);
    let mut start = 0;
    while start < segments.len() {
        let current = key(&segments[start].meta);
        let end = segments[start..]
            .iter()
            .position(|segment| key(&segment.meta) != current)
            .map_or(segments.len(), |offset| start + offset);
        let window_end = current.saturating_add(1).saturating_mul(window);
        if window_end.saturating_add(quiet) > now
            || active_key == Some(current)
            || segments[end - 1].meta.last_sequence > processed
        {
            return None;
        }
        let mut group: Vec<Arc<DiskSegment>> = vec![];
        let mut rows = 0usize;
        for segment in &segments[start..end] {
            if !group.is_empty() && rows.saturating_add(segment.meta.rows) > max_rows {
                if group.len() >= 2 {
                    return Some(group);
                }
                group.clear();
                rows = 0;
            }
            rows = rows.saturating_add(segment.meta.rows);
            group.push((*segment).clone());
        }
        if group.len() >= 2 {
            return Some(group);
        }
        start = end;
    }
    None
}

/// Пишет слитый сегмент в новый каталог; None - core останавливается, каталог удалён.
fn build(core: &Core, sources: &[Arc<DiskSegment>], window: i64) -> Result<Option<DiskSegment>> {
    let config = &core.journal.config;
    let first = &sources[0].meta;
    let last = &sources[sources.len() - 1].meta;
    let start = last
        .max_received_at
        .div_euclid(window)
        .saturating_mul(window);
    let date = OffsetDateTime::from_unix_timestamp_nanos(start as i128)?;
    let id = Uuid::now_v7().to_string();
    let relative = format!(
        "segments/{:04}/{:02}/{:02}/{id}",
        date.year(),
        date.month() as u8,
        date.day()
    );
    let path = config.data_dir.join(&relative);
    fs::create_dir_all(&path)?;
    let stopping = || core.journal.stopping.load(Ordering::Acquire);
    let built = (|| -> Result<bool> {
        let mut writer = columns::ParquetWriter::with_level(
            &path.join("logs.parquet"),
            columns::schema(),
            COMPACT_ZSTD_LEVEL,
        )?;
        for source in sources {
            let mut rows = 0;
            for batch in columns::stream_batches(&source.path.join("logs.parquet"))? {
                if stopping() {
                    return Ok(false);
                }
                let batch = batch?;
                rows += batch.num_rows();
                writer.write(&batch)?;
            }
            ensure!(rows == source.meta.rows, "segment row count mismatch");
        }
        writer.finish()?;
        index::build(
            &path.join("logs.parquet"),
            &path.join("index"),
            config.index_memory_bytes,
            &stopping,
        )
    })();
    if !matches!(built, Ok(true)) {
        fs::remove_dir_all(&path)?;
        sync_dir(path.parent().expect("segment has a day directory"))?;
        return built.map(|_| None);
    }
    let meta = SegmentMeta {
        version: 1,
        id,
        path: relative,
        rows: sources.iter().map(|source| source.meta.rows).sum(),
        first_sequence: first.first_sequence,
        last_sequence: last.last_sequence,
        min_timestamp: sources
            .iter()
            .map(|source| source.meta.min_timestamp)
            .min()
            .expect("nonempty group"),
        max_timestamp: sources
            .iter()
            .map(|source| source.meta.max_timestamp)
            .max()
            .expect("nonempty group"),
        max_received_at: last.max_received_at,
        uncompressed_bytes: sources
            .iter()
            .map(|source| source.meta.uncompressed_bytes)
            .sum(),
        closed_at: now_nanos(),
        checkpoint: last.checkpoint,
    };
    write_manifest(&path, &meta)?;
    sync_ancestors(&path.join("index"), &config.data_dir)?;
    failpoint("segment_compact_after_parquet");
    Ok(Some(DiskSegment::new(meta, path)))
}

pub(crate) fn spawn(
    core: Arc<Core>,
    requests: mpsc::Sender<Request>,
) -> std::io::Result<JoinHandle<()>> {
    thread::Builder::new()
        .name("lognara-compaction".into())
        .spawn(move || {
            let config = &core.journal.config;
            let window = config.segment_compact.as_nanos() as i64;
            let quiet = (config.segment_age * 2).as_nanos() as i64;
            // Без работы проверка не реже закрытия сегмента: готовность окна зависит от него.
            let idle = config.segment_age.min(Duration::from_secs(60));
            while !core.journal.stopping.load(Ordering::Acquire) {
                let pause = match step(&core, &requests, window, quiet) {
                    Ok(true) => Duration::from_secs(1),
                    Ok(false) => idle,
                    Err(error) => {
                        core.journal
                            .metrics
                            .segment_compaction_errors
                            .fetch_add(1, Ordering::Relaxed);
                        tracing::error!(error = %format_args!("{error:#}"), "segment compaction failed; retrying later");
                        Duration::from_secs(60)
                    }
                };
                thread::park_timeout(pause);
            }
        })
}

/// Одна группа за шаг; true - сегменты заменены или замена отклонена и работа может быть ещё.
fn step(core: &Core, requests: &mpsc::Sender<Request>, window: i64, quiet: i64) -> Result<bool> {
    let Some(processed) = core.patterns.compaction_limit() else {
        return Ok(false);
    };
    let group = {
        let snapshot = core.snapshot();
        select(
            &snapshot.closed,
            snapshot.active.as_ref().map(|active| &active.meta),
            window,
            quiet,
            now_nanos(),
            processed,
            MAX_COMPACT_ROWS,
        )
    };
    let Some(sources) = group else {
        return Ok(false);
    };
    let Some(merged) = build(core, &sources, window)? else {
        return Ok(false);
    };
    let (reply, replies) = mpsc::channel();
    let path = merged.path.clone();
    if requests
        .send(Request {
            merged,
            sources,
            reply,
        })
        .is_err()
    {
        // Материализатор остановлен: незарегистрированный каталог удалит восстановление.
        return Ok(false);
    }
    loop {
        match replies.recv_timeout(Duration::from_millis(100)) {
            Ok(applied) => {
                if !applied {
                    tracing::info!(path = %path.display(), "segment compaction discarded");
                }
                return Ok(true);
            }
            Err(RecvTimeoutError::Disconnected) => return Ok(false),
            Err(RecvTimeoutError::Timeout) => {
                if core.journal.stopping.load(Ordering::Acquire) {
                    return Ok(false);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wal::Position;
    use std::path::PathBuf;

    fn segment(first: u64, last: u64, rows: usize, received: i64) -> Arc<DiskSegment> {
        Arc::new(DiskSegment::new(
            meta(first, last, rows, received),
            PathBuf::new(),
        ))
    }

    fn meta(first: u64, last: u64, rows: usize, received: i64) -> SegmentMeta {
        SegmentMeta {
            version: 1,
            id: format!("{first}"),
            path: String::new(),
            rows,
            first_sequence: first,
            last_sequence: last,
            min_timestamp: 0,
            max_timestamp: 0,
            max_received_at: received,
            uncompressed_bytes: 0,
            closed_at: 0,
            checkpoint: Position {
                batch_id: 0,
                offset: 0,
            },
        }
    }

    fn firsts(group: Option<Vec<Arc<DiskSegment>>>) -> Vec<u64> {
        group
            .unwrap_or_default()
            .iter()
            .map(|segment| segment.meta.first_sequence)
            .collect()
    }

    #[test]
    fn selects_oldest_ready_window_and_skips_single_segments() {
        let closed = vec![
            segment(7, 8, 2, 25),
            segment(1, 2, 2, 5),
            segment(3, 4, 2, 15),
            segment(5, 6, 2, 18),
        ];
        // Окно [0,10) из одного сегмента пропускается, [10,20) готово.
        assert_eq!(firsts(select(&closed, None, 10, 2, 22, 100, 100)), [3, 5]);
        // Тишина после конца окна ещё не прошла.
        assert!(select(&closed, None, 10, 2, 21, 100, 100).is_none());
        // Активный сегмент лежит в окне.
        let active = meta(9, 9, 1, 19);
        assert!(select(&closed, Some(&active), 10, 2, 40, 100, 100).is_none());
        // Каталог паттернов не обработал окно.
        assert!(select(&closed, None, 10, 2, 40, 5, 100).is_none());
    }

    #[test]
    fn splits_window_by_row_limit_and_keeps_large_segments() {
        let closed = vec![
            segment(1, 10, 10, 1),
            segment(11, 12, 2, 2),
            segment(13, 14, 2, 3),
            segment(15, 16, 2, 4),
        ];
        assert_eq!(firsts(select(&closed, None, 10, 0, 10, 100, 5)), [11, 13]);
        assert_eq!(
            firsts(select(&closed, None, 10, 0, 10, 100, 100)),
            [1, 11, 13, 15]
        );
        let merged = vec![segment(1, 10, 10, 1), segment(11, 12, 2, 2)];
        assert!(select(&merged, None, 10, 0, 10, 100, 10).is_none());
    }
}
