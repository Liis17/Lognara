//! Суточные агрегаты точек метрик в фоне: почасовые и 12-часовые Parquet. Сырые точки остаются источником истины.
use std::{
    collections::{BTreeSet, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock, atomic::Ordering},
    thread::{self, JoinHandle},
    time::Duration,
};

use anyhow::{Context, Result};
use datafusion::{
    arrow::{
        array::RecordBatch,
        datatypes::{DataType, Field, IntervalMonthDayNano, Schema, SchemaRef, TimeUnit},
    },
    common::ScalarValue,
    functions::datetime::expr_fn::date_bin,
    functions_aggregate::expr_fn::{count, last_value, max, min, sum},
    prelude::*,
};
use sha2::{Digest, Sha256};
use time::{Date, OffsetDateTime};
use uuid::Uuid;

use crate::{
    columns::ParquetWriter,
    config::Config,
    model::now_nanos,
    series::SeriesFile,
    series_query,
    storage::{Core, Snapshot, nested_directories},
    wal::{failpoint, sync_dir},
};

pub(crate) const DAY_NANOS: i128 = 86_400 * 1_000_000_000;
const HOURLY: u32 = 3600;
const HALF_DAY: u32 = 43_200;
/// Без работы поток спит до пробуждения при изменении серий, но не дольше этого срока.
const IDLE: Duration = Duration::from_secs(60);
const BACKOFF: Duration = Duration::from_secs(60);
/// Пока заменённые файлы держит читатель, поток проверяет их чаще.
const RETIRED_POLL: Duration = Duration::from_secs(1);

/// Схема агрегатов: одна строка на корзину, серию и правило.
pub fn schema() -> SchemaRef {
    static SCHEMA: OnceLock<SchemaRef> = OnceLock::new();
    SCHEMA
        .get_or_init(|| {
            let time = || DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into()));
            Arc::new(Schema::new(vec![
                Field::new("bucket", time(), false),
                Field::new("service", DataType::Utf8, false),
                Field::new("environment", DataType::Utf8, true),
                Field::new("server", DataType::Utf8, false),
                Field::new("backend", DataType::Utf8, false),
                Field::new("service_instance", DataType::Utf8, true),
                Field::new("name", DataType::Utf8, false),
                Field::new("origin", DataType::Utf8, false),
                Field::new("rule_id", DataType::Utf8, true),
                Field::new("labels", DataType::Utf8, false),
                Field::new("sum", DataType::Float64, false),
                Field::new("count", DataType::Int64, false),
                Field::new("min", DataType::Float64, false),
                Field::new("max", DataType::Float64, false),
                Field::new("last", DataType::Float64, false),
                Field::new("last_ts", time(), false),
            ]))
        })
        .clone()
}

/// Строка каталога series_rollups.
#[derive(Debug, Clone)]
pub struct RollupMeta {
    pub id: String,
    /// Сутки UTC в виде YYYY-MM-DD.
    pub day: String,
    /// Корзина в секундах: 3600 или 43200.
    pub resolution: u32,
    /// Путь относительно data_dir.
    pub path: String,
    pub fingerprint: String,
    pub built_at: i64,
}

pub struct RollupFile {
    pub meta: RollupMeta,
    pub path: PathBuf,
}

/// Корзина суток по времени их окончания: None - без агрегата, запросы читают сырые точки.
fn resolution(day_end: i128, now: i128, config: &Config) -> Option<u32> {
    let raw = now - config.series_raw_days as i128 * DAY_NANOS;
    let hourly = now - config.series_hourly_days as i128 * DAY_NANOS;
    if day_end > raw {
        None
    } else if day_end > hourly {
        Some(HOURLY)
    } else {
        Some(HALF_DAY)
    }
}

/// Начало суток UTC в наносекундах без зажатия к i64.
fn day_start(day: Date) -> i128 {
    day.midnight().assume_utc().unix_timestamp_nanos()
}

/// Окно суток для чтения файлов серий; границы зажаты в диапазон i64.
fn window(day: Date) -> (i64, i64) {
    let clamp = |nanos: i128| nanos.clamp(i64::MIN as i128, i64::MAX as i128) as i64;
    (clamp(day_start(day)), clamp(day_start(day) + DAY_NANOS))
}

/// Ключ суток YYYY-MM-DD.
fn key(day: Date) -> String {
    format!(
        "{:04}-{:02}-{:02}",
        day.year(),
        day.month() as u8,
        day.day()
    )
}

/// Каталог разрешения: rollups/1h или rollups/12h.
fn directory(resolution: u32) -> &'static str {
    if resolution == HOURLY { "1h" } else { "12h" }
}

/// Сутки UTC, которые пересекает хотя бы один файл серий, по его диапазону времени точек.
fn candidate_days(files: &[Arc<SeriesFile>]) -> Result<BTreeSet<Date>> {
    let mut days = BTreeSet::new();
    for file in files {
        let last =
            OffsetDateTime::from_unix_timestamp_nanos(file.meta.max_timestamp as i128)?.date();
        let mut day =
            OffsetDateTime::from_unix_timestamp_nanos(file.meta.min_timestamp as i128)?.date();
        while day <= last {
            days.insert(day);
            let Some(next) = day.next_day() else { break };
            day = next;
        }
    }
    Ok(days)
}

/// Отпечаток суток: SHA-256 отсортированного списка "id:rows" файлов, пересекающих сутки.
fn fingerprint(files: &[Arc<SeriesFile>], (from, to): (i64, i64)) -> String {
    let mut items: Vec<String> = files
        .iter()
        .filter(|file| file.meta.overlaps(from, to))
        .map(|file| format!("{}:{}", file.meta.id, file.meta.rows))
        .collect();
    items.sort();
    let mut hasher = Sha256::new();
    for item in items {
        hasher.update(item.as_bytes());
        hasher.update(b"\n");
    }
    hex::encode(hasher.finalize())
}

/// Агрегат суток для чтения вместо точек: шаг кратен корзине, отпечаток равен текущим файлам,
/// и в открытом сегменте нет точек суток.
pub(crate) fn usable_rollup(
    rollups: &[Arc<RollupFile>],
    snapshot: &Snapshot,
    day_start: i128,
    step_seconds: u64,
) -> Option<Arc<RollupFile>> {
    let day = OffsetDateTime::from_unix_timestamp_nanos(day_start)
        .ok()?
        .date();
    let key = key(day);
    let candidates: Vec<&Arc<RollupFile>> = rollups
        .iter()
        .filter(|file| {
            file.meta.day == key && step_seconds.is_multiple_of(u64::from(file.meta.resolution))
        })
        .collect();
    if candidates.is_empty() || open_points(snapshot, day_start) {
        return None;
    }
    let digest = fingerprint(&snapshot.series_files, window(day));
    candidates
        .into_iter()
        .find(|file| file.meta.fingerprint == digest)
        .cloned()
}

/// Есть ли в открытом сегменте точки суток: агрегат их не содержит, поэтому сутки читаются сырыми.
fn open_points(snapshot: &Snapshot, day_start: i128) -> bool {
    snapshot.active.as_ref().is_some_and(|active| {
        active.names.values().any(|&(first, last)| {
            i128::from(first) < day_start + DAY_NANOS && i128::from(last) >= day_start
        })
    })
}

/// Агрегаты суток по файлам серий, пересекающим сутки. Открытый сегмент не входит:
/// его точки попадут в отпечаток после запечатывания и пересоберут сутки.
async fn build(
    core: &Core,
    snapshot: &Snapshot,
    day: Date,
    resolution: u32,
) -> Result<Vec<RecordBatch>> {
    let (from, to) = window(day);
    let view = Snapshot {
        series_files: snapshot.series_files.clone(),
        ..Snapshot::default()
    };
    let bucket = date_bin(
        lit(ScalarValue::IntervalMonthDayNano(Some(
            IntervalMonthDayNano::new(0, 0, resolution as i64 * 1_000_000_000),
        ))),
        col("timestamp"),
        series_query::timestamp(0),
    );
    let batches = series_query::dedup(
        series_query::frame(core, &view, from, to).await?.filter(
            col("timestamp")
                .gt_eq(series_query::timestamp(from))
                .and(col("timestamp").lt(series_query::timestamp(to))),
        )?,
    )?
    .aggregate(
        vec![
            bucket.alias("bucket"),
            col("service"),
            col("environment"),
            col("server"),
            col("backend"),
            col("service_instance"),
            col("name"),
            col("origin"),
            col("rule_id"),
            col("labels"),
        ],
        vec![
            sum(col("value")).alias("sum"),
            count(lit(1u8)).alias("count"),
            min(col("value")).alias("min"),
            max(col("value")).alias("max"),
            last_value(
                col("value"),
                vec![
                    col("timestamp").sort(true, false),
                    col("value").sort(true, false),
                ],
            )
            .alias("last"),
            max(col("timestamp")).alias("last_ts"),
        ],
    )?
    .collect()
    .await?;
    batches
        .iter()
        .map(|batch| Ok(RecordBatch::try_new(schema(), batch.columns().to_vec())?))
        .collect()
}

/// Удаляет файл, если он есть, и синхронизирует его каталог.
fn remove_if_exists(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_file(path)?;
        sync_dir(path.parent().context("rollup outside data directory")?)?;
    }
    Ok(())
}

/// Запускает поток агрегатов; остановка выполняется в Core::shutdown.
pub(crate) fn spawn(core: Arc<Core>) -> std::io::Result<JoinHandle<()>> {
    thread::Builder::new()
        .name("lognara-rollups".into())
        .spawn(move || {
            match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => Worker {
                    core,
                    runtime,
                    recovered: false,
                    files: vec![],
                    retired: vec![],
                    forced: BTreeSet::new(),
                    nightly: None,
                }
                .run(),
                Err(error) => {
                    tracing::error!(%error, "rollup runtime unavailable; rollups are not built")
                }
            }
        })
}

enum Step {
    Built,
    Idle,
}

struct Worker {
    core: Arc<Core>,
    runtime: tokio::runtime::Runtime,
    recovered: bool,
    /// Готовые агрегаты; тот же состав опубликован в Core::rollups.
    files: Vec<Arc<RollupFile>>,
    /// Заменённые агрегаты; файл удаляется, когда его не держит ни один список.
    retired: Vec<Arc<RollupFile>>,
    /// Сутки на принудительную перестройку; заполняет ночная проверка.
    forced: BTreeSet<String>,
    /// Последняя дата UTC ночной проверки; только в памяти.
    nightly: Option<Date>,
}

impl Worker {
    fn run(mut self) {
        loop {
            if self.core.journal.stopping.load(Ordering::Acquire) {
                return;
            }
            let pause = match self.step() {
                Ok(Step::Built) => self.core.journal.config.series_rollup_pause,
                Ok(Step::Idle) if self.retired.is_empty() => IDLE,
                Ok(Step::Idle) => RETIRED_POLL,
                Err(error) => {
                    self.core
                        .journal
                        .metrics
                        .series_rollup_errors
                        .fetch_add(1, Ordering::Relaxed);
                    tracing::warn!(%error, "rollup step failed; retrying after back-off");
                    BACKOFF
                }
            };
            thread::park_timeout(pause);
        }
    }

    /// Восстановление при первом шаге, очистка заменённых файлов, затем не больше одних суток.
    fn step(&mut self) -> Result<Step> {
        if !self.recovered {
            self.recover()?;
            self.recovered = true;
        }
        self.finish_deletions()?;
        self.schedule_nightly();
        let snapshot = self.core.snapshot();
        let now = now_nanos() as i128;
        let days = candidate_days(&snapshot.series_files)?;
        // Сутки без файлов точек остаются только после срока хранения метрик: агрегат снимается.
        let keys: HashSet<String> = days.iter().map(|day| key(*day)).collect();
        let orphan = self
            .files
            .iter()
            .find(|file| !keys.contains(&file.meta.day))
            .map(|file| file.meta.day.clone());
        if let Some(orphan) = orphan {
            self.replace(&orphan, None)?;
            return Ok(Step::Built);
        }
        for day in days {
            let key = key(day);
            let force = self.forced.contains(&key);
            let digest = fingerprint(&snapshot.series_files, window(day));
            let target = resolution(day_start(day) + DAY_NANOS, now, &self.core.journal.config);
            let current: Vec<&RollupMeta> = self
                .files
                .iter()
                .filter(|file| file.meta.day == key)
                .map(|file| &file.meta)
                .collect();
            let needed = match target {
                Some(wanted) => {
                    force
                        || current.iter().any(|meta| meta.resolution != wanted)
                        || !current
                            .iter()
                            .any(|meta| meta.resolution == wanted && meta.fingerprint == digest)
                }
                None => !current.is_empty(),
            };
            if needed {
                self.apply(&snapshot, day, target, digest)?;
                self.forced.remove(&key);
                return Ok(Step::Built);
            }
        }
        Ok(Step::Idle)
    }

    /// Строит агрегат суток (при target None только снимает старые) и публикует замену.
    fn apply(
        &mut self,
        snapshot: &Snapshot,
        day: Date,
        target: Option<u32>,
        digest: String,
    ) -> Result<()> {
        let new = match target {
            Some(resolution) => {
                let batches = self
                    .runtime
                    .block_on(build(&self.core, snapshot, day, resolution))?;
                Some(self.write(day, resolution, digest, &batches)?)
            }
            None => None,
        };
        self.replace(&key(day), new)
    }

    /// Публикует замену агрегатов суток `key`: старые становятся deleting, новый - ready.
    fn replace(&mut self, key: &str, new: Option<RollupFile>) -> Result<()> {
        let ids: Vec<String> = self
            .files
            .iter()
            .filter(|file| file.meta.day == key)
            .map(|file| file.meta.id.clone())
            .collect();
        self.core
            .journal
            .catalog
            .lock()
            .unwrap()
            .replace_rollups(new.as_ref().map(|file| &file.meta), &ids)?;
        failpoint("rollup_after_mark");
        let (old, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.files)
            .into_iter()
            .partition(|file| file.meta.day == key);
        self.files = kept;
        self.retired.extend(old);
        if let Some(file) = new {
            self.core
                .journal
                .metrics
                .series_rollups_built
                .fetch_add(1, Ordering::Relaxed);
            self.files.push(Arc::new(file));
        }
        self.publish();
        Ok(())
    }

    /// Пишет Parquet через .tmp и переименование; файл без строки в каталоге уберёт следующий старт.
    fn write(
        &self,
        day: Date,
        resolution: u32,
        digest: String,
        batches: &[RecordBatch],
    ) -> Result<RollupFile> {
        let id = Uuid::now_v7().to_string();
        let path = format!(
            "rollups/{}/{}/{id}.parquet",
            directory(resolution),
            key(day).replace('-', "/")
        );
        let absolute = self.core.journal.config.data_dir.join(&path);
        fs::create_dir_all(absolute.parent().context("rollup outside data directory")?)?;
        let mut writer = ParquetWriter::create(&absolute, schema())?;
        for batch in batches {
            writer.write(batch)?;
        }
        writer.finish()?;
        failpoint("rollup_after_parquet");
        Ok(RollupFile {
            meta: RollupMeta {
                id,
                day: key(day),
                resolution,
                path,
                fingerprint: digest,
                built_at: now_nanos(),
            },
            path: absolute,
        })
    }

    /// Публикует готовые агрегаты для читателей; порядок - по дню и разрешению.
    fn publish(&mut self) {
        self.files.sort_by(|a, b| {
            (&a.meta.day, a.meta.resolution).cmp(&(&b.meta.day, b.meta.resolution))
        });
        *self.core.rollups.write().unwrap() = Arc::new(self.files.clone());
    }

    /// Ночью по UTC в заданный час ставит в очередь перестройку всех суток с агрегатами.
    fn schedule_nightly(&mut self) {
        let now = OffsetDateTime::now_utc();
        if now.hour() != self.core.journal.config.series_rollup_hour_utc
            || self.nightly == Some(now.date())
        {
            return;
        }
        self.nightly = Some(now.date());
        self.forced = self
            .files
            .iter()
            .map(|file| file.meta.day.clone())
            .collect();
    }

    /// Восстановление реестра: удаляет отложенные, снимает строки без файла и незарегистрированные файлы.
    fn recover(&mut self) -> Result<()> {
        let data_dir = self.core.journal.config.data_dir.clone();
        let root = data_dir.join("rollups");
        fs::create_dir_all(&root)?;
        let mut files = vec![];
        {
            let mut catalog = self.core.journal.catalog.lock().unwrap();
            for meta in catalog.rollups("deleting")? {
                remove_if_exists(&data_dir.join(&meta.path))?;
                catalog.finish_deleting_rollup(&meta.id)?;
            }
            for meta in catalog.rollups("ready")? {
                let path = data_dir.join(&meta.path);
                if path.is_file() {
                    files.push(Arc::new(RollupFile { meta, path }));
                } else {
                    tracing::warn!(id = %meta.id, "rollup file missing; it will be rebuilt");
                    catalog.replace_rollups(None, std::slice::from_ref(&meta.id))?;
                    catalog.finish_deleting_rollup(&meta.id)?;
                }
            }
        }
        let registered: HashSet<PathBuf> = files.iter().map(|file| file.path.clone()).collect();
        for day in nested_directories(&root, 4)? {
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
        self.files = files;
        self.publish();
        Ok(())
    }

    /// Удаляет заменённые агрегаты без читателей; остальные ждут следующего прохода.
    fn finish_deletions(&mut self) -> Result<()> {
        let mut waiting = vec![];
        for file in self.retired.drain(..) {
            if Arc::strong_count(&file) != 1 {
                waiting.push(file);
                continue;
            }
            remove_if_exists(&file.path)?;
            self.core
                .journal
                .catalog
                .lock()
                .unwrap()
                .finish_deleting_rollup(&file.meta.id)?;
        }
        self.retired = waiting;
        Ok(())
    }
}
