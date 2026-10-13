//! Материализация WAL и атомарные снимки открытых/закрытых сегментов.
use crate::{
    catalog::{SegmentMeta, SeriesName},
    columns,
    config::Config,
    definitions::{LabelGuard, RuleSet},
    extract,
    index::{ActiveIndex, IndexCache},
    journal::Journal,
    metrics::Metrics,
    model::{StoredEvent, now_nanos},
    series::{self, SeriesFile, SeriesFileMeta},
    wal::{Position, failpoint, sync_dir},
    wire,
};
use anyhow::{Context, Result, ensure};
use datafusion::arrow::array::RecordBatch;
use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock, atomic::Ordering},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use tantivy::Searcher;
use time::OffsetDateTime;
use tokio::sync::Semaphore;
use uuid::Uuid;

pub struct DiskSegment {
    pub meta: SegmentMeta,
    pub path: PathBuf,
}

#[derive(Clone)]
pub struct ActiveView {
    pub meta: SegmentMeta,
    pub batches: Vec<RecordBatch>,
    pub points: Vec<RecordBatch>,
    pub names: BTreeMap<(String, String, &'static str), (i64, i64)>,
    pub searcher: Searcher,
}

#[derive(Default)]
pub struct Snapshot {
    pub closed: Vec<Arc<DiskSegment>>,
    pub series_files: Vec<Arc<SeriesFile>>,
    pub active: Option<ActiveView>,
    pub watermark: u64,
    pub generation: u64,
    pub published_at: i64,
}

pub struct Core {
    pub journal: Arc<Journal>,
    snapshot: RwLock<Arc<Snapshot>>,
    pub rules: RwLock<Arc<RuleSet>>,
    pub indexes: IndexCache,
    pub instance: Uuid,
    pub(crate) cursor_key: [u8; 32],
    pub search_slots: Arc<Semaphore>,
    pub analytics_slots: Arc<Semaphore>,
    pub query_runtime: Arc<datafusion::execution::runtime_env::RuntimeEnv>,
    worker: Mutex<Option<JoinHandle<Result<()>>>>,
}

impl Core {
    /// Blocking startup: готовность наступает только после восстановления WAL.
    pub fn open(config: Config) -> Result<Arc<Self>> {
        let query_runtime = datafusion::execution::runtime_env::RuntimeEnvBuilder::new()
            .with_memory_pool(Arc::new(
                datafusion::execution::memory_pool::GreedyMemoryPool::new(
                    config.query_memory_bytes,
                ),
            ))
            .with_metadata_cache_limit(32 << 20)
            .with_file_statistics_cache_limit(16 << 20)
            .with_object_list_cache_limit(8 << 20)
            .build_arc()?;
        let journal = Journal::open(config)?;
        let rules = RuleSet::load(&journal.catalog.lock().unwrap().definitions()?)?;
        let mut key = [0; 32];
        key[..16].copy_from_slice(Uuid::new_v4().as_bytes());
        key[16..].copy_from_slice(Uuid::new_v4().as_bytes());
        let core = Arc::new(Self {
            indexes: IndexCache::new(
                journal.config.index_cache_entries,
                journal.config.index_memory_bytes,
            ),
            journal,
            snapshot: RwLock::new(Arc::new(Snapshot::default())),
            rules: RwLock::new(Arc::new(rules)),
            instance: Uuid::new_v4(),
            cursor_key: key,
            search_slots: Arc::new(Semaphore::new(4)),
            analytics_slots: Arc::new(Semaphore::new(2)),
            query_runtime,
            worker: Mutex::new(None),
        });
        let mut worker = Materializer::recover(core.clone())?;
        while worker.next()? {}
        worker.publish()?;
        let handle = thread::Builder::new().name("lognara-segments".into()).spawn(move || {
            let result = worker.run();
            if let Err(error) = &result {
                worker.core.journal.healthy.store(false, Ordering::Release);
                tracing::error!(%error, "segment worker stopped; acknowledged data remains in WAL");
            }
            result
        })?;
        *core.worker.lock().unwrap() = Some(handle);
        Ok(core)
    }

    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.snapshot.read().unwrap().clone()
    }

    pub fn shutdown(&self) -> Result<()> {
        self.journal.stopping.store(true, Ordering::Release);
        let handle = self.worker.lock().unwrap().take();
        if let Some(handle) = handle {
            handle.thread().unpark();
            handle
                .join()
                .map_err(|_| anyhow::anyhow!("segment worker panicked"))??;
        }
        Ok(())
    }
}

struct ActiveSegment {
    meta: SegmentMeta,
    path: PathBuf,
    batches: Vec<RecordBatch>,
    points: Vec<RecordBatch>,
    point_rows: usize,
    point_min: i64,
    point_max: i64,
    names: BTreeMap<(String, String, &'static str), (i64, i64)>,
    series_path: String,
    index: ActiveIndex,
    started: Instant,
}

impl ActiveSegment {
    /// Извлекает точки из строк пачки; ошибки разбора только считаются в метриках.
    fn add_points(
        &mut self,
        rows: &[StoredEvent],
        rules: &RuleSet,
        guard: &mut LabelGuard,
        metrics: &Metrics,
    ) -> Result<()> {
        let mut points = vec![];
        for row in rows {
            let (found, invalid) = extract::native(row);
            if invalid {
                metrics.series_parse_errors.fetch_add(1, Ordering::Relaxed);
            }
            points.extend(found);
            let (found, invalid) = extract::rules(row, rules);
            if invalid {
                metrics.series_parse_errors.fetch_add(1, Ordering::Relaxed);
            }
            points.extend(found);
        }
        let extracted = points.len();
        points.retain(|point| {
            point
                .rule_id
                .as_ref()
                .is_none_or(|rule_id| guard.admit(rule_id, &point.labels))
        });
        metrics
            .series_dropped_points
            .fetch_add((extracted - points.len()) as u64, Ordering::Relaxed);
        if points.is_empty() {
            return Ok(());
        }
        metrics
            .series_points
            .fetch_add(points.len() as u64, Ordering::Relaxed);
        for point in &points {
            self.names
                .entry((
                    point.source.service.clone(),
                    point.name.clone(),
                    point.origin.as_str(),
                ))
                .and_modify(|(first, last)| {
                    *first = (*first).min(point.timestamp);
                    *last = (*last).max(point.timestamp);
                })
                .or_insert((point.timestamp, point.timestamp));
            self.point_min = self.point_min.min(point.timestamp);
            self.point_max = self.point_max.max(point.timestamp);
        }
        self.point_rows += points.len();
        self.points.push(series::encode(&points)?);
        Ok(())
    }
}

struct Materializer {
    core: Arc<Core>,
    closed: Vec<Arc<DiskSegment>>,
    series_files: Vec<Arc<SeriesFile>>,
    active: Option<ActiveSegment>,
    retired: Vec<Arc<DiskSegment>>,
    position: Position,
    watermark: u64,
    generation: u64,
    guard: LabelGuard,
    last_publish: Instant,
    last_retention: Instant,
}

impl Materializer {
    fn recover(core: Arc<Core>) -> Result<Self> {
        let (position, _, _) = core.journal.catalog.lock().unwrap().progress()?;
        let mut closed = vec![];
        let root = core.journal.config.data_dir.join("segments");
        fs::create_dir_all(&root)?;
        let deleting = core.journal.catalog.lock().unwrap().segments("deleting")?;
        for meta in deleting {
            let path = core.journal.config.data_dir.join(&meta.path);
            if path.exists() {
                fs::remove_dir_all(&path)?;
                sync_dir(path.parent().unwrap())?;
            }
            core.journal
                .catalog
                .lock()
                .unwrap()
                .finish_deleting(&meta.id)?;
        }
        for meta in core.journal.catalog.lock().unwrap().segments("ready")? {
            let path = core.journal.config.data_dir.join(&meta.path);
            ensure!(
                path.join("logs.parquet").is_file() && path.join("meta.json").is_file(),
                "missing durable segment {}",
                meta.id
            );
            let manifest: SegmentMeta =
                serde_json::from_reader(File::open(path.join("meta.json"))?)?;
            ensure!(
                manifest.id == meta.id
                    && manifest.rows == meta.rows
                    && manifest.checkpoint == meta.checkpoint,
                "segment manifest disagrees with catalog"
            );
            closed.push(Arc::new(DiskSegment { meta, path }));
        }
        let retained: HashSet<_> = closed.iter().map(|segment| segment.path.clone()).collect();
        for path in nested_directories(&root, 4)? {
            if !retained.contains(&path) {
                fs::remove_dir_all(&path)?;
                sync_dir(path.parent().unwrap())?;
            }
        }
        let series_root = core.journal.config.data_dir.join("metrics");
        fs::create_dir_all(&series_root)?;
        let deleting_series = core
            .journal
            .catalog
            .lock()
            .unwrap()
            .series_files("deleting")?;
        for meta in deleting_series {
            let path = core.journal.config.data_dir.join(&meta.path);
            if path.exists() {
                fs::remove_file(&path)?;
                sync_dir(path.parent().unwrap())?;
            }
            core.journal
                .catalog
                .lock()
                .unwrap()
                .finish_deleting_series(&meta.id)?;
        }
        let mut series_files = vec![];
        for meta in core.journal.catalog.lock().unwrap().series_files("ready")? {
            let path = core.journal.config.data_dir.join(&meta.path);
            ensure!(path.is_file(), "missing durable series file {}", meta.id);
            series_files.push(Arc::new(SeriesFile { meta, path }));
        }
        let registered: HashSet<_> = series_files.iter().map(|file| file.path.clone()).collect();
        for day in nested_directories(&series_root, 3)? {
            let mut removed = false;
            for entry in fs::read_dir(&day)? {
                let path = entry?.path();
                let stray = path.is_file()
                    && path
                        .extension()
                        .is_some_and(|ext| ext == "parquet" || ext == "tmp");
                if stray && !registered.contains(&path) {
                    fs::remove_file(&path)?;
                    removed = true;
                }
            }
            if removed {
                sync_dir(&day)?;
            }
        }
        let wal = core.journal.wal.lock().unwrap();
        let watermark = wal.after(position).map_or(wal.next_ids().1 - 1, |receipt| {
            receipt.first_sequence
                + if receipt.batch_id == position.batch_id {
                    position.offset
                } else {
                    0
                }
                - 1
        });
        drop(wal);
        Ok(Self {
            core,
            closed,
            series_files,
            active: None,
            retired: vec![],
            position,
            watermark,
            generation: 0,
            guard: LabelGuard::default(),
            last_publish: Instant::now(),
            last_retention: Instant::now() - Duration::from_secs(60),
        })
    }

    fn run(&mut self) -> Result<()> {
        loop {
            let worked = self.next()?;
            if self.last_publish.elapsed() >= self.core.journal.config.refresh_interval {
                self.publish()?;
            }
            if self.active.as_ref().is_some_and(|segment| {
                segment.started.elapsed() >= self.core.journal.config.segment_age
            }) {
                self.seal()?;
            }
            if self.last_retention.elapsed() >= Duration::from_secs(1) {
                self.retain()?;
                self.last_retention = Instant::now();
            }
            if !worked {
                if self.core.journal.stopping.load(Ordering::Acquire) {
                    self.seal()?;
                    self.publish()?;
                    self.finish_deletions()?;
                    return Ok(());
                }
                thread::park_timeout(Duration::from_millis(20));
            }
        }
    }

    fn next(&mut self) -> Result<bool> {
        let next = {
            let wal = self.core.journal.wal.lock().unwrap();
            wal.after(self.position)
                .map(|receipt| wal.read(&receipt).map(|body| (receipt, body)))
                .transpose()?
        };
        let Some((receipt, body)) = next else {
            return Ok(false);
        };
        let batch = wire::decode_with_budget(
            &body,
            self.core.journal.config.max_decoded_bytes,
            self.core.journal.config.max_model_bytes,
        )?;
        ensure!(
            batch.event_count() as u64 == receipt.events,
            "WAL event count mismatch"
        );
        let skip = if receipt.batch_id == self.position.batch_id {
            self.position.offset as usize
        } else {
            0
        };
        self.position = Position {
            batch_id: receipt.batch_id,
            offset: skip as u64,
        };
        let mut chunk = Vec::new();
        let mut chunk_bytes = 0usize;
        for row in batch
            .into_events(receipt.first_sequence, receipt.received_at)
            .skip(skip)
        {
            chunk_bytes = chunk_bytes.saturating_add(estimated_bytes(&row));
            chunk.push(row);
            let active_rows = self.active.as_ref().map_or(0, |active| active.meta.rows);
            if chunk.len() >= 4096
                || chunk_bytes >= 4 << 20
                || active_rows + chunk.len() >= self.core.journal.config.segment_rows
            {
                self.append(&chunk)?;
                chunk.clear();
                chunk_bytes = 0;
            }
        }
        if !chunk.is_empty() {
            self.append(&chunk)?;
        }
        // Пачки только со счётчиками dropped тоже должны двигать checkpoint.
        if receipt.events == 0 && self.active.is_none() {
            self.checkpoint(None, None, &[])?;
        }
        Ok(true)
    }

    fn append(&mut self, rows: &[StoredEvent]) -> Result<()> {
        if self.active.is_none() {
            self.active = Some(self.new_segment(&rows[0])?);
        }
        // Один снимок правил на пачку: изменение в середине пачки не разделяет её строки.
        let rules = self.core.rules.read().unwrap().clone();
        let active = self.active.as_mut().unwrap();
        let batch = columns::encode(rows)?;
        for row in rows {
            active.index.add(row, active.meta.rows as u64)?;
            active.meta.rows += 1;
            active.meta.last_sequence = row.sequence;
            active.meta.min_timestamp = active.meta.min_timestamp.min(row.event.timestamp);
            active.meta.max_timestamp = active.meta.max_timestamp.max(row.event.timestamp);
            active.meta.max_received_at = active.meta.max_received_at.max(row.core_received_at);
        }
        active.meta.uncompressed_bytes += batch.get_array_memory_size();
        active.batches.push(batch);
        active.add_points(rows, &rules, &mut self.guard, &self.core.journal.metrics)?;
        self.position.offset += rows.len() as u64;
        self.watermark = rows.last().unwrap().sequence;
        active.meta.checkpoint = self.position;
        if active.meta.rows >= self.core.journal.config.segment_rows
            || active.meta.uncompressed_bytes >= self.core.journal.config.segment_bytes
        {
            self.seal()?;
        } else if self.last_publish.elapsed() >= self.core.journal.config.refresh_interval {
            self.publish()?;
        }
        Ok(())
    }

    fn new_segment(&self, row: &StoredEvent) -> Result<ActiveSegment> {
        let id = Uuid::now_v7().to_string();
        let date = OffsetDateTime::from_unix_timestamp_nanos(row.core_received_at as i128)?;
        let day = format!(
            "{:04}/{:02}/{:02}",
            date.year(),
            date.month() as u8,
            date.day()
        );
        let relative = format!("segments/{day}/{id}");
        let series_path = format!("metrics/{day}/{id}.parquet");
        let path = self.core.journal.config.data_dir.join(&relative);
        fs::create_dir_all(&path)?;
        let index = ActiveIndex::create(
            &path.join("index"),
            self.core.journal.config.index_memory_bytes,
        )?;
        Ok(ActiveSegment {
            meta: SegmentMeta {
                version: 1,
                id,
                path: relative,
                rows: 0,
                first_sequence: row.sequence,
                last_sequence: row.sequence,
                min_timestamp: row.event.timestamp,
                max_timestamp: row.event.timestamp,
                max_received_at: row.core_received_at,
                uncompressed_bytes: 0,
                closed_at: 0,
                checkpoint: self.position,
            },
            path,
            batches: vec![],
            points: vec![],
            point_rows: 0,
            point_min: i64::MAX,
            point_max: i64::MIN,
            names: BTreeMap::new(),
            series_path,
            index,
            started: Instant::now(),
        })
    }

    fn publish(&mut self) -> Result<()> {
        let active = if let Some(segment) = self.active.as_mut() {
            Some(ActiveView {
                meta: segment.meta.clone(),
                batches: segment.batches.clone(),
                points: segment.points.clone(),
                names: segment.names.clone(),
                searcher: segment.index.publish()?,
            })
        } else {
            None
        };
        *self.core.snapshot.write().unwrap() = Arc::new(Snapshot {
            closed: self.closed.clone(),
            series_files: self.series_files.clone(),
            active,
            watermark: self.watermark,
            generation: self.generation,
            published_at: now_nanos(),
        });
        self.core
            .journal
            .metrics
            .visible_sequence
            .store(self.watermark, Ordering::Relaxed);
        let oldest = self
            .core
            .journal
            .wal
            .lock()
            .unwrap()
            .after(self.position)
            .map(|receipt| receipt.received_at);
        self.core.journal.metrics.visibility_lag_ms.store(
            oldest.map_or(0, |time| {
                now_nanos().saturating_sub(time).max(0) as u64 / 1_000_000
            }),
            Ordering::Relaxed,
        );
        self.last_publish = Instant::now();
        Ok(())
    }

    fn seal(&mut self) -> Result<()> {
        if self.active.is_none() {
            return Ok(());
        }
        // Публикация перед длительным seal сохраняет доступность всех готовых строк.
        self.publish()?;
        let mut segment = self.active.take().unwrap();
        columns::write_parquet(&segment.path.join("logs.parquet"), &segment.batches)?;
        failpoint("segment_after_parquet");
        let sealed_series = self.seal_series(&segment)?;
        segment.index.finish()?;
        segment.meta.closed_at = now_nanos();
        segment.meta.checkpoint = self.position;
        let mut file = File::create(segment.path.join("meta.json.tmp"))?;
        file.write_all(&serde_json::to_vec_pretty(&segment.meta)?)?;
        file.sync_all()?;
        fs::rename(
            segment.path.join("meta.json.tmp"),
            segment.path.join("meta.json"),
        )?;
        sync_ancestors(
            &segment.path.join("index"),
            &self.core.journal.config.data_dir,
        )?;
        failpoint("segment_before_catalog");
        let names: Vec<SeriesName> = segment
            .names
            .iter()
            .map(
                |((service, name, origin), (first_seen, last_seen))| SeriesName {
                    service: service.clone(),
                    name: name.clone(),
                    origin: origin.to_string(),
                    first_seen: *first_seen,
                    last_seen: *last_seen,
                },
            )
            .collect();
        self.checkpoint(
            Some(&segment.meta),
            sealed_series.as_ref().map(|file| &file.meta),
            &names,
        )?;
        if let Some(file) = sealed_series {
            self.series_files.push(Arc::new(file));
        }
        self.closed.push(Arc::new(DiskSegment {
            meta: segment.meta,
            path: segment.path,
        }));
        self.publish()?;
        self.core
            .journal
            .metrics
            .sealed_segments
            .fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// Пишет Parquet серий сегмента; без точек файл не создаётся.
    fn seal_series(&self, segment: &ActiveSegment) -> Result<Option<SeriesFile>> {
        if segment.point_rows == 0 {
            return Ok(None);
        }
        let root = &self.core.journal.config.data_dir;
        let path = root.join(&segment.series_path);
        let directory = path.parent().context("series outside data directory")?;
        fs::create_dir_all(directory)?;
        columns::write_parquet_with(
            &path,
            series::schema(),
            &segment.points,
            "series_during_parquet",
        )?;
        sync_ancestors(directory, root)?;
        failpoint("series_after_parquet");
        Ok(Some(SeriesFile {
            meta: SeriesFileMeta {
                version: 1,
                id: segment.meta.id.clone(),
                path: segment.series_path.clone(),
                rows: segment.point_rows,
                min_timestamp: segment.point_min,
                max_timestamp: segment.point_max,
                closed_at: now_nanos(),
            },
            path,
        }))
    }

    fn checkpoint(
        &self,
        segment: Option<&SegmentMeta>,
        series: Option<&SeriesFileMeta>,
        names: &[SeriesName],
    ) -> Result<()> {
        // Lock order всегда WAL -> catalog, как при дедупликации входящего запроса.
        let mut wal = self.core.journal.wal.lock().unwrap();
        let receipts = wal.through(self.position);
        self.core.journal.catalog.lock().unwrap().publish(
            segment,
            series,
            names,
            self.position,
            &receipts,
            wal.next_ids(),
        )?;
        failpoint("segment_after_catalog");
        wal.prune(self.position)?;
        failpoint("wal_after_prune");
        self.core
            .journal
            .metrics
            .wal_bytes
            .store(wal.bytes(), Ordering::Relaxed);
        Ok(())
    }

    fn retain(&mut self) -> Result<()> {
        let now = now_nanos();
        let before = now.saturating_sub(self.core.journal.config.retention.as_nanos() as i64);
        let ids: Vec<_> = self
            .closed
            .iter()
            .filter(|segment| segment.meta.max_received_at < before)
            .map(|segment| segment.meta.id.clone())
            .collect();
        if !ids.is_empty() {
            self.core
                .journal
                .catalog
                .lock()
                .unwrap()
                .mark_deleting(&ids)?;
            failpoint("retention_after_mark");
            self.closed.retain(|segment| {
                if ids.contains(&segment.meta.id) {
                    self.retired.push(segment.clone());
                    false
                } else {
                    true
                }
            });
            self.generation += 1;
            self.publish()?;
        }
        self.finish_deletions()?;
        let first_retained = self
            .closed
            .iter()
            .chain(self.retired.iter())
            .map(|segment| segment.meta.first_sequence)
            .chain(
                self.active
                    .iter()
                    .map(|segment| segment.meta.first_sequence),
            )
            .min()
            .unwrap_or(self.watermark.saturating_add(1));
        let receipt_age = self
            .core
            .journal
            .config
            .retention
            .max(Duration::from_secs(7 * 86400));
        self.core.journal.catalog.lock().unwrap().prune_receipts(
            now.saturating_sub(receipt_age.as_nanos() as i64),
            first_retained,
        )?;
        Ok(())
    }

    fn finish_deletions(&mut self) -> Result<()> {
        let mut waiting = vec![];
        for segment in self.retired.drain(..) {
            if Arc::strong_count(&segment) != 1 {
                waiting.push(segment);
                continue;
            }
            self.core.indexes.invalidate(&segment.path);
            fs::remove_dir_all(&segment.path)?;
            sync_dir(segment.path.parent().unwrap())?;
            self.core
                .journal
                .catalog
                .lock()
                .unwrap()
                .finish_deleting(&segment.meta.id)?;
            self.core
                .journal
                .metrics
                .deleted_segments
                .fetch_add(1, Ordering::Relaxed);
        }
        self.retired = waiting;
        Ok(())
    }
}

fn estimated_bytes(row: &StoredEvent) -> usize {
    160 + row.event.message.len()
        + row.source.server.len()
        + row.source.backend.len()
        + row.source.service.len()
        + serde_json::to_string(&row.event.attributes).map_or(0, |json| json.len())
        + row.source.environment.as_ref().map_or(0, String::len)
        + row.source.service_instance.as_ref().map_or(0, String::len)
        + row.event.action.as_ref().map_or(0, String::len)
        + row.event.request_id.as_ref().map_or(0, String::len)
}

/// Синхронизирует каталог и его предков до корня данных, чтобы новые имена пережили сбой.
fn sync_ancestors(start: &Path, root: &Path) -> Result<()> {
    let mut parent = start.to_path_buf();
    loop {
        sync_dir(&parent)?;
        if parent == root {
            return Ok(());
        }
        parent = parent
            .parent()
            .context("path outside data directory")?
            .into();
    }
}

/// Каталоги ровно на указанной глубине под root (например, segments/YYYY/MM/DD/<id>).
fn nested_directories(root: &Path, depth: usize) -> Result<Vec<PathBuf>> {
    let mut level = vec![root.to_path_buf()];
    for _ in 0..depth {
        let mut next = vec![];
        for parent in level {
            for entry in fs::read_dir(parent)? {
                let entry = entry?;
                if entry.file_type()?.is_dir() {
                    next.push(entry.path());
                }
            }
        }
        level = next;
    }
    Ok(level)
}
