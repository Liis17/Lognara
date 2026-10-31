//! Графики метрик: список серий и bucket-значения поверх Parquet серий и открытых точек.
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlertQueryRequest {
    pub server: String,
    pub service: String,
    pub name: String,
    pub origin: Origin,
    #[serde(default)]
    pub environment: Option<String>,
    pub windows_seconds: Vec<u64>,
    #[serde(default = "alert_query_need_increase")]
    pub need_increase: bool,
}

fn alert_query_need_increase() -> bool {
    true
}

#[derive(Debug, Serialize)]
pub struct AlertQueryResponse {
    pub as_of: String,
    pub last_point_at: Option<String>,
    pub windows: Vec<AlertWindow>,
}

#[derive(Debug, Serialize)]
pub struct AlertWindow {
    pub seconds: u64,
    pub count: u64,
    pub sum: f64,
    pub avg: Option<f64>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub increase: Option<f64>,
}

#[cfg(test)]
#[derive(Clone)]
struct AlertPoint {
    timestamp: i64,
    identity: Identity,
    value: f64,
}

/// Query-auth endpoint for alerts. It intentionally reads raw points and does not bucket,
/// split or truncate the selected source/environment.
pub async fn alert_query(
    core: Arc<Core>,
    request: AlertQueryRequest,
) -> Result<AlertQueryResponse, QueryError> {
    validate_alert_request(&request)?;
    let as_of = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| QueryError::Storage(error.into()))?
        .as_nanos()
        .min(i64::MAX as u128) as i64;
    let longest = *request.windows_seconds.iter().max().unwrap();
    let from = as_of.saturating_sub((longest as i64).saturating_mul(1_000_000_000));
    let snapshot = core.snapshot();
    let window_frame = frame(&core, &snapshot, from, as_of)
        .await?
        .filter(alert_scope(&request))?
        .filter(
            col("timestamp")
                .gt_eq(timestamp(from))
                .and(col("timestamp").lt(timestamp(as_of))),
        )?;
    let target_identities = if request.need_increase {
        collect_alert_identities(window_frame.clone()).await?.0
    } else {
        BTreeSet::new()
    };

    // Freshness is only needed to distinguish data in the last five minutes. Restrict that
    // lookup to recent files; older points do not improve a stale/no-data decision.
    let freshness_from = as_of.saturating_sub(300_i64 * 1_000_000_000);
    let last_point = latest_alert_timestamp(
        frame(&core, &snapshot, freshness_from, as_of)
            .await?
            .filter(alert_scope(&request))?
            .filter(
                col("timestamp")
                    .gt_eq(timestamp(freshness_from))
                    .and(col("timestamp").lt(timestamp(as_of))),
            )?,
    )
    .await?;

    // The exact increase baseline is the latest pre-window point per observed identity. The
    // catalog only indexes metric scope, so older files still need a targeted scan when a
    // requested identity may have a baseline.
    let mut baselines = BTreeMap::<Identity, (i64, f64)>::new();
    if request.need_increase
        && !target_identities.is_empty()
        && metric_scope_has_points_before(&core, &snapshot, &request, from).await?
    {
        if let Some(active) = &snapshot.active {
            let active_batches = if active.points.is_empty() {
                vec![RecordBatch::new_empty(series::schema())]
            } else {
                active.points.clone()
            };
            let active_frame = SessionContext::new_with_config_rt(
                SessionConfig::new()
                    .with_target_partitions(2)
                    .with_batch_size(8192),
                core.query_runtime.clone(),
            )
            .read_table(Arc::new(MemTable::try_new(
                series::schema(),
                vec![active_batches],
            )?))?;
            collect_alert_baselines(
                active_frame
                    .filter(alert_scope(&request))?
                    .filter(col("timestamp").lt(timestamp(from)))?,
                &target_identities,
                &mut baselines,
            )
            .await?;
        }

        let mut files: Vec<_> = snapshot
            .series_files
            .iter()
            .filter(|file| file.meta.min_timestamp < from)
            .cloned()
            .collect();
        files.sort_by_key(|file| std::cmp::Reverse(file.meta.max_timestamp));
        for file in files {
            let baselines_complete = target_identities
                .iter()
                .all(|identity| baselines.contains_key(identity));
            let baseline_safe = baselines_complete
                && baselines
                    .values()
                    .all(|(point_at, _)| file.meta.max_timestamp < *point_at);
            if baseline_safe {
                break;
            }
            let context = session(&core);
            let file_frame = context
                .read_parquet(
                    vec![file.path.to_string_lossy().into_owned()],
                    ParquetReadOptions::default().schema(&series::schema()),
                )
                .await?
                .filter(alert_scope(&request))?
                .filter(col("timestamp").lt(timestamp(from)))?;
            collect_alert_baselines(file_frame, &target_identities, &mut baselines).await?;
        }
    }

    let windows = aggregate_alert_windows(
        window_frame,
        &request.windows_seconds,
        as_of,
        &baselines,
        request.need_increase,
    )
    .await?;
    Ok(AlertQueryResponse {
        as_of: format_timestamp(as_of),
        last_point_at: last_point.map(format_timestamp),
        windows,
    })
}

fn validate_alert_request(request: &AlertQueryRequest) -> Result<(), QueryError> {
    if request.server.is_empty()
        || request.server.len() > 128
        || request.service.is_empty()
        || request.service.len() > 128
        || request.name.is_empty()
        || request.name.len() > 128
        || request
            .environment
            .as_ref()
            .is_some_and(|value| value.len() > 128)
        || request.windows_seconds.is_empty()
        || request.windows_seconds.len() > 20
        || request
            .windows_seconds
            .iter()
            .any(|seconds| !(60..=604_800).contains(seconds))
    {
        return Err(QueryError::Invalid("invalid alert series request".into()));
    }
    Ok(())
}

fn alert_scope(request: &AlertQueryRequest) -> Expr {
    let mut predicate = col("server")
        .eq(lit(request.server.as_str()))
        .and(col("service").eq(lit(request.service.as_str())))
        .and(col("name").eq(lit(request.name.as_str())))
        .and(col("origin").eq(lit(request.origin.as_str())));
    if let Some(environment) = &request.environment {
        predicate = predicate.and(col("environment").eq(lit(environment.as_str())));
    }
    predicate
}

async fn collect_alert_identities(
    frame: DataFrame,
) -> Result<(BTreeSet<Identity>, Option<i64>), QueryError> {
    let selected = dedup(frame)?.select([
        col("timestamp"),
        col("environment"),
        col("server"),
        col("backend"),
        col("service_instance"),
        col("origin"),
        col("rule_id"),
        col("labels"),
    ])?;
    let mut stream = selected.execute_stream().await?;
    let mut identities = BTreeSet::new();
    let mut last_point = None;
    while let Some(batch) = stream.try_next().await? {
        let timestamps = column::<TimestampNanosecondArray>(&batch, "timestamp")?;
        let columns = identity_columns(&batch)?;
        for row in 0..batch.num_rows() {
            identities.insert(identity_at(&columns, row));
            last_point = Some(last_point.map_or(timestamps.value(row), |old: i64| {
                old.max(timestamps.value(row))
            }));
        }
    }
    Ok((identities, last_point))
}

async fn latest_alert_timestamp(frame: DataFrame) -> Result<Option<i64>, QueryError> {
    let mut stream = frame
        .aggregate(vec![], vec![max(col("timestamp")).alias("timestamp")])?
        .execute_stream()
        .await?;
    let Some(batch) = stream.try_next().await? else {
        return Ok(None);
    };
    let timestamps = column::<TimestampNanosecondArray>(&batch, "timestamp")?;
    Ok((batch.num_rows() > 0 && !timestamps.is_null(0)).then(|| timestamps.value(0)))
}

async fn metric_scope_has_points_before(
    core: &Core,
    snapshot: &Snapshot,
    request: &AlertQueryRequest,
    from: i64,
) -> Result<bool, QueryError> {
    let active_has_prior = snapshot.active.as_ref().is_some_and(|active| {
        active.scoped.names.iter().any(
            |((server, service, name, origin, environment), (first_seen, _))| {
                server == &request.server
                    && service == &request.service
                    && name == &request.name
                    && origin == request.origin.as_str()
                    && request
                        .environment
                        .as_ref()
                        .is_none_or(|selected| selected == environment)
                    && *first_seen < from
            },
        )
    });
    if active_has_prior {
        return Ok(true);
    }

    let journal = core.journal.clone();
    let server = request.server.clone();
    let service = request.service.clone();
    let name = request.name.clone();
    let origin = request.origin.as_str().to_owned();
    let environment = request.environment.clone();
    tokio::task::spawn_blocking(move || {
        let catalog = journal
            .catalog
            .lock()
            .map_err(|_| anyhow::anyhow!("catalog lock poisoned"))?;
        let exists = catalog.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM scoped_series
             WHERE server=?1 AND service=?2 AND name=?3 AND origin=?4
               AND (?5 IS NULL OR environment=?5) AND first_seen<?6)",
            rusqlite::params![server, service, name, origin, environment, from],
            |row| row.get::<_, bool>(0),
        )?;
        anyhow::Result::<bool>::Ok(exists)
    })
    .await
    .map_err(|error| QueryError::Storage(error.into()))?
    .map_err(QueryError::Storage)
}

async fn collect_alert_baselines(
    frame: DataFrame,
    target_identities: &BTreeSet<Identity>,
    baselines: &mut BTreeMap<Identity, (i64, f64)>,
) -> Result<(), QueryError> {
    let selected = dedup(frame)?.select([
        col("timestamp"),
        col("environment"),
        col("server"),
        col("backend"),
        col("service_instance"),
        col("origin"),
        col("rule_id"),
        col("labels"),
        col("value"),
    ])?;
    let mut stream = selected.execute_stream().await?;
    while let Some(batch) = stream.try_next().await? {
        let timestamps = column::<TimestampNanosecondArray>(&batch, "timestamp")?;
        let values = column::<Float64Array>(&batch, "value")?;
        let columns = identity_columns(&batch)?;
        for row in 0..batch.num_rows() {
            let timestamp = timestamps.value(row);
            let identity = identity_at(&columns, row);
            if target_identities.contains(&identity) {
                baselines
                    .entry(identity)
                    .and_modify(|baseline| {
                        if timestamp > baseline.0 {
                            *baseline = (timestamp, values.value(row));
                        } else if timestamp == baseline.0 {
                            baseline.1 = baseline.1.max(values.value(row));
                        }
                    })
                    .or_insert((timestamp, values.value(row)));
            }
        }
    }
    Ok(())
}

type IdentityColumns<'a> = (
    &'a StringArray,
    &'a StringArray,
    &'a StringArray,
    &'a StringArray,
    &'a StringArray,
    &'a StringArray,
    &'a StringArray,
);

fn identity_columns(batch: &RecordBatch) -> Result<IdentityColumns<'_>> {
    Ok((
        column(batch, "environment")?,
        column(batch, "server")?,
        column(batch, "backend")?,
        column(batch, "service_instance")?,
        column(batch, "origin")?,
        column(batch, "rule_id")?,
        column(batch, "labels")?,
    ))
}

fn identity_at(columns: &IdentityColumns<'_>, row: usize) -> Identity {
    (
        text(columns.0, row),
        columns.1.value(row).to_owned(),
        columns.2.value(row).to_owned(),
        text(columns.3, row),
        columns.4.value(row).to_owned(),
        text(columns.5, row),
        columns.6.value(row).to_owned(),
    )
}

#[derive(Default)]
struct AlertWindowAccumulator {
    count: u64,
    sum: f64,
    min: Option<f64>,
    max: Option<f64>,
    increase: f64,
    has_increase: bool,
}

async fn aggregate_alert_windows(
    frame: DataFrame,
    windows: &[u64],
    end: i64,
    baselines: &BTreeMap<Identity, (i64, f64)>,
    need_increase: bool,
) -> Result<Vec<AlertWindow>, QueryError> {
    let seconds: BTreeSet<_> = windows.iter().copied().collect();
    let starts: BTreeMap<_, _> = seconds
        .iter()
        .map(|seconds| {
            (
                *seconds,
                end.saturating_sub((*seconds as i64).saturating_mul(1_000_000_000)),
            )
        })
        .collect();
    let mut totals: BTreeMap<u64, AlertWindowAccumulator> = seconds
        .iter()
        .map(|seconds| (*seconds, AlertWindowAccumulator::default()))
        .collect();
    let mut selected = dedup(frame)?.select([
        col("timestamp"),
        col("environment"),
        col("server"),
        col("backend"),
        col("service_instance"),
        col("origin"),
        col("rule_id"),
        col("labels"),
        col("event_id"),
        col("value"),
    ])?;
    if need_increase {
        selected = selected.sort(vec![
            col("environment").sort(true, false),
            col("server").sort(true, false),
            col("backend").sort(true, false),
            col("service_instance").sort(true, false),
            col("origin").sort(true, false),
            col("rule_id").sort(true, false),
            col("labels").sort(true, false),
            col("timestamp").sort(true, false),
            col("event_id").sort(true, false),
        ])?;
    }
    let mut stream = selected.execute_stream().await?;
    let mut current_identity: Option<Identity> = None;
    let mut previous = BTreeMap::<u64, Option<f64>>::new();
    while let Some(batch) = stream.try_next().await? {
        let timestamps = column::<TimestampNanosecondArray>(&batch, "timestamp")?;
        let values = column::<Float64Array>(&batch, "value")?;
        let columns = need_increase
            .then(|| identity_columns(&batch))
            .transpose()?;
        for row in 0..batch.num_rows() {
            let timestamp = timestamps.value(row);
            let value = values.value(row);
            if let Some(columns) = &columns {
                let identity = identity_at(columns, row);
                if current_identity.as_ref() != Some(&identity) {
                    previous = seconds
                        .iter()
                        .map(|seconds| {
                            (*seconds, baselines.get(&identity).map(|(_, value)| *value))
                        })
                        .collect();
                    current_identity = Some(identity);
                }
            }
            for seconds in &seconds {
                if timestamp < starts[seconds] {
                    if need_increase {
                        *previous.get_mut(seconds).expect("all windows initialized") = Some(value);
                    }
                    continue;
                }
                let total = totals.get_mut(seconds).expect("all windows initialized");
                total.count += 1;
                total.sum += value;
                total.min = Some(total.min.map_or(value, |old| old.min(value)));
                total.max = Some(total.max.map_or(value, |old| old.max(value)));
                if need_increase {
                    let before = previous.get_mut(seconds).expect("all windows initialized");
                    if let Some(old) = *before {
                        total.increase += if value >= old { value - old } else { value };
                        total.has_increase = true;
                    }
                    *before = Some(value);
                }
            }
        }
    }
    windows
        .iter()
        .map(|seconds| {
            let total = totals
                .get(seconds)
                .expect("all requested windows initialized");
            if !total.sum.is_finite() || !total.increase.is_finite() {
                return Err(QueryError::Invalid("alert aggregate is not finite".into()));
            }
            Ok(AlertWindow {
                seconds: *seconds,
                count: total.count,
                sum: total.sum,
                avg: (total.count > 0).then(|| total.sum / total.count as f64),
                min: total.min,
                max: total.max,
                increase: (need_increase && total.has_increase).then_some(total.increase),
            })
        })
        .collect()
}

#[cfg(test)]
fn aggregate_alert_window(
    seconds: u64,
    start: i64,
    end: i64,
    points: &[AlertPoint],
    baselines: &BTreeMap<Identity, (i64, f64)>,
) -> AlertWindow {
    let selected: Vec<_> = points
        .iter()
        .filter(|point| point.timestamp >= start && point.timestamp < end)
        .collect();
    let sum: f64 = selected.iter().map(|point| point.value).sum();
    let count = selected.len() as u64;
    let min = selected.iter().map(|point| point.value).reduce(f64::min);
    let max = selected.iter().map(|point| point.value).reduce(f64::max);
    let mut series_points = BTreeMap::<Identity, BTreeMap<i64, f64>>::new();
    for point in points.iter().filter(|point| point.timestamp < end) {
        series_points
            .entry(point.identity.clone())
            .or_default()
            .entry(point.timestamp)
            .and_modify(|value| *value = value.max(point.value))
            .or_insert(point.value);
    }
    let mut increase = 0.0;
    let mut has_increase = false;
    for (identity, values) in &series_points {
        let old = baselines
            .get(identity)
            .filter(|(timestamp, _)| *timestamp < start)
            .copied();
        let recent = values
            .range(..start)
            .next_back()
            .map(|(timestamp, value)| (*timestamp, *value));
        let mut previous = match (old, recent) {
            (Some(old), Some(recent)) if recent.0 > old.0 => Some(recent.1),
            (Some(old), _) => Some(old.1),
            (None, Some(recent)) => Some(recent.1),
            (None, None) => None,
        };
        for (_, current) in values.range(start..end) {
            if let Some(before) = previous {
                has_increase = true;
                increase += if *current >= before {
                    *current - before
                } else {
                    *current
                };
            }
            previous = Some(*current);
        }
    }
    let increase = (count > 0 && has_increase).then_some(increase);
    AlertWindow {
        seconds,
        count,
        sum,
        avg: (count > 0).then(|| sum / count as f64),
        min,
        max,
        increase,
    }
}

#[cfg(test)]
mod alert_query_tests {
    use super::*;

    fn point(timestamp: i64, instance: Option<&str>, value: f64) -> AlertPoint {
        AlertPoint {
            timestamp,
            identity: (
                Some("production".into()),
                "eu-prod-01".into(),
                "barkcloud".into(),
                instance.map(Into::into),
                "counter".into(),
                None,
                String::new(),
            ),
            value,
        }
    }

    #[test]
    fn alert_windows_are_half_open_and_each_increase_uses_the_last_prior_value() {
        let identity = point(800, Some("one"), 1.0).identity;
        let points = vec![
            point(900, Some("one"), 3.0),
            point(950, Some("one"), 4.0),
            point(999, Some("one"), 5.0),
            point(1000, Some("one"), 99.0),
            point(970, Some("new"), 10.0),
            point(980, Some("new"), 13.0),
        ];
        let baselines = BTreeMap::from([(identity, (800, 1.0))]);

        let wide = aggregate_alert_window(100, 900, 1000, &points, &baselines);
        assert_eq!(wide.count, 5);
        assert_eq!(wide.sum, 35.0);
        assert_eq!(wide.min, Some(3.0));
        assert_eq!(wide.max, Some(13.0));
        // one: 2 + 1 + 1; new series: its first sample establishes a baseline, then +3.
        assert_eq!(wide.increase, Some(7.0));

        let narrow = aggregate_alert_window(50, 950, 1000, &points, &baselines);
        assert_eq!(narrow.count, 4);
        assert_eq!(narrow.sum, 32.0);
        // The point at 950 is included; 1000 is excluded. The prior point at 900 is baseline.
        assert_eq!(narrow.increase, Some(5.0));
    }

    #[test]
    fn increase_without_a_baseline_counts_resets_from_the_current_value() {
        let points = vec![
            point(100, Some("one"), 20.0),
            point(200, Some("one"), 25.0),
            point(299, Some("one"), 3.0),
        ];
        let result = aggregate_alert_window(300, 0, 300, &points, &BTreeMap::new());
        // 20 establishes the baseline, then +5 and a reset to 3 contribute 8 in total.
        assert_eq!(result.increase, Some(8.0));

        let baseline = BTreeMap::from([(point(-50, Some("one"), 30.0).identity, (-50, 30.0))]);
        let result = aggregate_alert_window(300, 0, 300, &points, &baseline);
        // The first value after the reset is counted from zero; the following reset counts 3.
        assert_eq!(result.increase, Some(28.0));
    }

    #[test]
    fn increase_is_unknown_without_a_pair_and_zero_with_an_equal_pair() {
        let singleton = aggregate_alert_window(
            300,
            0,
            300,
            &[point(100, Some("one"), 7.0)],
            &BTreeMap::new(),
        );
        assert_eq!(singleton.increase, None);

        let identities_without_pairs = aggregate_alert_window(
            300,
            0,
            300,
            &[point(100, Some("one"), 7.0), point(200, Some("two"), 7.0)],
            &BTreeMap::new(),
        );
        assert_eq!(identities_without_pairs.increase, None);

        let equal_pair = aggregate_alert_window(
            300,
            0,
            300,
            &[point(100, Some("one"), 7.0), point(200, Some("one"), 7.0)],
            &BTreeMap::new(),
        );
        assert_eq!(equal_pair.increase, Some(0.0));
    }
}

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
    #[serde(default)]
    pub server: Option<String>,
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
    pub truncated: bool,
    pub truncated_count: usize,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
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
type CatalogRows = (
    Vec<SeriesName>,
    Vec<SeriesEnvironment>,
    Vec<Definition>,
    crate::scoped_metrics::Metadata,
);

/// Имена, окружения и определения из каталога; SQLite читается вне async-потока.
async fn catalog(core: Arc<Core>) -> Result<CatalogRows, QueryError> {
    tokio::task::spawn_blocking(move || -> Result<CatalogRows, QueryError> {
        let catalog = core.journal.catalog.lock().unwrap();
        Ok((
            catalog.series_names()?,
            catalog.series_environments()?,
            catalog.definitions()?,
            crate::scoped_metrics::Metadata::load(&catalog)?,
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
    let (names, catalog_environments, definitions, _) = catalog(core.clone()).await?;
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
                server: None,
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
                    server: None,
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
    let (_, _, definitions, mut metadata) = catalog(core.clone()).await?;
    if let Some(active) = &snapshot.active {
        metadata.merge(&active.scoped);
    }
    if let Some(server) = &request.server {
        crate::definitions::validate_service(server).map_err(QueryError::Invalid)?;
    }
    let candidates: Vec<_> = definitions
        .iter()
        .filter(|d| d.service == request.service && d.name == request.name)
        .collect();
    let mut servers: BTreeSet<String> = metadata
        .names
        .keys()
        .filter(|(_, service, name, origin, _)| {
            service == &request.service
                && name == &request.name
                && request.origin.is_none_or(|o| o.as_str() == origin)
        })
        .map(|(server, _, _, _, _)| server.clone())
        .collect();
    servers.extend(
        candidates
            .iter()
            .filter(|d| {
                request
                    .origin
                    .is_none_or(|origin| default_origin(d) == origin)
            })
            .map(|d| d.server.clone()),
    );
    if let Some(server) = &request.server {
        servers = BTreeSet::from([server.clone()]);
    }
    let settings_for = |server: &str| {
        if let Some(definition) = candidates.iter().find(|d| d.server == server) {
            (
                definition.kind,
                if definition.kind == Kind::Cumulative {
                    Aggregation::Sum
                } else {
                    definition.aggregation
                },
                definition.unit.clone(),
            )
        } else {
            let origins: Vec<_> = metadata
                .names
                .keys()
                .filter(|(s, service, name, _, _)| {
                    s == server && service == &request.service && name == &request.name
                })
                .map(|(_, _, _, origin, _)| origin.as_str())
                .collect();
            let gauge = request
                .origin
                .map(|o| o == Origin::Gauge)
                .unwrap_or(!origins.is_empty() && origins.iter().all(|o| *o == "gauge"));
            if gauge {
                (Kind::Gauge, Aggregation::Avg, String::new())
            } else {
                (Kind::Delta, Aggregation::Sum, String::new())
            }
        }
    };
    let settings: Vec<_> = servers.iter().map(|server| settings_for(server)).collect();
    if settings.windows(2).any(|pair| pair[0] != pair[1]) {
        return Err(QueryError::Invalid("server_required".into()));
    }
    let (kind, aggregation, unit) = settings
        .into_iter()
        .next()
        .unwrap_or_else(|| settings_for(""));
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
    let truncated_count = ranked.len().saturating_sub(MAX_SERIES);
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
        truncated: truncated_count > 0,
        truncated_count,
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
    if let Some(server) = &request.server {
        predicate = predicate.and(col("server").eq(lit(server.as_str())));
    }
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
