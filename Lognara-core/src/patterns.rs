//! Паттерны сообщений: числа по сырым событиям обработанного снимка, lifetime-метаданные из каталога.
use crate::{
    analytics::{Bucket, bucket_bounds, frame},
    columns,
    model::{LogEvent, StoredEvent, format_timestamp},
    pattern_catalog::{State, column, optional_string, string},
    pattern_identity::{Identity, Part, VariableType, identify, valid_id},
    query::{self, Filters, PreparedFilter, PreparedText, QueryError, TextQuery},
    storage::Core,
    wire::LogLevel,
};
use datafusion::{
    arrow::{
        array::{
            Array, ArrayRef, RecordBatch, StringArray, TimestampNanosecondArray, UInt8Array,
            UInt64Array,
        },
        datatypes::DataType,
    },
    logical_expr::{ColumnarValue, Expr, Volatility, create_udf},
    prelude::*,
};
use futures::TryStreamExt;
use serde::{Deserialize, Serialize};
use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::Arc,
    time::Instant,
};

pub(crate) fn interval() -> u64 {
    300
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatternRequest {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub filters: Filters,
    pub text: Option<TextQuery>,
    #[serde(default = "interval")]
    pub interval_seconds: u64,
    #[serde(default = "query::default_limit")]
    pub limit: usize,
    #[serde(default)]
    pub sort: Sort,
    #[serde(default)]
    pub exclude_patterns: Vec<String>,
}

#[derive(Debug, Default, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sort {
    #[default]
    Count,
    Growth,
    LastSeen,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DetailsRequest {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub filters: Filters,
    pub text: Option<TextQuery>,
    #[serde(default = "interval")]
    pub interval_seconds: u64,
}

#[derive(Debug, Serialize)]
pub struct PatternResponse {
    pub algorithm_version: u8,
    pub as_of: String,
    pub watermark: u64,
    pub core_watermark: u64,
    pub history_from: Option<String>,
    pub history_complete: bool,
    pub total_events: u64,
    pub total_patterns: u64,
    pub visible_patterns: u64,
    pub new_patterns: Option<u64>,
    pub patterns: Vec<Pattern>,
}

#[derive(Debug, Serialize)]
pub struct Pattern {
    pub pattern_id: String,
    pub environment: Option<String>,
    pub service: String,
    pub level: String,
    pub template: String,
    pub parts: Vec<Part>,
    pub exact: bool,
    pub count: u64,
    pub share: f64,
    pub previous_count: u64,
    pub growth_percent: Option<f64>,
    pub is_new: Option<bool>,
    pub first_seen: String,
    pub last_seen: String,
    pub buckets: Vec<Bucket>,
}

#[derive(Debug, Serialize)]
pub struct DetailsResponse {
    pub as_of: String,
    pub pattern: Pattern,
    pub variables: Vec<VariableStats>,
    pub examples: Vec<LogEvent>,
}

#[derive(Debug, Serialize)]
pub struct VariableStats {
    pub name: String,
    pub variable_type: VariableType,
    pub total: u64,
    pub values: Vec<ValueCount>,
    pub other_count: u64,
}

#[derive(Debug, Serialize)]
pub struct ValueCount {
    pub value: String,
    pub count: u64,
}

/// pattern_id строки для точного фильтра в DataFusion-агрегатах.
pub fn id_expr() -> Expr {
    create_udf(
        "log_pattern_id",
        vec![
            DataType::Utf8,
            DataType::Utf8,
            DataType::UInt8,
            DataType::Utf8,
        ],
        DataType::Utf8,
        Volatility::Immutable,
        Arc::new(|args| {
            let arrays = ColumnarValue::values_to_arrays(args)?;
            let invalid =
                || datafusion::common::DataFusionError::Internal("invalid pattern columns".into());
            let environment = arrays[0]
                .as_any()
                .downcast_ref::<StringArray>()
                .ok_or_else(invalid)?;
            let service = arrays[1]
                .as_any()
                .downcast_ref::<StringArray>()
                .ok_or_else(invalid)?;
            let level = arrays[2]
                .as_any()
                .downcast_ref::<UInt8Array>()
                .ok_or_else(invalid)?;
            let message = arrays[3]
                .as_any()
                .downcast_ref::<StringArray>()
                .ok_or_else(invalid)?;
            let ids: StringArray = (0..message.len())
                .map(|row| {
                    let level = LogLevel::from_code(level.value(row)).map_err(|error| {
                        datafusion::common::DataFusionError::External(error.into())
                    })?;
                    let environment = (!environment.is_null(row)).then(|| environment.value(row));
                    Ok(Some(
                        identify(
                            environment,
                            service.value(row),
                            level.as_str(),
                            message.value(row),
                        )
                        .id,
                    ))
                })
                .collect::<datafusion::common::Result<_>>()?;
            Ok(ColumnarValue::Array(Arc::new(ids) as ArrayRef))
        }),
    )
    .call(vec![
        col("environment"),
        col("service"),
        col("level"),
        col("message"),
    ])
}

pub(crate) struct Window {
    pub filter: PreparedFilter,
    pub previous_from: i64,
    text: Option<PreparedText>,
    first: i128,
    last: i128,
    step: i128,
}

impl Window {
    pub(crate) fn new(
        from: &str,
        to: &str,
        filters: &Filters,
        text: Option<&TextQuery>,
        interval_seconds: u64,
    ) -> Result<Self, QueryError> {
        let filter = PreparedFilter::new(from, to, filters)?;
        let (first, last, step) = bucket_bounds(&filter, interval_seconds)?;
        // Соседнее окно той же длины; до начала i64-диапазона оно усечено.
        let length = filter.to as i128 - filter.from as i128;
        let previous_from = (filter.from as i128 - length).max(i64::MIN as i128) as i64;
        Ok(Self {
            text: text.map(PreparedText::new).transpose()?,
            filter,
            previous_from,
            first,
            last,
            step,
        })
    }

    fn bucket(&self, timestamp: i64) -> u32 {
        ((timestamp as i128 - self.first).div_euclid(self.step)) as u32
    }

    pub(crate) fn buckets(&self, counts: &BTreeMap<u32, u64>) -> Vec<Bucket> {
        (self.first..=self.last)
            .step_by(self.step as usize)
            .enumerate()
            .map(|(index, bucket)| Bucket {
                timestamp: format_timestamp(bucket as i64),
                count: counts.get(&(index as u32)).copied().unwrap_or(0),
            })
            .collect()
    }

    pub(crate) fn bucket_count(&self) -> usize {
        ((self.last - self.first) / self.step + 1) as usize
    }
}

#[derive(Default)]
pub(crate) struct Totals {
    pub count: u64,
    pub previous: u64,
    pub last: i64,
    pub buckets: BTreeMap<u32, u64>,
}

impl Totals {
    /// Возвращает true, если появилась новая корзина.
    pub(crate) fn add(&mut self, window: &Window, timestamp: i64) -> bool {
        if timestamp < window.filter.from {
            self.previous += 1;
            return false;
        }
        self.last = if self.count == 0 {
            timestamp
        } else {
            self.last.max(timestamp)
        };
        self.count += 1;
        let counter = self.buckets.entry(window.bucket(timestamp)).or_default();
        *counter += 1;
        *counter == 1
    }
}

/// Строка обработанного снимка в границах текущего и прошлого окна.
pub(crate) struct Row<'a> {
    pub batch: &'a RecordBatch,
    pub row: usize,
    pub timestamp: i64,
    pub identity: Identity,
}

pub(crate) async fn scan(
    core: &Core,
    state: &State,
    window: &Window,
    projection: Option<&[&str]>,
    deadline: Instant,
    mut visit: impl FnMut(Row<'_>) -> Result<(), QueryError>,
) -> Result<(), QueryError> {
    let snapshot = core.snapshot();
    let mut wide = window.filter.clone();
    wide.from = window.previous_from;
    let mut frame = frame(core, &snapshot, &wide)
        .await?
        .filter(col("sequence").lt_eq(lit(state.checkpoint)))?;
    if let Some(projection) = projection {
        frame = frame.select_columns(projection)?;
    }
    let mut stream = frame.execute_stream().await?;
    while let Some(batch) = stream.try_next().await? {
        query::check_deadline(deadline)?;
        for row in 0..batch.num_rows() {
            let message = string(&batch, "message", row)?;
            if window
                .text
                .as_ref()
                .is_some_and(|text| !text.matches(message))
            {
                continue;
            }
            let level = LogLevel::from_code(column::<UInt8Array>(&batch, "level")?.value(row))?;
            let identity = identify(
                optional_string(&batch, "environment", row)?,
                string(&batch, "service", row)?,
                level.as_str(),
                message,
            );
            visit(Row {
                batch: &batch,
                row,
                timestamp: column::<TimestampNanosecondArray>(&batch, "timestamp")?.value(row),
                identity,
            })?;
        }
    }
    query::check_deadline(deadline)
}

fn pattern(
    state: &State,
    window: &Window,
    id: &str,
    totals: &Totals,
    total_events: u64,
) -> Result<Pattern, QueryError> {
    let known = state
        .known
        .get(id)
        .ok_or_else(|| anyhow::anyhow!("processed pattern is missing from catalog"))?;
    let (from, to) = (window.filter.from, window.filter.to);
    let is_new = if known.first < from {
        Some(false)
    } else if state.complete {
        Some(known.first < to)
    } else {
        None
    };
    Ok(Pattern {
        pattern_id: id.into(),
        environment: known.environment.clone(),
        service: known.service.clone(),
        level: known.level.clone(),
        template: known.template.clone(),
        parts: known.parts.clone(),
        exact: known.exact,
        count: totals.count,
        share: totals.count as f64 / total_events.max(1) as f64,
        previous_count: totals.previous,
        growth_percent: (totals.previous > 0).then(|| {
            (totals.count as f64 - totals.previous as f64) / totals.previous as f64 * 100.0
        }),
        is_new,
        first_seen: format_timestamp(known.first),
        last_seen: format_timestamp(totals.last),
        buckets: window.buckets(&totals.buckets),
    })
}

fn order(sort: Sort, a: &(&String, &Totals), b: &(&String, &Totals)) -> Ordering {
    let growth = |totals: &Totals| {
        if totals.previous == 0 {
            f64::INFINITY
        } else {
            (totals.count as f64 - totals.previous as f64) / totals.previous as f64
        }
    };
    let primary = match sort {
        Sort::Count => b.1.count.cmp(&a.1.count),
        Sort::Growth => growth(b.1).total_cmp(&growth(a.1)),
        Sort::LastSeen => b.1.last.cmp(&a.1.last),
    };
    primary.then_with(|| a.0.cmp(b.0))
}

pub async fn search(core: &Core, request: PatternRequest) -> Result<PatternResponse, QueryError> {
    let deadline = Instant::now() + core.journal.config.query_timeout;
    let window = Window::new(
        &request.from,
        &request.to,
        &request.filters,
        request.text.as_ref(),
        request.interval_seconds,
    )?;
    if !(1..=100).contains(&request.limit) {
        return Err(QueryError::Invalid("limit must be 1..100".into()));
    }
    if request.exclude_patterns.len() > 1000
        || !request.exclude_patterns.iter().all(|id| valid_id(id))
    {
        return Err(QueryError::Invalid(
            "exclude_patterns requires up to 1000 pattern ids".into(),
        ));
    }
    let budget = core.journal.config.pattern_query_bytes;
    if request
        .limit
        .saturating_mul(window.bucket_count())
        .saturating_mul(64)
        > budget
    {
        return Err(QueryError::Invalid(
            "limit and buckets exceed the pattern response budget".into(),
        ));
    }
    let state = core.patterns.view()?;
    let mut patterns: HashMap<String, Totals> = HashMap::new();
    let mut bytes = 0_usize;
    let mut total_events = 0_u64;
    scan(
        core,
        &state,
        &window,
        Some(&["environment", "service", "level", "message", "timestamp"]),
        deadline,
        |row| {
            total_events += u64::from(row.timestamp >= window.filter.from);
            if !patterns.contains_key(&row.identity.id) {
                bytes += 256;
            }
            let totals = patterns.entry(row.identity.id).or_default();
            if totals.add(&window, row.timestamp) {
                bytes += 48;
            }
            if bytes > budget {
                return Err(QueryError::Busy);
            }
            Ok(())
        },
    )
    .await?;
    let excluded: BTreeSet<_> = request.exclude_patterns.iter().collect();
    let current: Vec<_> = patterns
        .iter()
        .filter(|(_, totals)| totals.count > 0)
        .collect();
    let mut visible: Vec<_> = current
        .iter()
        .filter(|(id, _)| !excluded.contains(id))
        .copied()
        .collect();
    let new_patterns = if state.complete {
        let mut count = 0;
        for (id, _) in &visible {
            let known = state
                .known
                .get(*id)
                .ok_or_else(|| anyhow::anyhow!("processed pattern is missing from catalog"))?;
            count += u64::from(known.first >= window.filter.from);
        }
        Some(count)
    } else {
        None
    };
    visible.sort_by(|a, b| order(request.sort, a, b));
    let top = visible
        .iter()
        .take(request.limit)
        .map(|(id, totals)| pattern(&state, &window, id, totals, total_events))
        .collect::<Result<_, _>>()?;
    Ok(PatternResponse {
        algorithm_version: crate::pattern_identity::VERSION,
        as_of: format_timestamp(state.as_of),
        watermark: state.checkpoint,
        core_watermark: core.snapshot().watermark.max(state.checkpoint),
        history_from: state.history_from.map(format_timestamp),
        history_complete: state.complete,
        total_events,
        total_patterns: current.len() as u64,
        visible_patterns: visible.len() as u64,
        new_patterns,
        patterns: top,
    })
}

/// До трёх новейших примеров по (timestamp, sequence) DESC.
pub(crate) fn remember(
    examples: &mut Vec<(i64, u64, StoredEvent)>,
    row: &Row<'_>,
) -> Result<(), QueryError> {
    let sequence = column::<UInt64Array>(row.batch, "sequence")?.value(row.row);
    let key = (row.timestamp, sequence);
    if examples.len() < 3
        || examples
            .last()
            .is_some_and(|(timestamp, sequence, _)| key > (*timestamp, *sequence))
    {
        let event = columns::decode(&row.batch.slice(row.row, 1))?
            .pop()
            .ok_or_else(|| anyhow::anyhow!("missing example row"))?;
        examples.push((row.timestamp, sequence, event));
        examples.sort_by_key(|example| std::cmp::Reverse((example.0, example.1)));
        examples.truncate(3);
    }
    Ok(())
}

pub async fn details(
    core: &Core,
    pattern_id: &str,
    request: DetailsRequest,
) -> Result<DetailsResponse, QueryError> {
    let deadline = Instant::now() + core.journal.config.query_timeout;
    if !valid_id(pattern_id) {
        return Err(QueryError::Invalid("invalid pattern_id".into()));
    }
    let window = Window::new(
        &request.from,
        &request.to,
        &request.filters,
        request.text.as_ref(),
        request.interval_seconds,
    )?;
    let state = core.patterns.view()?;
    let config = &core.journal.config;
    let mut totals = Totals::default();
    let mut total_events = 0_u64;
    let mut names: Vec<(String, VariableType)> = vec![];
    let mut values: Vec<HashMap<String, u64>> = vec![];
    let (mut distinct, mut bytes) = (0_usize, 0_usize);
    let mut examples: Vec<(i64, u64, StoredEvent)> = vec![];
    scan(core, &state, &window, None, deadline, |row| {
        let current = row.timestamp >= window.filter.from;
        total_events += u64::from(current);
        if row.identity.id != pattern_id {
            return Ok(());
        }
        totals.add(&window, row.timestamp);
        if !current {
            return Ok(());
        }
        if names.is_empty() && !row.identity.variables.is_empty() {
            names = row
                .identity
                .variables
                .iter()
                .map(|variable| (variable.name.clone(), variable.variable_type))
                .collect();
            values = vec![HashMap::new(); names.len()];
        }
        for (index, variable) in row.identity.variables.iter().enumerate() {
            let counts = values
                .get_mut(index)
                .ok_or_else(|| anyhow::anyhow!("pattern variables disagree"))?;
            if let Some(count) = counts.get_mut(&variable.value) {
                *count += 1;
                continue;
            }
            distinct += 1;
            bytes += variable.value.len() + 64;
            if distinct > config.pattern_variable_values || bytes > config.pattern_query_bytes {
                return Err(QueryError::Busy);
            }
            counts.insert(variable.value.clone(), 1);
        }
        remember(&mut examples, &row)
    })
    .await?;
    if totals.count == 0 {
        return Err(QueryError::NotFound);
    }
    let variables = names
        .into_iter()
        .zip(values)
        .map(|((name, variable_type), counts)| {
            let mut sorted: Vec<_> = counts.into_iter().collect();
            sorted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            let total = sorted.iter().map(|(_, count)| count).sum::<u64>();
            let values: Vec<_> = sorted
                .into_iter()
                .take(5)
                .map(|(value, count)| ValueCount { value, count })
                .collect();
            VariableStats {
                name,
                variable_type,
                total,
                other_count: total - values.iter().map(|value| value.count).sum::<u64>(),
                values,
            }
        })
        .collect();
    Ok(DetailsResponse {
        as_of: format_timestamp(state.as_of),
        pattern: pattern(&state, &window, pattern_id, &totals, total_events)?,
        variables,
        examples: examples
            .into_iter()
            .map(|(_, _, event)| LogEvent::from(event))
            .collect(),
    })
}
