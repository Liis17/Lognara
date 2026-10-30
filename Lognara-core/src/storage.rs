//! Материализация WAL и атомарные снимки открытых/закрытых сегментов.
use crate::{
    catalog::{SegmentMeta, SeriesEnvironment, SeriesName},
    columns,
    compaction::Replacement,
    config::Config,
    definitions::{LabelGuard, RuleSet},
    extract,
    index::{ActiveIndex, IndexCache},
    journal::Journal,
    metrics::Metrics,
    model::{StoredEvent, now_nanos},
    pattern_catalog::PatternCatalog,
    rollup::{self, RollupFile},
    series::{self, SeriesFile, SeriesFileMeta},
    wal::{Position, failpoint, sync_dir},
    wire,
};
use anyhow::{Context, Result, ensure};
use datafusion::arrow::array::RecordBatch;
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, RwLock, atomic::Ordering, mpsc::Receiver},
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
    sizes: OnceLock<(u64, u64)>,
}

impl DiskSegment {
    pub fn new(meta: SegmentMeta, path: PathBuf) -> Self {
        Self {
            meta,
            path,
            sizes: OnceLock::new(),
        }
    }

    /// Размеры Parquet и индекса; закрытый сегмент неизменен, поэтому считаются один раз.
    /// После редкой пересборки индекса из Parquet размер индекса может быть устаревшим.
    pub fn sizes(&self) -> std::io::Result<(u64, u64)> {
        if let Some(sizes) = self.sizes.get() {
            return Ok(*sizes);
        }
        let sizes = (
            crate::status::size(&self.path.join("logs.parquet"))?,
            crate::status::size(&self.path.join("index"))?,
        );
        // Индекс без файлов бывает только во время пересборки: такой размер не запоминается.
        if sizes.1 == 0 {
            return Ok(sizes);
        }
        Ok(*self.sizes.get_or_init(|| sizes))
    }
}

#[derive(Clone)]
pub struct ActiveView {
    pub meta: SegmentMeta,
    pub batches: Vec<RecordBatch>,
    pub points: Vec<RecordBatch>,
    pub scoped: crate::scoped_metrics::Metadata,
    pub names: BTreeMap<(String, String, &'static str), (i64, i64)>,
    pub environments: BTreeSet<SeriesEnvironment>,
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
    pub patterns: PatternCatalog,
    worker: Mutex<Option<JoinHandle<Result<()>>>>,
    pattern_worker: Mutex<Option<JoinHandle<()>>>,
    /// Готовые суточные агрегаты; публикует поток lognara-rollups.
    pub rollups: RwLock<Arc<Vec<Arc<RollupFile>>>>,
    rollup_worker: Mutex<Option<JoinHandle<()>>>,
    compaction_worker: Mutex<Option<JoinHandle<()>>>,
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
        crate::scoped_metrics::migrate(&journal)?;
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
            patterns: PatternCatalog::new(),
            worker: Mutex::new(None),
            pattern_worker: Mutex::new(None),
            rollups: RwLock::new(Arc::new(vec![])),
            rollup_worker: Mutex::new(None),
            compaction_worker: Mutex::new(None),
        });
        let (requests, compactions) = std::sync::mpsc::channel();
        let mut worker = Materializer::recover(core.clone(), compactions)?;
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
        *core.pattern_worker.lock().unwrap() = Some(PatternCatalog::start(core.clone())?);
        let rollups = rollup::spawn(core.clone())?;
        *core.rollup_worker.lock().unwrap() = Some(rollups);
        let compaction = crate::compaction::spawn(core.clone(), requests)?;
        *core.compaction_worker.lock().unwrap() = Some(compaction);
        Ok(core)
    }

    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.snapshot.read().unwrap().clone()
    }

    pub fn shutdown(&self) -> Result<()> {
        self.journal.stopping.store(true, Ordering::Release);
        if let Some(handle) = self.pattern_worker.lock().unwrap().take() {
            handle.thread().unpark();
            handle
                .join()
                .map_err(|_| anyhow::anyhow!("pattern worker panicked"))?;
        }
        let rollups = self.rollup_worker.lock().unwrap().take();
        if let Some(handle) = rollups {
            handle.thread().unpark();
            if handle.join().is_err() {
                tracing::error!("rollup worker panicked; rollups are derived and rebuilt on start");
            }
        }
        let compaction = self.compaction_worker.lock().unwrap().take();
        if let Some(handle) = compaction {
            handle.thread().unpark();
            if handle.join().is_err() {
                tracing::error!("compaction worker panicked; source segments remain registered");
            }
        }
        let handle = self.worker.lock().unwrap().take();
        if let Some(handle) = handle {
            handle.thread().unpark();
            handle
                .join()
                .map_err(|_| anyhow::anyhow!("segment worker panicked"))??;
        }
        Ok(())
    }

    /// Будит поток агрегатов после изменения файлов серий.
    fn wake_rollups(&self) {
        if let Some(handle) = self.rollup_worker.lock().unwrap().as_ref() {
            handle.thread().unpark();
        }
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
    scoped: crate::scoped_metrics::Metadata,
    names: BTreeMap<(String, String, &'static str), (i64, i64)>,
    environments: BTreeSet<SeriesEnvironment>,
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
            self.scoped.point(point);
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
            if let Some(environment) = &point.source.environment {
                self.environments.insert((
                    point.source.service.clone(),
                    point.name.clone(),
                    environment.clone(),
                ));
            }
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
    retired_series: Vec<Arc<SeriesFile>>,
    position: Position,
    watermark: u64,
    generation: u64,
    guard: LabelGuard,
    last_publish: Instant,
    last_retention: Instant,
    last_compaction: Instant,
    compactions: Receiver<crate::compaction::Request>,
}

impl Materializer {
    fn recover(core: Arc<Core>, compactions: Receiver<crate::compaction::Request>) -> Result<Self> {
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
            closed.push(Arc::new(DiskSegment::new(meta, path)));
        }
        // id слитого сегмента новее источников: порядок задаёт sequence, а не id.
        closed.sort_by_key(|segment| segment.meta.first_sequence);
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
        // Файлы лежат в каталогах дней, слитые по месяцам - прямо в каталоге месяца.
        let directories = nested_directories(&series_root, 2)?
            .into_iter()
            .chain(nested_directories(&series_root, 3)?);
        for day in directories {
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
            retired_series: vec![],
            position,
            watermark,
            generation: 0,
            guard: LabelGuard::default(),
            last_publish: Instant::now(),
            last_retention: Instant::now() - Duration::from_secs(60),
            last_compaction: Instant::now() - Duration::from_secs(60),
            compactions,
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
            let compact_every = self
                .core
                .journal
                .config
                .series_compact_quiet
                .min(Duration::from_secs(60));
            if self.last_compaction.elapsed() >= compact_every {
                self.compact()?;
                self.last_compaction = Instant::now();
            }
            while let Ok(request) = self.compactions.try_recv() {
                let applied = match request.replacement {
                    Replacement::Segments { merged, sources } => {
                        self.replace_segments(merged, &sources)
                    }
                    Replacement::Series { merged, sources } => {
                        self.replace_series(merged, &sources)
                    }
                };
                let _ = request.reply.send(matches!(applied, Ok(true)));
                applied?;
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
            self.checkpoint(None, None, &[], &[])?;
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
        active.scoped.rows(rows);
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
            scoped: Default::default(),
            names: BTreeMap::new(),
            environments: BTreeSet::new(),
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
                scoped: segment.scoped.clone(),
                names: segment.names.clone(),
                environments: segment.environments.clone(),
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
        write_manifest(&segment.path, &segment.meta)?;
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
        let environments: Vec<SeriesEnvironment> = segment.environments.iter().cloned().collect();
        segment
            .scoped
            .save(&self.core.journal.catalog.lock().unwrap())?;
        self.checkpoint(
            Some(&segment.meta),
            sealed_series.as_ref().map(|file| &file.meta),
            &names,
            &environments,
        )?;
        if let Some(file) = sealed_series {
            self.series_files.push(Arc::new(file));
        }
        self.closed
            .push(Arc::new(DiskSegment::new(segment.meta, segment.path)));
        self.publish()?;
        self.core
            .journal
            .metrics
            .sealed_segments
            .fetch_add(1, Ordering::Relaxed);
        self.core.wake_rollups();
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
        environments: &[SeriesEnvironment],
    ) -> Result<()> {
        // Lock order всегда WAL -> catalog, как при дедупликации входящего запроса.
        let mut wal = self.core.journal.wal.lock().unwrap();
        let receipts = wal.through(self.position);
        self.core.journal.catalog.lock().unwrap().publish(
            segment,
            series,
            names,
            environments,
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
        let mut ids: Vec<_> = self
            .closed
            .iter()
            .filter(|segment| segment.meta.max_received_at < before)
            .map(|segment| segment.meta.id.clone())
            .collect();
        if let Some(limit) = self.core.journal.config.retention_max_bytes {
            // closed упорядочен по sequence: сверх предела удаляются самые старые сегменты,
            // новейший закрытый сегмент остаётся. Ошибка подсчёта не останавливает приём.
            let mut kept = vec![];
            for segment in self.closed.iter().filter(|s| !ids.contains(&s.meta.id)) {
                let size = segment.sizes().map_or_else(
                    |error| {
                        tracing::warn!(%error, path = %segment.path.display(), "segment size is unavailable");
                        0
                    },
                    |(parquet, index)| parquet + index,
                );
                kept.push((&segment.meta.id, size));
            }
            let mut total: u64 = kept.iter().map(|(_, size)| size).sum();
            kept.pop();
            for (id, size) in kept {
                if total <= limit {
                    break;
                }
                ids.push(id.clone());
                total -= size;
            }
        }
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
        self.retain_series(now)?;
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

    /// Заменяет источники слитым сегментом; false - источник уже удалён, новый каталог снят.
    /// Видимые строки не меняются, поэтому поколение курсоров остаётся прежним.
    fn replace_segments(
        &mut self,
        merged: DiskSegment,
        sources: &[Arc<DiskSegment>],
    ) -> Result<bool> {
        let present = sources.iter().all(|source| {
            self.closed
                .iter()
                .any(|segment| Arc::ptr_eq(segment, source))
        });
        if !present {
            // Каталог не зарегистрирован: при ошибке удаления его дочистит восстановление.
            crate::compaction::discard(&merged.path);
            return Ok(false);
        }
        let old_ids: Vec<String> = sources
            .iter()
            .map(|source| source.meta.id.clone())
            .collect();
        self.core
            .journal
            .catalog
            .lock()
            .unwrap()
            .replace_segments(&merged.meta, &old_ids)?;
        failpoint("segment_compact_after_mark");
        self.closed
            .retain(|segment| !sources.iter().any(|source| Arc::ptr_eq(segment, source)));
        self.closed.push(Arc::new(merged));
        self.closed
            .sort_by_key(|segment| segment.meta.first_sequence);
        self.retired.extend(sources.iter().cloned());
        self.publish()?;
        self.core
            .journal
            .metrics
            .segment_compactions
            .fetch_add(1, Ordering::Relaxed);
        Ok(true)
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
        self.finish_series_deletions()
    }

    /// Сливает файлы серий одного дня, если все они старше порога тишины; за проход один день.
    /// Прошедшие месяцы крупнее и сливаются в потоке lognara-compaction.
    fn compact(&mut self) -> Result<()> {
        let quiet = self.core.journal.config.series_compact_quiet.as_nanos() as i64;
        let cutoff = now_nanos().saturating_sub(quiet);
        let Some((directory, sources)) = series_day_group(&self.series_files, cutoff)? else {
            return Ok(());
        };
        let merged = write_series(&self.core, &directory, &sources, &|| false)?
            .expect("day merge is not interrupted");
        self.replace_series(merged, &sources)?;
        Ok(())
    }

    /// Заменяет источники слитым файлом серий; false - источник уже заменён или удалён.
    fn replace_series(&mut self, merged: SeriesFile, sources: &[Arc<SeriesFile>]) -> Result<bool> {
        let present = sources.iter().all(|source| {
            self.series_files
                .iter()
                .any(|file| Arc::ptr_eq(file, source))
        });
        if !present {
            crate::compaction::discard(&merged.path);
            return Ok(false);
        }
        let old_ids: Vec<String> = sources.iter().map(|file| file.meta.id.clone()).collect();
        self.core
            .journal
            .catalog
            .lock()
            .unwrap()
            .replace_series(&merged.meta, &old_ids)?;
        failpoint("series_compact_after_mark");
        self.series_files
            .retain(|file| !sources.iter().any(|source| Arc::ptr_eq(file, source)));
        self.series_files.push(Arc::new(merged));
        self.series_files.sort_by(|a, b| a.path.cmp(&b.path));
        self.retired_series.extend(sources.iter().cloned());
        self.publish()?;
        self.core
            .journal
            .metrics
            .series_compactions
            .fetch_add(1, Ordering::Relaxed);
        self.core.wake_rollups();
        Ok(true)
    }

    /// Срок хранения метрик: файлы, все точки которых старше срока, исключаются из снимков
    /// и удаляются после освобождения; агрегаты их суток снимает поток lognara-rollups.
    fn retain_series(&mut self, now: i64) -> Result<()> {
        let Some(retention) = self.core.journal.config.series_retention else {
            return Ok(());
        };
        let before = now.saturating_sub(retention.as_nanos() as i64);
        let (expired, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.series_files)
            .into_iter()
            .partition(|file| file.meta.max_timestamp < before);
        self.series_files = kept;
        if expired.is_empty() {
            return Ok(());
        }
        let ids: Vec<String> = expired.iter().map(|file| file.meta.id.clone()).collect();
        self.core
            .journal
            .catalog
            .lock()
            .unwrap()
            .mark_series_deleting(&ids)?;
        self.retired_series.extend(expired);
        self.publish()?;
        self.core.wake_rollups();
        Ok(())
    }

    /// Удаляет исходные файлы серий без держателей снимка; остальные ждут в retired_series.
    fn finish_series_deletions(&mut self) -> Result<()> {
        let mut waiting = vec![];
        for file in self.retired_series.drain(..) {
            if Arc::strong_count(&file) != 1 {
                waiting.push(file);
                continue;
            }
            fs::remove_file(&file.path)?;
            sync_dir(file.path.parent().unwrap())?;
            self.core
                .journal
                .catalog
                .lock()
                .unwrap()
                .finish_deleting_series(&file.meta.id)?;
        }
        self.retired_series = waiting;
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

/// Долговечно публикует meta.json сегмента через `.tmp`, fsync и rename.
pub(crate) fn write_manifest(dir: &Path, meta: &SegmentMeta) -> Result<()> {
    let mut file = File::create(dir.join("meta.json.tmp"))?;
    file.write_all(&serde_json::to_vec_pretty(meta)?)?;
    file.sync_all()?;
    fs::rename(dir.join("meta.json.tmp"), dir.join("meta.json"))?;
    Ok(())
}

/// Синхронизирует каталог и его предков до корня данных, чтобы новые имена пережили сбой.
pub(crate) fn sync_ancestors(start: &Path, root: &Path) -> Result<()> {
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
pub(crate) fn nested_directories(root: &Path, depth: usize) -> Result<Vec<PathBuf>> {
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

type SeriesGroup = Option<(String, Vec<Arc<SeriesFile>>)>;

/// Файлы серий по каталогам дня (`metrics/YYYY/MM/DD`) и месяца (`metrics/YYYY/MM`).
/// Месяц берётся из пути, то есть по времени поступления, как и день.
#[allow(clippy::type_complexity)]
fn series_directories(
    files: &[Arc<SeriesFile>],
) -> Result<(
    BTreeMap<&str, Vec<&Arc<SeriesFile>>>,
    BTreeMap<&str, Vec<&Arc<SeriesFile>>>,
)> {
    let mut days: BTreeMap<&str, Vec<&Arc<SeriesFile>>> = BTreeMap::new();
    let mut months: BTreeMap<&str, Vec<&Arc<SeriesFile>>> = BTreeMap::new();
    for file in files {
        let (directory, _) = file
            .meta
            .path
            .rsplit_once('/')
            .context("series file outside metrics directory")?;
        // metrics/YYYY/MM - 15 байт; каталог дня длиннее, файл месяца лежит прямо в нём.
        let month = directory.get(..15).context("series file outside month")?;
        if directory.len() > month.len() {
            days.entry(directory).or_default().push(file);
        }
        months.entry(month).or_default().push(file);
    }
    Ok((days, months))
}

/// Ключи упорядочены по строке пути: первой идёт самая старая группа из двух и более файлов,
/// все файлы которой закрыты раньше `cutoff`.
fn oldest_ready(
    groups: BTreeMap<&str, Vec<&Arc<SeriesFile>>>,
    cutoff: i64,
    allowed: impl Fn(&str) -> bool,
) -> SeriesGroup {
    groups
        .into_iter()
        .find(|(directory, files)| {
            allowed(directory)
                && files.len() >= 2
                && files.iter().all(|file| file.meta.closed_at < cutoff)
        })
        .map(|(directory, files)| (directory.to_owned(), files.into_iter().cloned().collect()))
}

/// Самый старый день для слияния файлов серий.
fn series_day_group(files: &[Arc<SeriesFile>], cutoff: i64) -> Result<SeriesGroup> {
    Ok(oldest_ready(series_directories(files)?.0, cutoff, |_| true))
}

/// Самый старый прошедший месяц UTC для слияния в `metrics/YYYY/MM/<id>.parquet`.
pub(crate) fn series_month_group(
    files: &[Arc<SeriesFile>],
    cutoff: i64,
    now: i64,
) -> Result<SeriesGroup> {
    let date = OffsetDateTime::from_unix_timestamp_nanos(now as i128)?;
    let current = format!("metrics/{:04}/{:02}", date.year(), date.month() as u8);
    Ok(oldest_ready(
        series_directories(files)?.1,
        cutoff,
        |month| month < current.as_str(),
    ))
}

/// Пишет слияние файлов серий в `directory/<id>.parquet`; None - `stop` прервал запись,
/// временный файл удалён.
pub(crate) fn write_series(
    core: &Core,
    directory: &str,
    sources: &[Arc<SeriesFile>],
    stop: &dyn Fn() -> bool,
) -> Result<Option<SeriesFile>> {
    let id = Uuid::now_v7().to_string();
    let relative = format!("{directory}/{id}.parquet");
    let path = core.journal.config.data_dir.join(&relative);
    let written = (|| -> Result<bool> {
        let mut writer = columns::ParquetWriter::create(&path, series::schema())?;
        for source in sources {
            for batch in columns::stream_batches(&source.path)? {
                if stop() {
                    return Ok(false);
                }
                writer.write(&batch?)?;
                failpoint("series_compact_during_parquet");
            }
        }
        writer.finish()?;
        Ok(true)
    })();
    let temp = path.with_extension("parquet.tmp");
    match written {
        Ok(true) => {}
        Ok(false) => {
            fs::remove_file(temp)?;
            return Ok(None);
        }
        Err(error) => {
            let _ = fs::remove_file(temp);
            return Err(error);
        }
    }
    failpoint("series_compact_after_parquet");
    Ok(Some(SeriesFile {
        meta: SeriesFileMeta {
            version: 1,
            id,
            path: relative,
            rows: sources.iter().map(|file| file.meta.rows).sum(),
            min_timestamp: sources
                .iter()
                .fold(i64::MAX, |min, file| min.min(file.meta.min_timestamp)),
            max_timestamp: sources
                .iter()
                .fold(i64::MIN, |max, file| max.max(file.meta.max_timestamp)),
            closed_at: now_nanos(),
        },
        path,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, closed_at: i64) -> Arc<SeriesFile> {
        Arc::new(SeriesFile {
            meta: SeriesFileMeta {
                version: 1,
                id: path.into(),
                path: path.into(),
                rows: 1,
                min_timestamp: 0,
                max_timestamp: 0,
                closed_at,
            },
            path: path.into(),
        })
    }

    #[test]
    fn failed_series_merge_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = crate::config::Config::from_lookup(|key| match key {
            "LOGNARA_ADMIN_TOKEN" => Some("ingest".into()),
            "LOGNARA_QUERY_TOKEN" => Some("query".into()),
            _ => None,
        })
        .unwrap();
        config.data_dir = dir.path().into();
        config.disk_reserve_bytes = 1;
        let core = Core::open(config).unwrap();
        let directory = dir.path().join("metrics/2026/01");
        fs::create_dir_all(&directory).unwrap();
        let source = directory.join("broken.parquet");
        fs::write(&source, b"not parquet").unwrap();
        let mut broken = file("broken", 0);
        Arc::get_mut(&mut broken).unwrap().path = source;

        assert!(write_series(&core, "metrics/2026/01", &[broken], &|| false).is_err());

        let names: Vec<_> = fs::read_dir(&directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["broken.parquet"]);
        core.shutdown().unwrap();
    }

    fn ids(group: SeriesGroup) -> Option<(String, Vec<String>)> {
        group.map(|(directory, files)| {
            (
                directory,
                files.iter().map(|file| file.meta.id.clone()).collect(),
            )
        })
    }

    #[test]
    fn series_groups_merge_days_and_only_past_months() {
        // 2026-10-15T00:00:00Z
        let now = 1_792_022_400_000_000_000;
        let files = vec![
            file("metrics/2026/09/30/a.parquet", 1),
            file("metrics/2026/09/02/b.parquet", 1),
            file("metrics/2026/10/01/c.parquet", 1),
            file("metrics/2026/10/01/d.parquet", 1),
        ];
        assert_eq!(
            ids(series_day_group(&files, 10).unwrap()),
            Some((
                "metrics/2026/10/01".into(),
                vec![
                    "metrics/2026/10/01/c.parquet".into(),
                    "metrics/2026/10/01/d.parquet".into()
                ]
            ))
        );
        // Прошедший сентябрь собирается в файл месяца, текущий октябрь - нет.
        let month = ids(series_month_group(&files, 10, now).unwrap()).unwrap();
        assert_eq!(month.0, "metrics/2026/09");
        assert_eq!(month.1.len(), 2);
        assert!(series_month_group(&files[2..], 10, now).unwrap().is_none());
        // Поздний файл уже слитого месяца сливается с файлом месяца после тишины.
        let files = vec![
            file("metrics/2026/09/m.parquet", 1),
            file("metrics/2026/09/30/late.parquet", 20),
        ];
        assert!(series_day_group(&files, 30).unwrap().is_none());
        assert!(series_month_group(&files, 10, now).unwrap().is_none());
        assert_eq!(
            series_month_group(&files, 30, now)
                .unwrap()
                .unwrap()
                .1
                .len(),
            2
        );
    }
}
