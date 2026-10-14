//! Derived lifetime metadata. Raw events remain authoritative for every count.
use crate::{
    analytics::raw_frame,
    config::Config,
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

    pub(crate) fn start(core: Arc<Core>) -> Result<JoinHandle<()>> {
        Ok(thread::Builder::new()
            .name("lognara-patterns".into())
            .spawn(move || {
                if let Err(error) = Self::run(&core) {
                    tracing::error!(%error, "pattern catalog unavailable; ingestion and ordinary queries remain available");
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
        loop {
            if core.journal.stopping.load(Ordering::Acquire) {
                return Ok(());
            }
            let snapshot = core.snapshot();
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
fn next_chunk(snapshot: &Snapshot, checkpoint: u64) -> Option<Snapshot> {
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
        ensure!(version <= 1, "unsupported patterns schema");
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
        connection.execute_batch(
            "PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS progress(singleton INTEGER PRIMARY KEY CHECK(singleton=1), checkpoint INTEGER NOT NULL, as_of INTEGER NOT NULL, history_from INTEGER, complete INTEGER NOT NULL);
            INSERT OR IGNORE INTO progress VALUES(1,0,0,NULL,1);
            CREATE TABLE IF NOT EXISTS patterns(id TEXT PRIMARY KEY, metadata TEXT NOT NULL);
            PRAGMA user_version=1;",
        )?;
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
        let mut known = BTreeMap::new();
        {
            let mut statement = connection.prepare("SELECT id,metadata FROM patterns")?;
            let mut rows = statement.query([])?;
            while let Some(row) = rows.next()? {
                let id: String = row.get(0)?;
                let json: String = row.get(1)?;
                state.bytes = state.bytes.saturating_add(json.len() + id.len() + 256);
                ensure!(
                    known.len() < config.pattern_max_entries
                        && state.bytes <= config.pattern_catalog_bytes,
                    "pattern catalog exceeds configured limits"
                );
                let entry: Known = serde_json::from_str(&json)?;
                ensure!(entry.id == id, "pattern metadata id mismatch");
                known.insert(id, Arc::new(entry));
            }
        }
        state.known = Arc::new(known);
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
        let mut changed = BTreeSet::new();
        let mut rows = 0_u64;
        let mut stream = raw_frame(core, snapshot, None)
            .await?
            .filter(col("sequence").gt(lit(next.checkpoint)))?
            .select_columns(&["environment", "service", "level", "message", "timestamp"])?
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
                let level = LogLevel::from_code(column::<UInt8Array>(&batch, "level")?.value(row))?
                    .as_str();
                let message = string(&batch, "message", row)?;
                let timestamp = column::<TimestampNanosecondArray>(&batch, "timestamp")?.value(row);
                let identity = identify(environment, service, level, message);
                next.history_from = Some(
                    next.history_from
                        .map_or(timestamp, |old| old.min(timestamp)),
                );
                if let Some(entry) = known.get_mut(&identity.id) {
                    if timestamp < entry.first || timestamp > entry.last {
                        let entry = Arc::make_mut(entry);
                        entry.first = entry.first.min(timestamp);
                        entry.last = entry.last.max(timestamp);
                        changed.insert(identity.id);
                    }
                    continue;
                }
                let entry = Known {
                    id: identity.id.clone(),
                    environment: environment.map(str::to_owned),
                    service: service.into(),
                    level: level.into(),
                    template: identity.template,
                    parts: identity.parts,
                    exact: identity.exact,
                    first: timestamp,
                    last: timestamp,
                };
                next.bytes = next.bytes.saturating_add(
                    serde_json::to_vec(&entry)
                        .map_err(anyhow::Error::from)?
                        .len()
                        + entry.id.len()
                        + 256,
                );
                if known.len() >= config.pattern_max_entries
                    || next.bytes > config.pattern_catalog_bytes
                {
                    return Err(anyhow::anyhow!("pattern catalog capacity exhausted").into());
                }
                known.insert(identity.id.clone(), Arc::new(entry));
                changed.insert(identity.id);
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
        for id in &changed {
            transaction
                .execute(
                    "INSERT INTO patterns VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET metadata=excluded.metadata",
                    params![id, serde_json::to_string(&*known[id]).map_err(anyhow::Error::from)?],
                )
                .map_err(anyhow::Error::from)?;
        }
        transaction
            .execute(
                "UPDATE progress SET checkpoint=?1,as_of=?2,history_from=?3,complete=?4 WHERE singleton=1",
                params![next.checkpoint, next.as_of, next.history_from, next.complete],
            )
            .map_err(anyhow::Error::from)?;
        transaction.commit().map_err(anyhow::Error::from)?;
        next.known = Arc::new(known);
        self.state = next.clone();
        Ok(next)
    }
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
