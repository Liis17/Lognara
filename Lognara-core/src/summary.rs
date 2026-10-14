//! Сводка и первая страница над одним удерживаемым снимком, без материализации всех событий.
use crate::{
    analytics::{bucket_bounds, count_value, frame, timestamp},
    model::format_timestamp,
    query::{
        self, Filters, PreparedFilter, PreparedText, QueryError, SearchRequest, SearchResponse,
        TextQuery,
    },
    storage::Core,
    wire::LogLevel,
};
use anyhow::{Context, Result};
use datafusion::{
    arrow::{
        array::{ArrayRef, BooleanArray, Float64Array, RecordBatch, StringArray},
        datatypes::{DataType, IntervalMonthDayNano},
    },
    common::ScalarValue,
    dataframe::DataFrame,
    functions::datetime::expr_fn::date_bin,
    functions_aggregate::{
        approx_percentile_cont::approx_percentile_cont,
        expr_fn::{count, max},
    },
    logical_expr::{ColumnarValue, Expr, Volatility, create_udf},
    prelude::*,
};
use futures::TryStreamExt;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Arc, time::Instant};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SummaryRequest {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub filters: Filters,
    pub text: Option<TextQuery>,
    #[serde(default = "minute")]
    pub interval_seconds: u64,
    pub search_limit: Option<usize>,
}
fn minute() -> u64 {
    60
}

#[derive(Debug, Serialize)]
pub struct SummaryResponse {
    pub total: u64,
    pub buckets: Vec<Bucket>,
    pub levels: BTreeMap<String, u64>,
    pub error_services: Vec<ServiceCount>,
    pub duration: Duration,
    pub actions: Vec<Action>,
    pub page: Option<SearchResponse>,
    pub as_of: String,
}
#[derive(Debug, Serialize)]
pub struct Bucket {
    pub timestamp: String,
    pub count: u64,
    pub levels: BTreeMap<String, u64>,
}
#[derive(Debug, Serialize)]
pub struct ServiceCount {
    pub service: Option<String>,
    pub count: u64,
}
#[derive(Debug, Serialize)]
pub struct Duration {
    pub p95_ms: Option<f64>,
    pub measured: u64,
    pub coverage: f64,
    pub method: &'static str,
}
#[derive(Debug, Serialize)]
pub struct Action {
    pub action: Option<String>,
    pub count: u64,
    pub duration: Duration,
}

fn empty_levels() -> BTreeMap<String, u64> {
    [
        "trace", "debug", "info", "warn", "error", "fatal", "unknown",
    ]
    .into_iter()
    .map(|level| (level.into(), 0))
    .collect()
}

/// Сам владеет analytics permit, чтобы blocking-поиск сохранил его после отмены HTTP.
pub async fn summary(
    core: Arc<Core>,
    request: SummaryRequest,
) -> Result<SummaryResponse, QueryError> {
    let permit = Arc::new(
        core.analytics_slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| QueryError::Busy)?,
    );
    let deadline = Instant::now() + core.journal.config.query_timeout;
    let filter = PreparedFilter::new(&request.from, &request.to, &request.filters)?;
    let text = request.text.as_ref().map(PreparedText::new).transpose()?;
    let (first, last, step) = bucket_bounds(&filter, request.interval_seconds)?;
    if request
        .search_limit
        .is_some_and(|limit| !(1..=1000).contains(&limit))
    {
        return Err(QueryError::Invalid("search_limit must be 1..1000".into()));
    }
    let snapshot = core.snapshot();
    let mut frame = frame(&core, &snapshot, &filter).await?;
    if let Some(text) = text {
        let matches = create_udf(
            "log_message_matches",
            vec![DataType::Utf8],
            DataType::Boolean,
            Volatility::Immutable,
            Arc::new(move |args| {
                let arrays = ColumnarValue::values_to_arrays(args)?;
                let strings = arrays[0]
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .ok_or_else(|| {
                        datafusion::common::DataFusionError::Internal(
                            "invalid message array".into(),
                        )
                    })?;
                let values = BooleanArray::from_iter(
                    strings
                        .iter()
                        .map(|value| value.map(|value| text.matches(value))),
                );
                Ok(ColumnarValue::Array(Arc::new(values)))
            }),
        );
        frame = frame
            .filter(matches.call(vec![col("message")]))
            .map_err(anyhow::Error::from)?;
    }
    let duration = duration_udf().call(vec![col("attributes")]);
    // Линейный масштаб не меняет квантиль и предотвращает overflow суммы centroid.
    // Обычные длительности остаются без пересчёта; предел учитывает u64 число строк.
    let mut maximum = 0.0_f64;
    for batch in collect(
        frame
            .clone()
            .aggregate(vec![], vec![max(duration.clone()).alias("maximum")])?,
        deadline,
    )
    .await?
    {
        if batch.num_rows() > 0 {
            let ScalarValue::Float64(value) = ScalarValue::try_from_array(batch.column(0), 0)?
            else {
                return Err(anyhow::anyhow!("invalid duration maximum").into());
            };
            maximum = value.unwrap_or(0.0);
        }
    }
    let scale = (maximum / 1.0e100).max(1.0);
    let bucket = date_bin(
        lit(ScalarValue::IntervalMonthDayNano(Some(
            IntervalMonthDayNano::new(0, 0, step as i64),
        ))),
        col("timestamp"),
        timestamp(0),
    )
    .alias("bucket");
    let mut counts = BTreeMap::<i64, BTreeMap<String, u64>>::new();
    let mut levels = empty_levels();
    for batch in collect(
        frame.clone().aggregate(
            vec![bucket, col("level")],
            vec![count(lit(1u8)).alias("count")],
        )?,
        deadline,
    )
    .await?
    {
        for row in 0..batch.num_rows() {
            let ScalarValue::TimestampNanosecond(Some(bucket), _) =
                ScalarValue::try_from_array(batch.column(0), row)?
            else {
                return Err(anyhow::anyhow!("invalid summary bucket").into());
            };
            let ScalarValue::UInt8(Some(level)) =
                ScalarValue::try_from_array(batch.column(1), row)?
            else {
                return Err(anyhow::anyhow!("invalid summary level").into());
            };
            let level = LogLevel::from_code(level)?.as_str().to_owned();
            let count = count_value(&batch, 2, row)?;
            *levels.get_mut(&level).context("unknown level")? += count;
            counts.entry(bucket).or_default().insert(level, count);
        }
    }
    let total = levels.values().sum();
    let buckets = (first..=last)
        .step_by(step as usize)
        .map(|bucket| {
            let mut levels = empty_levels();
            levels.extend(counts.remove(&(bucket as i64)).unwrap_or_default());
            Bucket {
                timestamp: format_timestamp(bucket as i64),
                count: levels.values().sum(),
                levels,
            }
        })
        .collect();
    let mut global_duration = Duration {
        p95_ms: None,
        measured: 0,
        coverage: 0.0,
        method: "tdigest",
    };
    for batch in collect(
        frame
            .clone()
            .aggregate(vec![], duration_aggregates(&duration, scale))?,
        deadline,
    )
    .await?
    {
        if batch.num_rows() > 0 {
            global_duration = read_duration(&batch, 0, 0, total, scale, maximum)?;
        }
    }
    let service_frame = frame
        .clone()
        .filter(col("level").eq(lit(ScalarValue::UInt8(Some(LogLevel::Error.code())))))?
        .aggregate(vec![col("service")], vec![count(lit(1u8)).alias("count")])?
        .sort(vec![
            col("count").sort(false, false),
            col("service").sort(true, false),
        ])?
        .limit(0, Some(5))?;
    let mut error_services = vec![];
    for batch in collect(service_frame, deadline).await? {
        for row in 0..batch.num_rows() {
            error_services.push(ServiceCount {
                service: string_value(&batch, 0, row)?,
                count: count_value(&batch, 1, row)?,
            });
        }
    }
    let mut aggregates = vec![count(lit(1u8)).alias("count")];
    aggregates.extend(duration_aggregates(&duration, scale));
    let action_frame = frame
        .aggregate(vec![col("action")], aggregates)?
        .sort(vec![
            col("count").sort(false, false),
            col("action").sort(true, false),
        ])?
        .limit(0, Some(20))?;
    let mut actions = vec![];
    for batch in collect(action_frame, deadline).await? {
        for row in 0..batch.num_rows() {
            let count = count_value(&batch, 1, row)?;
            actions.push(Action {
                action: string_value(&batch, 0, row)?,
                count,
                duration: read_duration(&batch, 2, row, count, scale, maximum)?,
            });
        }
    }
    query::check_deadline(deadline)?;
    let page = if let Some(limit) = request.search_limit {
        let worker = core.clone();
        let view = snapshot.clone();
        let task = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            query::search_snapshot(
                &worker,
                &view,
                SearchRequest {
                    from: request.from,
                    to: request.to,
                    filters: request.filters,
                    text: request.text,
                    limit,
                    cursor: None,
                },
                false,
                deadline,
            )
        });
        Some(task.await.map_err(anyhow::Error::from)??)
    } else {
        None
    };
    Ok(SummaryResponse {
        total,
        buckets,
        levels,
        error_services,
        duration: global_duration,
        actions,
        page,
        as_of: format_timestamp(snapshot.published_at),
    })
}

fn duration_aggregates(duration: &Expr, scale: f64) -> Vec<Expr> {
    vec![
        count(duration.clone()).alias("measured"),
        approx_percentile_cont(
            (duration.clone() / lit(scale)).sort(true, false),
            lit(0.95f64),
            None,
        )
        .alias("p95_ms"),
    ]
}
fn read_duration(
    batch: &RecordBatch,
    offset: usize,
    row: usize,
    total: u64,
    scale: f64,
    maximum: f64,
) -> Result<Duration> {
    let measured = count_value(batch, offset, row)?;
    let p95_ms = match ScalarValue::try_from_array(batch.column(offset + 1), row)? {
        ScalarValue::Float64(value) => value,
        _ => anyhow::bail!("invalid percentile type"),
    };
    anyhow::ensure!(
        p95_ms.is_none_or(|value| value.is_finite() && value >= 0.0),
        "invalid percentile value"
    );
    let p95_ms = p95_ms.map(|value| (value * scale).min(maximum));
    Ok(Duration {
        p95_ms,
        measured,
        coverage: if total == 0 {
            0.0
        } else {
            measured as f64 / total as f64
        },
        method: "tdigest",
    })
}
fn string_value(batch: &RecordBatch, column: usize, row: usize) -> Result<Option<String>> {
    match ScalarValue::try_from_array(batch.column(column), row)? {
        ScalarValue::Utf8(value) | ScalarValue::Utf8View(value) | ScalarValue::LargeUtf8(value) => {
            Ok(value)
        }
        _ => anyhow::bail!("invalid summary key"),
    }
}
async fn collect(frame: DataFrame, deadline: Instant) -> Result<Vec<RecordBatch>, QueryError> {
    query::check_deadline(deadline)?;
    let batches = frame
        .execute_stream()
        .await
        .map_err(anyhow::Error::from)?
        .try_collect()
        .await
        .map_err(anyhow::Error::from)?;
    query::check_deadline(deadline)?;
    Ok(batches)
}
fn duration_udf() -> datafusion::logical_expr::ScalarUDF {
    create_udf(
        "log_duration_ms",
        vec![DataType::Utf8],
        DataType::Float64,
        Volatility::Immutable,
        Arc::new(|args| {
            let arrays = ColumnarValue::values_to_arrays(args)?;
            let strings = arrays[0]
                .as_any()
                .downcast_ref::<StringArray>()
                .ok_or_else(|| {
                    datafusion::common::DataFusionError::Internal("invalid attributes array".into())
                })?;
            let values: Float64Array = strings
                .iter()
                .map(|text| {
                    let value: serde_json::Value = serde_json::from_str(text?).ok()?;
                    value
                        .get("duration_ms")?
                        .as_f64()
                        .filter(|value| value.is_finite() && *value >= 0.0)
                })
                .collect();
            Ok(ColumnarValue::Array(Arc::new(values) as ArrayRef))
        }),
    )
}
