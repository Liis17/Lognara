//! Проблемы: error/fatal события по fingerprint. Числа — по сырым событиям обработанного
//! снимка, lifetime-метаданные — из каталога. Статусы хранит Dock и передаёт в запросе.
use crate::{
    analytics::{raw_frame, timestamp},
    issue_identity::{Exception, fingerprint, valid_id},
    model::{LogEvent, StoredEvent, format_timestamp, parse_timestamp},
    pattern_catalog::{KnownIssue, State, column, next_chunk, optional_string, string},
    pattern_identity::{Part, identify},
    patterns::{Row, Totals, ValueCount, Window, interval, remember, scan},
    query::{self, Filters, QueryError},
    storage::Core,
    wire::LogLevel,
};
use datafusion::{
    arrow::{
        array::{Array, ArrayRef, StringArray, TimestampNanosecondArray, UInt8Array},
        datatypes::DataType,
    },
    common::ScalarValue,
    logical_expr::{ColumnarValue, Expr, Volatility, create_udf},
    prelude::*,
};
use futures::TryStreamExt;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    sync::Arc,
    time::Instant,
};

const MAX_STATUSES: usize = 10_000;
const PROJECTION: &[&str] = &[
    "environment",
    "service",
    "action",
    "level",
    "message",
    "attributes",
    "timestamp",
    "server",
];

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    #[default]
    Open,
    New,
    Regressed,
    Resolved,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IssueRequest {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub filters: Filters,
    #[serde(default = "interval")]
    pub interval_seconds: u64,
    #[serde(default = "query::default_limit")]
    pub limit: usize,
    #[serde(default)]
    pub status: Status,
    /// Решённые и вернувшиеся fingerprint из состояния Dock.
    #[serde(default)]
    pub resolved: Vec<String>,
    #[serde(default)]
    pub regressed: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IssueDetailsRequest {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub filters: Filters,
    #[serde(default = "interval")]
    pub interval_seconds: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecurrenceRequest {
    pub after: u64,
    #[serde(default)]
    pub issues: Vec<Watch>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Watch {
    pub fingerprint: String,
    pub since: String,
}

#[derive(Debug, Serialize)]
pub struct IssueResponse {
    pub algorithm_version: u8,
    pub as_of: String,
    pub watermark: u64,
    pub core_watermark: u64,
    pub history_from: Option<String>,
    pub history_complete: bool,
    pub counts: Counts,
    /// Сумма событий периода по всем проблемам выбранного статуса.
    pub status_events: u64,
    pub issues: Vec<Issue>,
}

#[derive(Debug, Serialize)]
pub struct Counts {
    pub open: u64,
    pub new: Option<u64>,
    pub regressed: u64,
    pub resolved: u64,
}

#[derive(Debug, Serialize)]
pub struct Issue {
    pub fingerprint: String,
    pub environment: Option<String>,
    pub service: String,
    pub action: Option<String>,
    pub exception_type: Option<String>,
    pub frames: Vec<String>,
    pub pattern_id: String,
    pub template: String,
    pub parts: Vec<Part>,
    pub exact: bool,
    pub count: u64,
    pub servers: u64,
    pub is_new: Option<bool>,
    pub first_seen: String,
    pub last_seen: String,
    pub buckets: Vec<crate::analytics::Bucket>,
}

#[derive(Debug, Serialize)]
pub struct IssueDetails {
    pub as_of: String,
    pub issue: Issue,
    pub servers: Vec<ValueCount>,
    pub other_servers: u64,
    pub examples: Vec<LogEvent>,
}

#[derive(Debug, Serialize)]
pub struct RecurrenceResponse {
    pub watermark: u64,
    pub core_watermark: u64,
    pub recurrences: Vec<Recurrence>,
}

#[derive(Debug, Serialize)]
pub struct Recurrence {
    pub fingerprint: String,
    pub first_seen: String,
    pub count: u64,
}

/// fingerprint строки для точного фильтра в DataFusion-агрегатах; null ниже error.
pub fn id_expr() -> Expr {
    create_udf(
        "log_issue_fingerprint",
        vec![
            DataType::Utf8,
            DataType::Utf8,
            DataType::Utf8,
            DataType::UInt8,
            DataType::Utf8,
            DataType::Utf8,
        ],
        DataType::Utf8,
        Volatility::Immutable,
        Arc::new(|args| {
            let arrays = ColumnarValue::values_to_arrays(args)?;
            let invalid =
                || datafusion::common::DataFusionError::Internal("invalid issue columns".into());
            let strings = |index: usize| {
                arrays[index]
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .ok_or_else(invalid)
            };
            let (environment, service, action) = (strings(0)?, strings(1)?, strings(2)?);
            let (message, attributes) = (strings(4)?, strings(5)?);
            let level = arrays[3]
                .as_any()
                .downcast_ref::<UInt8Array>()
                .ok_or_else(invalid)?;
            fn optional(array: &StringArray, row: usize) -> Option<&str> {
                (!array.is_null(row)).then(|| array.value(row))
            }
            let ids: StringArray = (0..message.len())
                .map(|row| {
                    let level = LogLevel::from_code(level.value(row)).map_err(|error| {
                        datafusion::common::DataFusionError::External(error.into())
                    })?;
                    let environment = optional(environment, row);
                    let identity = identify(
                        environment,
                        service.value(row),
                        level.as_str(),
                        message.value(row),
                    );
                    Ok(fingerprint(
                        environment,
                        service.value(row),
                        optional(action, row),
                        level,
                        &identity,
                        &Exception::parse(attributes.value(row)),
                    )
                    .map(|issue| issue.id))
                })
                .collect::<datafusion::common::Result<_>>()?;
            Ok(ColumnarValue::Array(Arc::new(ids) as ArrayRef))
        }),
    )
    .call(
        [
            "environment",
            "service",
            "action",
            "level",
            "message",
            "attributes",
        ]
        .into_iter()
        .map(col)
        .collect(),
    )
}

/// Только environment/service/action: ими же задана область fingerprint в каталоге.
fn window(
    from: &str,
    to: &str,
    filters: &Filters,
    interval_seconds: u64,
) -> Result<Window, QueryError> {
    if filters
        .keys()
        .any(|field| !["environment", "service", "action"].contains(&field.as_str()))
    {
        return Err(QueryError::Invalid(
            "issue filters accept environment, service and action".into(),
        ));
    }
    let mut filters = filters.clone();
    filters.insert("level".into(), vec!["error".into(), "fatal".into()]);
    let mut window = Window::new(from, to, &filters, None, interval_seconds)?;
    // Прошлое окно проблемам не нужно.
    window.previous_from = window.filter.from;
    Ok(window)
}

fn row_fingerprint(row: &Row<'_>) -> Result<Option<String>, QueryError> {
    let level = LogLevel::from_code(column::<UInt8Array>(row.batch, "level")?.value(row.row))?;
    let exception = Exception::parse(string(row.batch, "attributes", row.row)?);
    Ok(fingerprint(
        optional_string(row.batch, "environment", row.row)?,
        string(row.batch, "service", row.row)?,
        optional_string(row.batch, "action", row.row)?,
        level,
        &row.identity,
        &exception,
    )
    .map(|issue| issue.id))
}

fn known<'a>(state: &'a State, id: &str) -> Result<&'a KnownIssue, QueryError> {
    state
        .issues
        .get(id)
        .map(|issue| &**issue)
        .ok_or_else(|| anyhow::anyhow!("processed issue is missing from catalog").into())
}

fn matches(filters: &Filters, issue: &KnownIssue) -> bool {
    let field = |name: &str, value: Option<&String>| {
        filters
            .get(name)
            .is_none_or(|values| value.is_some_and(|value| values.contains(value)))
    };
    field("environment", issue.environment.as_ref())
        && field("service", Some(&issue.service))
        && field("action", issue.action.as_ref())
}

#[derive(Default)]
struct IssueTotals {
    totals: Totals,
    servers: HashSet<String>,
}

fn issue(
    state: &State,
    window: &Window,
    id: &str,
    totals: Option<&IssueTotals>,
) -> Result<Issue, QueryError> {
    let known = known(state, id)?;
    let (from, to) = (window.filter.from, window.filter.to);
    let is_new = if known.first < from {
        Some(false)
    } else if state.complete {
        Some(known.first < to)
    } else {
        None
    };
    let empty = IssueTotals::default();
    let totals = totals.unwrap_or(&empty);
    Ok(Issue {
        fingerprint: id.into(),
        environment: known.environment.clone(),
        service: known.service.clone(),
        action: known.action.clone(),
        exception_type: known.exception_type.clone(),
        frames: known.frames.clone(),
        pattern_id: known.pattern_id.clone(),
        template: known.template.clone(),
        parts: known.parts.clone(),
        exact: known.exact,
        count: totals.totals.count,
        servers: totals.servers.len() as u64,
        is_new,
        first_seen: format_timestamp(known.first),
        last_seen: format_timestamp(known.last),
        buckets: window.buckets(&totals.totals.buckets),
    })
}

fn statuses(ids: &[String]) -> Result<BTreeSet<&String>, QueryError> {
    if ids.len() > MAX_STATUSES || !ids.iter().all(|id| valid_id(id)) {
        return Err(QueryError::Invalid(
            "resolved and regressed accept up to 10000 fingerprints".into(),
        ));
    }
    Ok(ids.iter().collect())
}

pub async fn search(core: &Core, request: IssueRequest) -> Result<IssueResponse, QueryError> {
    let deadline = Instant::now() + core.journal.config.query_timeout;
    let window = window(
        &request.from,
        &request.to,
        &request.filters,
        request.interval_seconds,
    )?;
    if !(1..=100).contains(&request.limit) {
        return Err(QueryError::Invalid("limit must be 1..100".into()));
    }
    let resolved = statuses(&request.resolved)?;
    let regressed = statuses(&request.regressed)?;
    let budget = core.journal.config.pattern_query_bytes;
    if request
        .limit
        .saturating_mul(window.bucket_count())
        .saturating_mul(64)
        > budget
    {
        return Err(QueryError::Invalid(
            "limit and buckets exceed the issue response budget".into(),
        ));
    }
    let state = core.patterns.view()?;
    let mut issues: HashMap<String, IssueTotals> = HashMap::new();
    let mut bytes = 0_usize;
    scan(core, &state, &window, Some(PROJECTION), deadline, |row| {
        let Some(id) = row_fingerprint(&row)? else {
            return Ok(());
        };
        if !issues.contains_key(&id) {
            bytes += 256;
        }
        let entry = issues.entry(id).or_default();
        if entry.totals.add(&window, row.timestamp) {
            bytes += 48;
        }
        let server = string(row.batch, "server", row.row)?;
        if !entry.servers.contains(server) {
            bytes += server.len() + 32;
            entry.servers.insert(server.to_owned());
        }
        if bytes > budget {
            return Err(QueryError::Busy);
        }
        Ok(())
    })
    .await?;
    let open: Vec<&String> = issues.keys().filter(|id| !resolved.contains(id)).collect();
    let (mut new, mut returning) = (vec![], vec![]);
    for id in &open {
        if known(&state, id)?.first >= window.filter.from {
            new.push(*id);
        }
        if regressed.contains(id) {
            returning.push(*id);
        }
    }
    let solved: Vec<&String> = resolved
        .iter()
        .copied()
        .filter(|id| {
            state
                .issues
                .get(*id)
                .is_some_and(|issue| matches(&window.filter.filters, issue))
        })
        .collect();
    let counts = Counts {
        open: open.len() as u64,
        new: state.complete.then_some(new.len() as u64),
        regressed: returning.len() as u64,
        resolved: solved.len() as u64,
    };
    let mut selected = match request.status {
        Status::Open => open,
        // Новизну нельзя доказать без полной истории.
        Status::New if state.complete => new,
        Status::New => vec![],
        Status::Regressed => returning,
        Status::Resolved => solved,
    };
    let count = |id: &String| issues.get(id).map_or(0, |issue| issue.totals.count);
    let status_events = selected.iter().map(|id| count(id)).sum();
    if request.status == Status::Resolved {
        selected.sort_by(|a, b| {
            let last = |id: &String| state.issues[id].last;
            last(b).cmp(&last(a)).then_with(|| a.cmp(b))
        });
    } else {
        selected.sort_by(|a, b| count(b).cmp(&count(a)).then_with(|| a.cmp(b)));
    }
    let top = selected
        .iter()
        .take(request.limit)
        .map(|id| issue(&state, &window, id, issues.get(*id)))
        .collect::<Result<_, _>>()?;
    Ok(IssueResponse {
        algorithm_version: crate::issue_identity::VERSION,
        as_of: format_timestamp(state.as_of),
        watermark: state.checkpoint,
        core_watermark: core.snapshot().watermark.max(state.checkpoint),
        history_from: state.history_from.map(format_timestamp),
        history_complete: state.complete,
        counts,
        status_events,
        issues: top,
    })
}

/// Известная каталогу проблема без событий в периоде отдаётся с нулевыми числами.
pub async fn details(
    core: &Core,
    fingerprint: &str,
    request: IssueDetailsRequest,
) -> Result<IssueDetails, QueryError> {
    let deadline = Instant::now() + core.journal.config.query_timeout;
    if !valid_id(fingerprint) {
        return Err(QueryError::Invalid("invalid fingerprint".into()));
    }
    let window = window(
        &request.from,
        &request.to,
        &request.filters,
        request.interval_seconds,
    )?;
    let state = core.patterns.view()?;
    if !state.issues.contains_key(fingerprint) {
        return Err(QueryError::NotFound);
    }
    let config = &core.journal.config;
    let mut totals = IssueTotals::default();
    let mut servers: HashMap<String, u64> = HashMap::new();
    let mut bytes = 0_usize;
    let mut examples: Vec<(i64, u64, StoredEvent)> = vec![];
    scan(core, &state, &window, None, deadline, |row| {
        if row_fingerprint(&row)?.as_deref() != Some(fingerprint) {
            return Ok(());
        }
        totals.totals.add(&window, row.timestamp);
        let server = string(row.batch, "server", row.row)?;
        if let Some(count) = servers.get_mut(server) {
            *count += 1;
        } else {
            bytes += server.len() + 64;
            if servers.len() >= config.pattern_variable_values || bytes > config.pattern_query_bytes
            {
                return Err(QueryError::Busy);
            }
            servers.insert(server.to_owned(), 1);
        }
        remember(&mut examples, &row)
    })
    .await?;
    totals.servers = servers.keys().cloned().collect();
    let mut sorted: Vec<_> = servers.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let top: Vec<_> = sorted
        .iter()
        .take(5)
        .map(|(value, count)| ValueCount {
            value: value.clone(),
            count: *count,
        })
        .collect();
    Ok(IssueDetails {
        as_of: format_timestamp(state.as_of),
        issue: issue(&state, &window, fingerprint, Some(&totals))?,
        other_servers: sorted.iter().skip(5).map(|(_, count)| count).sum(),
        servers: top,
        examples: examples
            .into_iter()
            .map(|(_, _, event)| LogEvent::from(event))
            .collect(),
    })
}

/// Случаи после `after` с timestamp не раньше `since`; один сегмент за вызов.
/// watermark меньше `after` означает, что raw-хранилище начато заново.
pub async fn recurrences(
    core: &Core,
    request: RecurrenceRequest,
) -> Result<RecurrenceResponse, QueryError> {
    let deadline = Instant::now() + core.journal.config.query_timeout;
    if request.issues.len() > MAX_STATUSES {
        return Err(QueryError::Invalid(
            "issues accept up to 10000 fingerprints".into(),
        ));
    }
    let mut watched = HashMap::new();
    for watch in &request.issues {
        let since = parse_timestamp(&watch.since)
            .map_err(|_| QueryError::Invalid("since must be RFC3339".into()))?;
        if !valid_id(&watch.fingerprint) || watched.insert(&watch.fingerprint, since).is_some() {
            return Err(QueryError::Invalid(
                "issues require distinct valid fingerprints".into(),
            ));
        }
    }
    let snapshot = core.snapshot();
    let empty = RecurrenceResponse {
        watermark: snapshot.watermark,
        core_watermark: snapshot.watermark,
        recurrences: vec![],
    };
    let Some(since) = watched.values().min().copied() else {
        return Ok(empty);
    };
    if request.after >= snapshot.watermark {
        return Ok(empty);
    }
    let chunk = next_chunk(&snapshot, request.after)
        .ok_or_else(|| anyhow::anyhow!("raw storage has no chunk after checkpoint"))?;
    let levels = [LogLevel::Error, LogLevel::Fatal]
        .map(|level| lit(ScalarValue::UInt8(Some(level.code()))))
        .to_vec();
    let mut stream = raw_frame(core, &chunk, None)
        .await?
        .filter(
            col("sequence")
                .gt(lit(request.after))
                .and(col("level").in_list(levels, false))
                .and(col("timestamp").gt_eq(timestamp(since))),
        )?
        .select_columns(&PROJECTION[..7])?
        .execute_stream()
        .await?;
    let mut found: HashMap<&String, (i64, u64)> = HashMap::new();
    while let Some(batch) = stream.try_next().await? {
        query::check_deadline(deadline)?;
        for row in 0..batch.num_rows() {
            let environment = optional_string(&batch, "environment", row)?;
            let service = string(&batch, "service", row)?;
            let level = LogLevel::from_code(column::<UInt8Array>(&batch, "level")?.value(row))?;
            let identity = identify(
                environment,
                service,
                level.as_str(),
                string(&batch, "message", row)?,
            );
            let Some(issue) = fingerprint(
                environment,
                service,
                optional_string(&batch, "action", row)?,
                level,
                &identity,
                &Exception::parse(string(&batch, "attributes", row)?),
            ) else {
                continue;
            };
            let timestamp = column::<TimestampNanosecondArray>(&batch, "timestamp")?.value(row);
            let Some((id, since)) = watched.get_key_value(&issue.id) else {
                continue;
            };
            if timestamp >= *since {
                let entry = found.entry(*id).or_insert((timestamp, 0));
                entry.0 = entry.0.min(timestamp);
                entry.1 += 1;
            }
        }
    }
    let mut recurrences: Vec<_> = found
        .into_iter()
        .map(|(id, (first, count))| Recurrence {
            fingerprint: id.clone(),
            first_seen: format_timestamp(first),
            count,
        })
        .collect();
    recurrences.sort_by(|a, b| a.fingerprint.cmp(&b.fingerprint));
    Ok(RecurrenceResponse {
        watermark: chunk.watermark,
        core_watermark: snapshot.watermark,
        recurrences,
    })
}
