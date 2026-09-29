//! Типизированная аналитика над одним снимком Parquet и открытых Arrow-батчей.
use crate::{
    columns,
    model::format_timestamp,
    query::{Filters, PreparedFilter, QueryError, level},
    storage::{Core, Snapshot},
    wire::LogLevel,
};
use anyhow::{Context, Result, bail};
use datafusion::{
    arrow::{array::RecordBatch, datatypes::IntervalMonthDayNano},
    common::ScalarValue,
    dataframe::DataFrame,
    datasource::MemTable,
    functions::datetime::expr_fn::date_bin,
    functions_aggregate::expr_fn::count,
    logical_expr::Expr,
    prelude::*,
};
use futures::TryStreamExt;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Arc};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistogramRequest {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub filters: Filters,
    #[serde(default = "minute")]
    pub interval_seconds: u64,
}
fn minute() -> u64 {
    60
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupRequest {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub filters: Filters,
    pub group_by: Vec<String>,
    #[serde(default = "crate::query::default_limit")]
    pub limit: usize,
}

#[derive(Debug, Serialize)]
pub struct HistogramResponse {
    pub buckets: Vec<Bucket>,
    pub as_of: String,
}
#[derive(Debug, Serialize)]
pub struct Bucket {
    pub timestamp: String,
    pub count: u64,
}
#[derive(Debug, Serialize)]
pub struct GroupResponse {
    pub groups: Vec<Group>,
    pub as_of: String,
}
#[derive(Debug, Serialize)]
pub struct Group {
    pub values: BTreeMap<String, Option<String>>,
    pub count: u64,
}

pub async fn histogram(
    core: &Core,
    request: HistogramRequest,
) -> Result<HistogramResponse, QueryError> {
    let filter = PreparedFilter::new(&request.from, &request.to, &request.filters)?;
    if !(1..=86400).contains(&request.interval_seconds) {
        return Err(QueryError::Invalid(
            "interval_seconds must be 1..86400".into(),
        ));
    }
    let step = request.interval_seconds as i128 * 1_000_000_000;
    let first = (filter.from as i128).div_euclid(step) * step;
    let last = (filter.to as i128 - 1).div_euclid(step) * step;
    if first < i64::MIN as i128 || (last - first) / step + 1 > 10_000 {
        return Err(QueryError::Invalid(
            "histogram exceeds 10000 buckets or timestamp range".into(),
        ));
    }
    let snapshot = core.snapshot();
    let frame = frame(core, &snapshot, &filter).await?;
    let bucket = date_bin(
        lit(ScalarValue::IntervalMonthDayNano(Some(
            IntervalMonthDayNano::new(0, 0, step as i64),
        ))),
        col("timestamp"),
        timestamp(0),
    )
    .alias("bucket");
    let frame = frame
        .aggregate(vec![bucket], vec![count(lit(1u8)).alias("count")])
        .map_err(anyhow::Error::from)?;
    let batches: Vec<RecordBatch> = frame
        .execute_stream()
        .await
        .map_err(anyhow::Error::from)?
        .try_collect()
        .await
        .map_err(anyhow::Error::from)?;
    let mut counts = BTreeMap::new();
    for batch in batches {
        for row in 0..batch.num_rows() {
            let value =
                ScalarValue::try_from_array(batch.column(0), row).map_err(anyhow::Error::from)?;
            let ScalarValue::TimestampNanosecond(Some(bucket), _) = value else {
                return Err(anyhow::anyhow!("invalid histogram bucket type").into());
            };
            counts.insert(bucket, count_value(&batch, 1, row)?);
        }
    }
    let buckets = (first..=last)
        .step_by(step as usize)
        .map(|bucket| Bucket {
            timestamp: format_timestamp(bucket as i64),
            count: counts.get(&(bucket as i64)).copied().unwrap_or(0),
        })
        .collect();
    Ok(HistogramResponse {
        buckets,
        as_of: format_timestamp(snapshot.published_at),
    })
}

pub async fn group_by(core: &Core, request: GroupRequest) -> Result<GroupResponse, QueryError> {
    let filter = PreparedFilter::new(&request.from, &request.to, &request.filters)?;
    let allowed = [
        "environment",
        "server",
        "backend",
        "service",
        "service_instance",
        "action",
        "level",
    ];
    if !(1..=2).contains(&request.group_by.len())
        || request
            .group_by
            .iter()
            .any(|field| !allowed.contains(&field.as_str()))
        || (request.group_by.len() == 2 && request.group_by[0] == request.group_by[1])
    {
        return Err(QueryError::Invalid(
            "group_by requires one or two distinct infrastructure fields, level or action".into(),
        ));
    }
    if !(1..=1000).contains(&request.limit) {
        return Err(QueryError::Invalid("limit must be 1..1000".into()));
    }
    let snapshot = core.snapshot();
    let frame = frame(core, &snapshot, &filter).await?;
    let mut order = vec![col("count").sort(false, false)];
    order.extend(
        request
            .group_by
            .iter()
            .map(|name| col(name).sort(true, false)),
    );
    let frame = frame
        .aggregate(
            request.group_by.iter().map(col).collect(),
            vec![count(lit(1u8)).alias("count")],
        )
        .and_then(|frame| frame.sort(order))
        .and_then(|frame| frame.limit(0, Some(request.limit)))
        .map_err(anyhow::Error::from)?;
    let batches: Vec<RecordBatch> = frame
        .execute_stream()
        .await
        .map_err(anyhow::Error::from)?
        .try_collect()
        .await
        .map_err(anyhow::Error::from)?;
    let mut groups = vec![];
    for batch in batches {
        for row in 0..batch.num_rows() {
            let mut values = BTreeMap::new();
            for (i, name) in request.group_by.iter().enumerate() {
                let scalar = ScalarValue::try_from_array(batch.column(i), row)
                    .map_err(anyhow::Error::from)?;
                let value = match scalar {
                    ScalarValue::Utf8(value)
                    | ScalarValue::LargeUtf8(value)
                    | ScalarValue::Utf8View(value) => value,
                    ScalarValue::UInt8(Some(code)) if name == "level" => {
                        Some(LogLevel::from_code(code)?.as_str().into())
                    }
                    _ => return Err(anyhow::anyhow!("invalid group column type").into()),
                };
                values.insert(name.clone(), value);
            }
            groups.push(Group {
                values,
                count: count_value(&batch, request.group_by.len(), row)?,
            });
        }
    }
    Ok(GroupResponse {
        groups,
        as_of: format_timestamp(snapshot.published_at),
    })
}

fn count_value(batch: &RecordBatch, column: usize, row: usize) -> Result<u64> {
    match ScalarValue::try_from_array(batch.column(column), row)? {
        ScalarValue::Int64(Some(value)) => Ok(value.try_into()?),
        ScalarValue::UInt64(Some(value)) => Ok(value),
        _ => bail!("invalid count type"),
    }
}

fn timestamp(value: i64) -> Expr {
    lit(ScalarValue::TimestampNanosecond(
        Some(value),
        Some("UTC".into()),
    ))
}

async fn frame(core: &Core, snapshot: &Snapshot, filter: &PreparedFilter) -> Result<DataFrame> {
    let config = SessionConfig::new()
        .with_target_partitions(2)
        .with_batch_size(8192)
        .with_collect_statistics(false)
        .with_parquet_pruning(true);
    let context = SessionContext::new_with_config_rt(config, core.query_runtime.clone());
    let paths: Vec<_> = snapshot
        .closed
        .iter()
        .filter(|segment| filter.overlaps(segment.meta.min_timestamp, segment.meta.max_timestamp))
        .map(|segment| {
            segment
                .path
                .join("logs.parquet")
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    let active = snapshot
        .active
        .as_ref()
        .filter(|active| filter.overlaps(active.meta.min_timestamp, active.meta.max_timestamp));
    let memory = active.map_or_else(
        || vec![RecordBatch::new_empty(columns::schema())],
        |active| active.batches.clone(),
    );
    let mut frame = context.read_table(Arc::new(MemTable::try_new(
        columns::schema(),
        vec![memory],
    )?))?;
    if !paths.is_empty() {
        // Явная Arrow-схема сохраняет nullability/типы при union с MemTable.
        let parquet = context
            .read_parquet(
                paths,
                ParquetReadOptions::default().schema(&columns::schema()),
            )
            .await?;
        frame = frame.union(parquet)?;
    }
    let mut predicate = col("timestamp")
        .gt_eq(timestamp(filter.from))
        .and(col("timestamp").lt(timestamp(filter.to)))
        .and(col("sequence").lt_eq(lit(snapshot.watermark)));
    for (field, values) in &filter.filters {
        let literals: Result<Vec<_>> = values
            .iter()
            .map(|value| {
                Ok(match field.as_str() {
                    "id" => lit(ScalarValue::FixedSizeBinary(
                        16,
                        Some(uuid::Uuid::parse_str(value)?.as_bytes().to_vec()),
                    )),
                    "trace_id" => lit(ScalarValue::FixedSizeBinary(16, Some(hex::decode(value)?))),
                    "span_id" | "parent_span_id" => {
                        lit(ScalarValue::FixedSizeBinary(8, Some(hex::decode(value)?)))
                    }
                    "level" => lit(ScalarValue::UInt8(Some(
                        level(value).context("validated level")?.code(),
                    ))),
                    _ => lit(value.clone()),
                })
            })
            .collect();
        predicate = predicate.and(col(field).in_list(literals?, false));
    }
    Ok(frame.filter(predicate)?)
}
