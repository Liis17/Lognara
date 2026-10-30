//! Derived lifetime metadata. Raw events remain authoritative for every count.
use crate::{
    analytics::raw_frame,
    config::Config,
    issue_identity::{Exception, fingerprint, is_error},
    pattern_identity::{Part, identify},
    query::{self, QueryError},
    storage::{Core, Snapshot},
    wire::LogLevel,
};
use anyhow::{Context, Result, ensure};
use datafusion::{
    arrow::array::{Array, RecordBatch, StringArray, TimestampNanosecondArray, UInt8Array},
    prelude::*,
};
use futures::TryStreamExt;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::{Arc, RwLock, atomic::Ordering},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Known {
    pub id: String,
    pub environment: Option<String>,
    pub service: String,
    pub level: String,
    pub template: String,
    pub parts: Vec<Part>,
    pub exact: bool,
    pub first: i64,
    pub last: i64,
}

/// Lifetime-метаданные проблемы; template/parts — сообщение первого обработанного события.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct KnownIssue {
    pub id: String,
    pub environment: Option<String>,
    pub service: String,
    pub action: Option<String>,
    pub exception_type: Option<String>,
    pub frames: Vec<String>,
    pub pattern_id: String,
    pub template: String,
    pub parts: Vec<Part>,
    pub exact: bool,
    pub first: i64,
    pub last: i64,
}

pub(crate) trait Lifetime: Clone + Serialize {
    fn span(&mut self) -> (&mut i64, &mut i64);
    fn last(&self) -> i64;
}

impl Lifetime for Known {
    fn span(&mut self) -> (&mut i64, &mut i64) {
        (&mut self.first, &mut self.last)
    }

    fn last(&self) -> i64 {
        self.last
    }
}

impl Lifetime for KnownIssue {
    fn span(&mut self) -> (&mut i64, &mut i64) {
        (&mut self.first, &mut self.last)
    }

    fn last(&self) -> i64 {
        self.last
    }
}

#[derive(Clone, Default)]
pub(crate) struct State {
    pub ready: bool,
    pub failed: bool,
    pub checkpoint: u64,
    pub as_of: i64,
    pub history_from: Option<i64>,
    pub complete: bool,
    /// Copy-on-write: публикация копирует только узлы дерева, не метаданные.
    pub known: Arc<BTreeMap<String, Arc<Known>>>,
    pub issues: Arc<BTreeMap<String, Arc<KnownIssue>>>,
    pub bytes: usize,
}

pub struct PatternCatalog {
    state: RwLock<Arc<State>>,
}

impl PatternCatalog {
    pub(crate) fn new() -> Self {
        Self {
            state: RwLock::new(Arc::new(State::default())),
        }
    }

    pub(crate) fn view(&self) -> Result<Arc<State>, QueryError> {
        let state = self.state.read().unwrap().clone();
        if state.failed {
            return Err(QueryError::Storage(anyhow::anyhow!(
                "pattern catalog is unavailable"
            )));
        }
        if !state.ready {
            return Err(QueryError::NotReady);
        }
        Ok(state)
    }

    /// Обработанный sequence; None, пока первичный проход не завершён.
    pub fn checkpoint(&self) -> Option<u64> {
        self.view().ok().map(|state| state.checkpoint)
    }

    /// Граница слияния сегментов: обработанный sequence, без границы при отказе каталога,
    /// None до первичного прохода.
    pub(crate) fn compaction_limit(&self) -> Option<u64> {
        let state = self.state.read().unwrap();
        if state.failed {
            Some(u64::MAX)
        } else {
            state.ready.then_some(state.checkpoint)
        }
    }

    pub(crate) fn start(core: Arc<Core>) -> Result<JoinHandle<()>> {
        Ok(thread::Builder::new()
            .name("lognara-patterns".into())
            .spawn(move || {
                if let Err(error) = Self::run(&core) {
                    tracing::error!(error = %format_args!("{error:#}"), "pattern catalog unavailable; ingestion and ordinary queries remain available");
                    let mut state = (*core.patterns.state.read().unwrap().clone()).clone();
                    state.failed = true;
                    *core.patterns.state.write().unwrap() = Arc::new(state);
                }
            })?)
    }

    fn run(core: &Core) -> Result<()> {
        let mut catalog = DiskCatalog::open(
            &core.journal.config.data_dir.join("patterns.sqlite"),
            &core.journal.config,
        )?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let mut generation = None;
        loop {
            if core.journal.stopping.load(Ordering::Acquire) {
                return Ok(());
            }
            let snapshot = core.snapshot();
            // Поколение растёт при удалении данных retention; первый проход проверяет старые.
            if generation != Some(snapshot.generation)
                && snapshot.watermark >= catalog.state.checkpoint
            {
                generation = Some(snapshot.generation);
                if let Some(state) = catalog.expire(&snapshot)? {
                    *core.patterns.state.write().unwrap() = Arc::new(state);
                }
            }
            let ready = core.patterns.state.read().unwrap().ready;
            match next_chunk(&snapshot, catalog.state.checkpoint) {
                Some(chunk) if !ready || chunk.watermark > catalog.state.checkpoint => {
                    // Один сегмент за проход: большая история не упирается в deadline запроса.
                    let deadline = Instant::now() + core.journal.config.query_timeout * 10;
                    match runtime.block_on(catalog.advance(core, &chunk, deadline)) {
                        Ok(state) => {
                            *core.patterns.state.write().unwrap() = Arc::new(state);
                            continue;
                        }
                        // Остановка и deadline не фиксируют checkpoint и повторяются.
                        Err(QueryError::Timeout | QueryError::Busy) => {}
                        Err(error) => return Err(anyhow::anyhow!(error)),
                    }
                }
                _ => {}
            }
            thread::park_timeout(Duration::from_millis(100));
        }
    }
}

/// Первый сегмент после checkpoint; watermark ограничен его последней строкой.
pub(crate) fn next_chunk(snapshot: &Snapshot, checkpoint: u64) -> Option<Snapshot> {
    let closed = snapshot
        .closed
        .iter()
        .filter(|segment| segment.meta.last_sequence > checkpoint)
        .min_by_key(|segment| segment.meta.first_sequence);
    let (closed, active, watermark) = match closed {
        Some(segment) => (
            vec![segment.clone()],
            None,
            segment.meta.last_sequence.min(snapshot.watermark),
        ),
        None => (
            vec![],
            snapshot
                .active
                .clone()
                .filter(|active| active.meta.last_sequence > checkpoint),
            snapshot.watermark,
        ),
    };
    (watermark >= checkpoint).then(|| Snapshot {
        closed,
        // Каталог паттернов читает только raw-логи этого chunk, не серии метрик.
        series_files: vec![],
        active,
        watermark,
        generation: snapshot.generation,
        published_at: snapshot.published_at,
    })
}

struct DiskCatalog {
    connection: Connection,
    state: State,
}

impl DiskCatalog {
    fn open(path: &Path, config: &Config) -> Result<Self> {
        let connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_secs(1))?;
        let version: u32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        ensure!(version <= 3, "unsupported patterns schema");
        // Новые правила объединяют старые ID; производные метаданные пересобираются
        // из raw, иначе накопленные временные метки продолжат занимать весь каталог.
        let reset = if version < 3 {
            "DROP TABLE IF EXISTS progress; DROP TABLE IF EXISTS patterns; DROP TABLE IF EXISTS issues;"
        } else {
            ""
        };
        let page_size: usize = connection.query_row("PRAGMA page_size", [], |row| row.get(0))?;
        let page_count: usize = connection.query_row("PRAGMA page_count", [], |row| row.get(0))?;
        ensure!(
            page_count.saturating_mul(page_size) <= config.pattern_catalog_bytes,
            "pattern catalog exceeds disk budget"
        );
        connection.pragma_update(
            None,
            "max_page_count",
            config.pattern_catalog_bytes / page_size,
        )?;
        connection.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL;")?;
        connection.execute_batch(&format!(
            "BEGIN IMMEDIATE; {reset}
            CREATE TABLE IF NOT EXISTS progress(singleton INTEGER PRIMARY KEY CHECK(singleton=1), checkpoint INTEGER NOT NULL, as_of INTEGER NOT NULL, history_from INTEGER, complete INTEGER NOT NULL);
            INSERT OR IGNORE INTO progress VALUES(1,0,0,NULL,1);
            CREATE TABLE IF NOT EXISTS patterns(id TEXT PRIMARY KEY, metadata TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS issues(id TEXT PRIMARY KEY, metadata TEXT NOT NULL);
            PRAGMA user_version=3; COMMIT;",
        ))?;
        let mut state = connection.query_row(
            "SELECT checkpoint,as_of,history_from,complete FROM progress WHERE singleton=1",
            [],
            |row| {
                Ok(State {
                    checkpoint: row.get(0)?,
                    as_of: row.get(1)?,
                    history_from: row.get(2)?,
                    complete: row.get(3)?,
                    ..State::default()
                })
            },
        )?;
        let known = load(&connection, "patterns", &mut state.bytes, 0, config)?;
        let issues = load(&connection, "issues", &mut state.bytes, known.len(), config)?;
        state.known = Arc::new(known);
        state.issues = Arc::new(issues);
        Ok(Self { connection, state })
    }

    async fn advance(
        &mut self,
        core: &Core,
        snapshot: &Snapshot,
        deadline: Instant,
    ) -> Result<State, QueryError> {
        if snapshot.watermark < self.state.checkpoint {
            return Err(anyhow::anyhow!("pattern checkpoint ahead of raw storage").into());
        }
        let config = &core.journal.config;
        let mut next = self.state.clone();
        let mut known = (*next.known).clone();
        let mut issues = (*next.issues).clone();
        let (mut changed, mut changed_issues) = (BTreeSet::new(), BTreeSet::new());
        let mut rows = 0_u64;
        let mut stream = raw_frame(core, snapshot, None)
            .await?
            .filter(col("sequence").gt(lit(next.checkpoint)))?
            .select_columns(&[
                "environment",
                "service",
                "action",
                "level",
                "message",
                "attributes",
                "timestamp",
            ])?
            .execute_stream()
            .await?;
        while let Some(batch) = stream.try_next().await? {
            query::check_deadline(deadline)?;
            if core.journal.stopping.load(Ordering::Acquire) {
                return Err(QueryError::Timeout);
            }
            rows = rows
                .checked_add(batch.num_rows() as u64)
                .context("pattern row overflow")?;
            for row in 0..batch.num_rows() {
                let environment = optional_string(&batch, "environment", row)?;
                let service = string(&batch, "service", row)?;
                let action = optional_string(&batch, "action", row)?;
                let level = LogLevel::from_code(column::<UInt8Array>(&batch, "level")?.value(row))?;
                let message = string(&batch, "message", row)?;
                let timestamp = column::<TimestampNanosecondArray>(&batch, "timestamp")?.value(row);
                let identity = identify(environment, service, level.as_str(), message);
                next.history_from = Some(
                    next.history_from
                        .map_or(timestamp, |old| old.min(timestamp)),
                );
                let issue = if is_error(level) {
                    let exception = Exception::parse(string(&batch, "attributes", row)?);
                    fingerprint(environment, service, action, level, &identity, &exception)
                } else {
                    None
                };
                if let Some(issue) = issue {
                    observe(
                        &mut issues,
                        &mut changed_issues,
                        &mut next.bytes,
                        timestamp,
                        issue.id.clone(),
                        || KnownIssue {
                            id: issue.id,
                            environment: environment.map(str::to_owned),
                            service: service.into(),
                            action: action.map(str::to_owned),
                            exception_type: issue.exception_type,
                            frames: issue.frames,
                            pattern_id: identity.id.clone(),
                            template: identity.template.clone(),
                            parts: identity.parts.clone(),
                            exact: identity.exact,
                            first: timestamp,
                            last: timestamp,
                        },
                    )?;
                }
                observe(
                    &mut known,
                    &mut changed,
                    &mut next.bytes,
                    timestamp,
                    identity.id.clone(),
                    || Known {
                        id: identity.id,
                        environment: environment.map(str::to_owned),
                        service: service.into(),
                        level: level.as_str().into(),
                        template: identity.template,
                        parts: identity.parts,
                        exact: identity.exact,
                        first: timestamp,
                        last: timestamp,
                    },
                )?;
                if known.len() + issues.len() > config.pattern_max_entries
                    || next.bytes > config.pattern_catalog_bytes
                {
                    return Err(anyhow::anyhow!(
                        "pattern catalog capacity exhausted: patterns={}, issues={}, entries_limit={}, estimated_bytes={}, bytes_limit={}",
                        known.len(), issues.len(), config.pattern_max_entries, next.bytes, config.pattern_catalog_bytes
                    ).into());
                }
            }
        }
        // Sequence непрерывны при приёме: недостающие строки означают удаление retention.
        if rows != snapshot.watermark - next.checkpoint {
            next.complete = false;
        }
        next.checkpoint = snapshot.watermark;
        next.as_of = snapshot.published_at;
        next.ready = true;
        query::check_deadline(deadline)?;
        let transaction = self.connection.transaction().map_err(anyhow::Error::from)?;
        store(&transaction, "patterns", &changed, &known)?;
        store(&transaction, "issues", &changed_issues, &issues)?;
        transaction
            .execute(
                "UPDATE progress SET checkpoint=?1,as_of=?2,history_from=?3,complete=?4 WHERE singleton=1",
                params![next.checkpoint, next.as_of, next.history_from, next.complete],
            )
            .map_err(anyhow::Error::from)?;
        transaction.commit().map_err(anyhow::Error::from)?;
        next.known = Arc::new(known);
        next.issues = Arc::new(issues);
        self.state = next.clone();
        Ok(next)
    }

    /// Удаляет записи без строк в хранилище: их последнее событие раньше самого старого
    /// хранимого, либо хранилище пусто. Освобождает лимиты каталога и без нового приёма.
    fn expire(&mut self, snapshot: &Snapshot) -> Result<Option<State>> {
        let oldest = snapshot
            .closed
            .iter()
            .map(|segment| &segment.meta)
            .chain(snapshot.active.as_ref().map(|active| &active.meta))
            .filter(|meta| meta.rows > 0)
            .map(|meta| meta.min_timestamp)
            .min();
        let bound = oldest.unwrap_or(i64::MAX);
        let mut next = self.state.clone();
        let mut known = (*next.known).clone();
        let mut issues = (*next.issues).clone();
        let removed = prune(&mut known, &mut next.bytes, bound)?;
        let removed_issues = prune(&mut issues, &mut next.bytes, bound)?;
        if removed.is_empty() && removed_issues.is_empty() {
            return Ok(None);
        }
        // История каталога теперь начинается не раньше самого старого хранимого события.
        next.history_from =
            oldest.and_then(|oldest| next.history_from.map(|from| from.max(oldest)));
        let transaction = self.connection.transaction()?;
        for (table, ids) in [("patterns", &removed), ("issues", &removed_issues)] {
            for id in ids {
                transaction.execute(&format!("DELETE FROM {table} WHERE id=?1"), [id])?;
            }
        }
        transaction.execute(
            "UPDATE progress SET history_from=?1 WHERE singleton=1",
            params![next.history_from],
        )?;
        transaction.commit()?;
        next.known = Arc::new(known);
        next.issues = Arc::new(issues);
        self.state = next.clone();
        Ok(Some(next))
    }
}

/// Удаляет записи с последним событием раньше `oldest` и их оценку размера (как в `observe`).
fn prune<T: Lifetime>(
    map: &mut BTreeMap<String, Arc<T>>,
    bytes: &mut usize,
    oldest: i64,
) -> Result<Vec<String>> {
    let expired: Vec<String> = map
        .iter()
        .filter(|(_, entry)| entry.last() < oldest)
        .map(|(id, _)| id.clone())
        .collect();
    for id in &expired {
        let entry = map.remove(id).expect("listed entry");
        let size = serde_json::to_vec(&*entry)?.len() + id.len() + 256;
        *bytes = bytes.saturating_sub(size);
    }
    Ok(expired)
}

/// Обновляет lifetime-границы или добавляет запись с учётом её размера.
fn observe<T: Lifetime>(
    map: &mut BTreeMap<String, Arc<T>>,
    changed: &mut BTreeSet<String>,
    bytes: &mut usize,
    timestamp: i64,
    id: String,
    create: impl FnOnce() -> T,
) -> Result<(), QueryError> {
    if let Some(entry) = map.get_mut(&id) {
        let (first, last) = Arc::make_mut(entry).span();
        if timestamp < *first || timestamp > *last {
            *first = (*first).min(timestamp);
            *last = (*last).max(timestamp);
            changed.insert(id);
        }
        return Ok(());
    }
    let entry = create();
    *bytes = bytes.saturating_add(
        serde_json::to_vec(&entry)
            .map_err(anyhow::Error::from)?
            .len()
            + id.len()
            + 256,
    );
    map.insert(id.clone(), Arc::new(entry));
    changed.insert(id);
    Ok(())
}

fn load<T: for<'de> Deserialize<'de>>(
    connection: &Connection,
    table: &str,
    bytes: &mut usize,
    loaded: usize,
    config: &Config,
) -> Result<BTreeMap<String, Arc<T>>> {
    let mut entries = BTreeMap::new();
    let mut statement = connection.prepare(&format!("SELECT id,metadata FROM {table}"))?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let json: String = row.get(1)?;
        *bytes = bytes.saturating_add(json.len() + id.len() + 256);
        ensure!(
            loaded + entries.len() < config.pattern_max_entries
                && *bytes <= config.pattern_catalog_bytes,
            "pattern catalog exceeds configured limits"
        );
        let value: serde_json::Value = serde_json::from_str(&json)?;
        ensure!(value["id"] == id, "catalog metadata id mismatch");
        entries.insert(id, Arc::new(serde_json::from_value(value)?));
    }
    Ok(entries)
}

fn store<T: Serialize>(
    transaction: &rusqlite::Transaction<'_>,
    table: &str,
    changed: &BTreeSet<String>,
    entries: &BTreeMap<String, Arc<T>>,
) -> Result<(), QueryError> {
    for id in changed {
        transaction
            .execute(
                &format!("INSERT INTO {table} VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET metadata=excluded.metadata"),
                params![id, serde_json::to_string(&*entries[id]).map_err(anyhow::Error::from)?],
            )
            .map_err(anyhow::Error::from)?;
    }
    Ok(())
}

pub(crate) fn column<'a, T: 'static>(batch: &'a RecordBatch, name: &str) -> Result<&'a T> {
    batch
        .column_by_name(name)
        .and_then(|array| array.as_any().downcast_ref())
        .with_context(|| format!("invalid pattern column {name}"))
}

pub(crate) fn string<'a>(batch: &'a RecordBatch, name: &str, row: usize) -> Result<&'a str> {
    Ok(column::<StringArray>(batch, name)?.value(row))
}

pub(crate) fn optional_string<'a>(
    batch: &'a RecordBatch,
    name: &str,
    row: usize,
) -> Result<Option<&'a str>> {
    let strings = column::<StringArray>(batch, name)?;
    Ok((!strings.is_null(row)).then(|| strings.value(row)))
}
