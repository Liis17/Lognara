//! Графики метрик: список серий и bucket-значения поверх Parquet серий и открытых точек.
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::Arc,
};

use anyhow::{Context, Result};
use datafusion::{
    arrow::{
        array::{
            Array, Float64Array, Int64Array, RecordBatch, StringArray, TimestampNanosecondArray,
        },
        datatypes::IntervalMonthDayNano,
    },
    common::ScalarValue,
    datasource::MemTable,
    functions::datetime::expr_fn::date_bin,
    functions_aggregate::expr_fn::{last_value, max, min, sum},
    prelude::*,
};
use futures::TryStreamExt;
use serde::{Deserialize, Serialize};

use crate::{
    catalog::{SeriesEnvironment, SeriesName},
    definitions::{Aggregation, Definition, Kind},
    model::format_timestamp,
    query::{Filters, PreparedFilter, QueryError},
    rollup::{self, DAY_NANOS, RollupFile},
    series::{self, Origin},
    storage::{Core, Snapshot},
};

const MAX_BUCKETS: i128 = 10_000;
const MAX_SERIES: usize = 20;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Split {
    #[default]
    None,
    Instance,
    Labels,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryRequest {
    pub service: String,
    pub name: String,
    pub from: String,
    pub to: String,
    pub step_seconds: u64,
    #[serde(default)]
    pub split: Split,
    #[serde(default)]
    pub origin: Option<Origin>,
    #[serde(default)]
    pub environment: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct QueryResponse {
    pub kind: Kind,
    pub aggregation: Aggregation,
    pub unit: String,
    pub buckets: Vec<String>,
    pub series: Vec<SeriesLine>,
    pub as_of: String,
}

#[derive(Debug, Serialize)]
pub struct SeriesLine {
    pub label: String,
    pub values: Vec<Option<f64>>,
}

#[derive(Debug, Serialize)]
pub struct ListResponse {
    pub metrics: Vec<Metric>,
}

#[derive(Debug, Serialize)]
pub struct Metric {
    pub service: String,
    pub name: String,
    pub origin: String,
    pub first_seen: Option<String>,
    pub last_seen: Option<String>,
    pub definition: Option<Definition>,
    pub environments: Vec<String>,
}

/// Ключ серии без split: окружение, сервер, бэкенд, экземпляр, origin, правило и метки.
type Identity = (
    Option<String>,
    String,
    String,
    Option<String>,
    String,
    Option<String>,
    String,
);

#[derive(Clone, Copy)]
struct Totals {
    sum: f64,
    count: i64,
    min: f64,
    max: f64,
    last: f64,
}

impl Totals {
    fn add(&mut self, other: Totals) {
        self.sum += other.sum;
        self.count += other.count;
        self.min = self.min.min(other.min);
        self.max = self.max.max(other.max);
        self.last += other.last;
    }
}

/// Имена из каталога и открытого сегмента; диапазон времени точек по (service, name, origin).
fn merged_names(
    names: &[SeriesName],
    snapshot: &Snapshot,
) -> BTreeMap<(String, String, String), (i64, i64)> {
    let mut merged = BTreeMap::new();
    let catalog = names.iter().map(|name| {
        (
            (name.service.clone(), name.name.clone(), name.origin.clone()),
            (name.first_seen, name.last_seen),
        )
    });
    let active = snapshot.active.iter().flat_map(|active| {
        active.names.iter().map(|((service, name, origin), range)| {
            ((service.clone(), name.clone(), origin.to_string()), *range)
        })
    });
    for (key, (first, last)) in catalog.chain(active) {
        merged
            .entry(key)
            .and_modify(|(seen_first, seen_last): &mut (i64, i64)| {
                *seen_first = (*seen_first).min(first);
                *seen_last = (*seen_last).max(last);
            })
            .or_insert((first, last));
    }
    merged
}

/// Имена, окружения и определения каталога.
type CatalogRows = (Vec<SeriesName>, Vec<SeriesEnvironment>, Vec<Definition>);

/// Имена, окружения и определения из каталога; SQLite читается вне async-потока.
async fn catalog(core: Arc<Core>) -> Result<CatalogRows, QueryError> {
    tokio::task::spawn_blocking(move || -> Result<CatalogRows, QueryError> {
        let catalog = core.journal.catalog.lock().unwrap();
        Ok((
            catalog.series_names()?,
            catalog.series_environments()?,
            catalog.definitions()?,
        ))
    })
    .await
    .map_err(|error| QueryError::Storage(error.into()))?
}

/// Origin для определения без точек: правило, иначе gauge для типа gauge, иначе counter.
fn default_origin(definition: &Definition) -> Origin {
    match (definition.rule.is_some(), definition.kind) {
        (true, _) => Origin::Rule,
        (false, Kind::Gauge) => Origin::Gauge,
        (false, _) => Origin::Counter,
    }
}

/// GET /v1/series: серии каталога и открытого сегмента, окружения, определения без точек и определения у серий.
pub async fn list(core: Arc<Core>) -> Result<ListResponse, QueryError> {
    let snapshot = core.snapshot();
    let (names, catalog_environments, definitions) = catalog(core.clone()).await?;
    let mut environments: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();
    let active_environments = snapshot
        .active
        .iter()
        .flat_map(|active| active.environments.iter().cloned());
    for (service, name, environment) in catalog_environments.into_iter().chain(active_environments)
    {
        environments
            .entry((service, name))
            .or_default()
            .insert(environment);
    }
    let mut metrics: BTreeMap<(String, String, String), Metric> = BTreeMap::new();
    for ((service, name, origin), (first, last)) in merged_names(&names, &snapshot) {
        metrics.insert(
            (service.clone(), name.clone(), origin.clone()),
            Metric {
                service,
                name,
                origin,
                first_seen: Some(format_timestamp(first)),
                last_seen: Some(format_timestamp(last)),
                definition: None,
                environments: vec![],
            },
        );
    }
    for definition in &definitions {
        let observed = metrics
            .keys()
            .any(|(service, name, _)| *service == definition.service && *name == definition.name);
        if !observed {
            let origin = default_origin(definition).as_str().to_owned();
            metrics.insert(
                (
                    definition.service.clone(),
                    definition.name.clone(),
                    origin.clone(),
                ),
                Metric {
                    service: definition.service.clone(),
                    name: definition.name.clone(),
                    origin,
                    first_seen: None,
                    last_seen: None,
                    definition: None,
                    environments: vec![],
                },
            );
        }
    }
    let mut metrics: Vec<Metric> = metrics.into_values().collect();
    for metric in &mut metrics {
        metric.definition = definitions
            .iter()
            .find(|definition| {
                definition.service == metric.service && definition.name == metric.name
            })
            .cloned();
        metric.environments = environments
            .get(&(metric.service.clone(), metric.name.clone()))
            .map(|set| set.iter().cloned().collect())
            .unwrap_or_default();
    }
    Ok(ListResponse { metrics })
}

/// POST /v1/series/query: значения по bucket и split; кумулятивные серии переводятся в приращения.
pub async fn query(core: Arc<Core>, request: QueryRequest) -> Result<QueryResponse, QueryError> {
    let filter = PreparedFilter::new(&request.from, &request.to, &Filters::new())?;
    if !(1..=604_800).contains(&request.step_seconds) {
        return Err(QueryError::Invalid("step_seconds must be 1..604800".into()));
    }
    let step = request.step_seconds as i128 * 1_000_000_000;
    let first = (filter.from as i128).div_euclid(step) * step;
    let last = (filter.to as i128 - 1).div_euclid(step) * step;
    let count = (last - first) / step + 1;
    if first < i64::MIN as i128 || count > MAX_BUCKETS {
        return Err(QueryError::Invalid(
            "query exceeds 10000 buckets or timestamp range".into(),
        ));
    }
    let snapshot = core.snapshot();
    let (names, _, definitions) = catalog(core.clone()).await?;
    let definition = definitions.iter().find(|definition| {
        definition.service == request.service && definition.name == request.name
    });
    let (kind, aggregation, unit) = match definition {
        Some(definition) => {
            // Кумулятивная серия всегда агрегируется суммой приращений.
            let aggregation = if definition.kind == Kind::Cumulative {
                Aggregation::Sum
            } else {
                definition.aggregation
            };
            (definition.kind, aggregation, definition.unit.clone())
        }
        None => {
            let origins: Vec<String> = merged_names(&names, &snapshot)
                .into_keys()
                .filter(|(service, name, _)| *service == request.service && *name == request.name)
                .map(|(_, _, origin)| origin)
                .collect();
            let gauge = match request.origin {
                Some(origin) => origin == Origin::Gauge,
                None => !origins.is_empty() && origins.iter().all(|origin| origin == "gauge"),
            };
            if gauge {
                (Kind::Gauge, Aggregation::Avg, String::new())
            } else {
                (Kind::Delta, Aggregation::Sum, String::new())
            }
        }
    };
    // Приращению нужен один bucket до начала окна: первое приращение считается от него.
    let read_from = if kind == Kind::Cumulative {
        (first - step).max(i64::MIN as i128)
    } else {
        first
    };
    let batches = read_buckets(
        &core,
        &snapshot,
        &request,
        step as i64,
        read_from as i64,
        filter.to,
    )
    .await?;
    let values = finish(&batches, kind, aggregation)?;
    let starts: Vec<i64> = (0..count)
        .map(|index| (first + index * step) as i64)
        .collect();
    let (first, last) = (first as i64, last as i64);
    let mut ranked: Vec<(f64, String, &BTreeMap<i64, f64>)> = values
        .iter()
        .filter(|(_, points)| points.range(first..=last).next().is_some())
        .map(|(label, points)| {
            let weight = points
                .range(first..=last)
                .map(|(_, value)| value.abs())
                .sum();
            (weight, label.clone(), points)
        })
        .collect();
    ranked.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    ranked.truncate(MAX_SERIES);
    ranked.sort_by(|a, b| a.1.cmp(&b.1));
    let series = ranked
        .into_iter()
        .map(|(_, label, points)| SeriesLine {
            label,
            values: starts
                .iter()
                .map(|bucket| points.get(bucket).copied())
                .collect(),
        })
        .collect();
    Ok(QueryResponse {
        kind,
        aggregation,
        unit,
        buckets: starts
            .iter()
            .map(|bucket| format_timestamp(*bucket))
            .collect(),
        series,
        as_of: format_timestamp(snapshot.published_at),
    })
}

/// Строки (bucket, split, серия) из сырых точек и агрегатов суток. Повторно доставленные точки схлопываются до агрегации.
async fn read_buckets(
    core: &Core,
    snapshot: &Snapshot,
    request: &QueryRequest,
    step: i64,
    from: i64,
    to: i64,
) -> Result<Vec<RecordBatch>> {
    let (files, ranges) = plan(core, snapshot, request.step_seconds, from, to);
    let raw = raw_rows(core, snapshot, request, step, &ranges).await?;
    let rolled = rollup_rows(core, &files, request, step).await?;
    let rows = match (raw, rolled) {
        (Some(raw), Some(rolled)) => raw.union(rolled)?,
        (Some(rows), None) | (None, Some(rows)) => rows,
        (None, None) => return Ok(vec![]),
    };
    Ok(merge(rows)?.execute_stream().await?.try_collect().await?)
}

/// Суточные агрегаты для полных суток окна и диапазоны сырых точек для остальных; соседние диапазоны сливаются.
fn plan(
    core: &Core,
    snapshot: &Snapshot,
    step_seconds: u64,
    from: i64,
    to: i64,
) -> (Vec<Arc<RollupFile>>, Vec<(i64, i64)>) {
    let rollups = core.rollups.read().unwrap().clone();
    let (from, to) = (i128::from(from), i128::from(to));
    let (mut files, mut ranges) = (vec![], Vec::<(i64, i64)>::new());
    let mut day = from.div_euclid(DAY_NANOS) * DAY_NANOS;
    while day < to {
        let end = day + DAY_NANOS;
        let whole = day >= from && end <= to;
        let file = if whole {
            rollup::usable_rollup(&rollups, snapshot, day, step_seconds)
        } else {
            None
        };
        match file {
            Some(file) => files.push(file),
            None => {
                let (start, stop) = (day.max(from) as i64, end.min(to) as i64);
                match ranges.last_mut() {
                    Some(last) if last.1 == start => last.1 = stop,
                    _ => ranges.push((start, stop)),
                }
            }
        }
        day = end;
    }
    (files, ranges)
}

/// Строки сырых точек в диапазонах; None, если диапазонов нет.
async fn raw_rows(
    core: &Core,
    snapshot: &Snapshot,
    request: &QueryRequest,
    step: i64,
    ranges: &[(i64, i64)],
) -> Result<Option<DataFrame>> {
    let (Some(&(from, _)), Some(&(_, to)), Some(inside)) = (
        ranges.first(),
        ranges.last(),
        ranges
            .iter()
            .map(|&(start, stop)| {
                col("timestamp")
                    .gt_eq(timestamp(start))
                    .and(col("timestamp").lt(timestamp(stop)))
            })
            .reduce(Expr::or),
    ) else {
        return Ok(None);
    };
    let frame = dedup(
        frame(core, snapshot, from, to)
            .await?
            .filter(inside.and(scope(request)))?,
    )?;
    let mut columns = keys(bucket(step, col("timestamp")), split_key(request.split));
    columns.extend([
        col("value").alias("sum"),
        lit(1i64).alias("count"),
        col("value").alias("min"),
        col("value").alias("max"),
        col("value").alias("last"),
        col("timestamp").alias("last_ts"),
    ]);
    Ok(Some(frame.select(columns)?))
}

/// Строки агрегатов суток той же формы, что сырые; None, если агрегатов нет.
async fn rollup_rows(
    core: &Core,
    files: &[Arc<RollupFile>],
    request: &QueryRequest,
    step: i64,
) -> Result<Option<DataFrame>> {
    if files.is_empty() {
        return Ok(None);
    }
    let paths: Vec<String> = files
        .iter()
        .map(|file| file.path.to_string_lossy().into_owned())
        .collect();
    let frame = session(core)
        .read_parquet(
            paths,
            ParquetReadOptions::default().schema(&rollup::schema()),
        )
        .await?
        .filter(scope(request))?;
    let mut columns = keys(bucket(step, col("bucket")), split_key(request.split));
    columns.extend(["sum", "count", "min", "max", "last", "last_ts"].map(col));
    Ok(Some(frame.select(columns)?))
}

/// Слияние строк одной корзины, split и серии: суммы, минимум, максимум и последнее значение по времени.
fn merge(rows: DataFrame) -> Result<DataFrame> {
    Ok(rows.aggregate(
        keys(col("bucket"), col("split")),
        vec![
            sum(col("sum")).alias("sum"),
            sum(col("count")).alias("count"),
            min(col("min")).alias("min"),
            max(col("max")).alias("max"),
            last_value(
                col("last"),
                vec![
                    col("last_ts").sort(true, false),
                    col("last").sort(true, false),
                ],
            )
            .alias("last"),
            max(col("last_ts")).alias("last_ts"),
        ],
    )?)
}

/// Столбцы строки: корзина, split и серия (окружение, сервер, бэкенд, экземпляр, origin, правило, метки).
fn keys(bucket: Expr, split: Expr) -> Vec<Expr> {
    let identity = [
        "environment",
        "server",
        "backend",
        "service_instance",
        "origin",
        "rule_id",
        "labels",
    ]
    .map(col);
    [bucket.alias("bucket"), split.alias("split")]
        .into_iter()
        .chain(identity)
        .collect()
}

/// Корзина запроса: date_bin от эпохи, как у агрегатов суток.
fn bucket(step: i64, time: Expr) -> Expr {
    date_bin(
        lit(ScalarValue::IntervalMonthDayNano(Some(
            IntervalMonthDayNano::new(0, 0, step),
        ))),
        time,
        timestamp(0),
    )
}

/// Ключ split: пустая строка без разбиения, экземпляр или метки.
fn split_key(split: Split) -> Expr {
    match split {
        Split::None => lit(""),
        Split::Instance => coalesce(vec![col("service_instance"), lit("")]),
        Split::Labels => col("labels"),
    }
}

/// Условия серии: сервис, имя и, если заданы, origin и окружение.
fn scope(request: &QueryRequest) -> Expr {
    let mut predicate = col("service")
        .eq(lit(request.service.as_str()))
        .and(col("name").eq(lit(request.name.as_str())));
    if let Some(origin) = request.origin {
        predicate = predicate.and(col("origin").eq(lit(origin.as_str())));
    }
    if let Some(environment) = &request.environment {
        predicate = predicate.and(col("environment").eq(lit(environment.as_str())));
    }
    predicate
}

/// Повторно доставленные точки схлопываются: одна строка на серию, время и event_id, значение - максимум.
pub(crate) fn dedup(frame: DataFrame) -> Result<DataFrame> {
    Ok(frame.aggregate(
        vec![
            col("service"),
            col("environment"),
            col("server"),
            col("backend"),
            col("service_instance"),
            col("name"),
            col("origin"),
            col("rule_id"),
            col("labels"),
            col("timestamp"),
            col("event_id"),
        ],
        vec![max(col("value")).alias("value")],
    )?)
}

/// Точки серий: Parquet файлов, пересекающих окно, и открытые точки сегмента из того же снимка.
pub(crate) async fn frame(
    core: &Core,
    snapshot: &Snapshot,
    from: i64,
    to: i64,
) -> Result<DataFrame> {
    let context = session(core);
    let paths: Vec<String> = snapshot
        .series_files
        .iter()
        .filter(|file| file.meta.overlaps(from, to))
        .map(|file| file.path.to_string_lossy().into_owned())
        .collect();
    let memory = snapshot
        .active
        .as_ref()
        .map(|active| active.points.clone())
        .unwrap_or_default();
    let memory = if memory.is_empty() {
        vec![RecordBatch::new_empty(series::schema())]
    } else {
        memory
    };
    let mut frame =
        context.read_table(Arc::new(MemTable::try_new(series::schema(), vec![memory])?))?;
    if !paths.is_empty() {
        // Явная схема серий сохраняет типы при union с MemTable.
        let parquet = context
            .read_parquet(
                paths,
                ParquetReadOptions::default().schema(&series::schema()),
            )
            .await?;
        frame = frame.union(parquet)?;
    }
    Ok(frame)
}

/// Контекст чтения серий: мало партиций и общий пул памяти core.
fn session(core: &Core) -> SessionContext {
    let config = SessionConfig::new()
        .with_target_partitions(2)
        .with_batch_size(8192)
        .with_collect_statistics(false)
        .with_parquet_pruning(true);
    SessionContext::new_with_config_rt(config, core.query_runtime.clone())
}

/// Значения по split-ключу и bucket: приращения для кумулятивных, иначе агрегат по строкам.
fn finish(
    batches: &[RecordBatch],
    kind: Kind,
    aggregation: Aggregation,
) -> Result<BTreeMap<String, BTreeMap<i64, f64>>> {
    let mut walks: HashMap<(String, Identity), BTreeMap<i64, f64>> = HashMap::new();
    let mut totals: BTreeMap<String, BTreeMap<i64, Totals>> = BTreeMap::new();
    for batch in batches {
        let buckets = column::<TimestampNanosecondArray>(batch, "bucket")?;
        let splits = column::<StringArray>(batch, "split")?;
        let environments = column::<StringArray>(batch, "environment")?;
        let servers = column::<StringArray>(batch, "server")?;
        let backends = column::<StringArray>(batch, "backend")?;
        let instances = column::<StringArray>(batch, "service_instance")?;
        let origins = column::<StringArray>(batch, "origin")?;
        let rule_ids = column::<StringArray>(batch, "rule_id")?;
        let labels = column::<StringArray>(batch, "labels")?;
        let sums = column::<Float64Array>(batch, "sum")?;
        let counts = column::<Int64Array>(batch, "count")?;
        let mins = column::<Float64Array>(batch, "min")?;
        let maxes = column::<Float64Array>(batch, "max")?;
        let lasts = column::<Float64Array>(batch, "last")?;
        for row in 0..batch.num_rows() {
            let total = Totals {
                sum: sums.value(row),
                count: counts.value(row),
                min: mins.value(row),
                max: maxes.value(row),
                last: lasts.value(row),
            };
            let bucket = buckets.value(row);
            let split = splits.value(row).to_owned();
            if kind == Kind::Cumulative {
                let identity: Identity = (
                    text(environments, row),
                    servers.value(row).to_owned(),
                    backends.value(row).to_owned(),
                    text(instances, row),
                    origins.value(row).to_owned(),
                    text(rule_ids, row),
                    labels.value(row).to_owned(),
                );
                walks
                    .entry((split, identity))
                    .or_default()
                    .insert(bucket, total.last);
            } else {
                totals
                    .entry(split)
                    .or_default()
                    .entry(bucket)
                    .and_modify(|all| all.add(total))
                    .or_insert(total);
            }
        }
    }
    let mut values: BTreeMap<String, BTreeMap<i64, f64>> = BTreeMap::new();
    for ((split, _), points) in walks {
        let series = values.entry(split).or_default();
        let mut previous = None;
        for (bucket, last) in points {
            if let Some(before) = previous {
                // Уменьшение счётчика считаем перезапуском: приращение равно текущему значению.
                let increase = if last >= before { last - before } else { last };
                *series.entry(bucket).or_insert(0.0) += increase;
            }
            previous = Some(last);
        }
    }
    for (split, buckets) in totals {
        let points = buckets
            .into_iter()
            .map(|(bucket, total)| (bucket, aggregate_value(total, aggregation)))
            .collect();
        values.insert(split, points);
    }
    Ok(values)
}

fn aggregate_value(total: Totals, aggregation: Aggregation) -> f64 {
    match aggregation {
        Aggregation::Sum => total.sum,
        Aggregation::Avg => total.sum / total.count as f64,
        Aggregation::Min => total.min,
        Aggregation::Max => total.max,
        Aggregation::Last => total.last,
    }
}

fn column<'a, T: Array + 'static>(batch: &'a RecordBatch, name: &str) -> Result<&'a T> {
    batch
        .column_by_name(name)
        .and_then(|column| column.as_any().downcast_ref::<T>())
        .with_context(|| format!("unexpected column {name}"))
}

fn text(column: &StringArray, row: usize) -> Option<String> {
    (!column.is_null(row)).then(|| column.value(row).to_owned())
}

pub(crate) fn timestamp(value: i64) -> Expr {
    lit(ScalarValue::TimestampNanosecond(
        Some(value),
        Some("UTC".into()),
    ))
}
