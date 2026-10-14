//! Проблемы: группировка ошибок и числа из core, общий для экземпляра статус в SQLite.
//! Наблюдатель переводит решённые проблемы в «возвращается» без открытого браузера.

use std::collections::BTreeSet;
use std::time::Duration;

use axum::Json;
use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::api::{ApiError, App, AppState};
use crate::core_client::CoreQueryError;
use crate::patterns::{Part, VariableValue, required_nullable};
use crate::query::{Bucket, Filters, LogEvent};
use crate::session::AuthUser;
use crate::store::{IssueAction, IssueState, Recurrence, StoreError};

/// Пауза наблюдателя после того, как он догнал core.
const WATCH_INTERVAL: Duration = Duration::from_secs(5);

pub fn valid_id(id: &str) -> bool {
    id.len() == 67
        && id.starts_with("f1_")
        && id.as_bytes()[3..]
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
}

fn interval() -> u64 {
    300
}
fn limit() -> usize {
    100
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    #[default]
    Open,
    New,
    Regressed,
    Resolved,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SearchRequest {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub filters: Filters,
    #[serde(default = "interval")]
    pub interval_seconds: u64,
    #[serde(default = "limit")]
    pub limit: usize,
    #[serde(default)]
    pub status: Status,
}

/// Запрос core: браузерный запрос и статусы из SQLite.
#[derive(Debug, Serialize)]
pub struct CoreSearchRequest {
    #[serde(flatten)]
    pub search: SearchRequest,
    pub resolved: Vec<String>,
    pub regressed: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DetailsRequest {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub filters: Filters,
    #[serde(default = "interval")]
    pub interval_seconds: u64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IssueResponse<T = Issue> {
    pub algorithm_version: u8,
    pub as_of: String,
    pub watermark: u64,
    pub core_watermark: u64,
    #[serde(deserialize_with = "required_nullable")]
    pub history_from: Option<String>,
    pub history_complete: bool,
    pub counts: Counts,
    pub status_events: u64,
    pub issues: Vec<T>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Counts {
    pub open: u64,
    #[serde(deserialize_with = "required_nullable")]
    pub new: Option<u64>,
    pub regressed: u64,
    pub resolved: u64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Issue {
    pub fingerprint: String,
    #[serde(deserialize_with = "required_nullable")]
    pub environment: Option<String>,
    pub service: String,
    #[serde(deserialize_with = "required_nullable")]
    pub action: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub exception_type: Option<String>,
    pub frames: Vec<String>,
    pub pattern_id: String,
    pub template: String,
    pub parts: Vec<Part>,
    pub exact: bool,
    pub count: u64,
    pub servers: u64,
    #[serde(deserialize_with = "required_nullable")]
    pub is_new: Option<bool>,
    pub first_seen: String,
    pub last_seen: String,
    pub buckets: Vec<Bucket>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IssueDetails {
    pub as_of: String,
    pub issue: Issue,
    pub servers: Vec<VariableValue>,
    pub other_servers: u64,
    pub examples: Vec<LogEvent>,
}

#[derive(Debug, Serialize)]
pub struct RecurrenceRequest {
    pub after: u64,
    pub issues: Vec<Watch>,
}

#[derive(Debug, Serialize)]
pub struct Watch {
    pub fingerprint: String,
    pub since: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecurrenceResponse {
    pub watermark: u64,
    pub core_watermark: u64,
    pub recurrences: Vec<CoreRecurrence>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreRecurrence {
    pub fingerprint: String,
    pub first_seen: String,
    pub count: u64,
}

impl Issue {
    fn valid(&self) -> bool {
        valid_id(&self.fingerprint)
            && crate::patterns::valid_id(&self.pattern_id)
            && self.template.len() <= 4096
            && self.frames.len() <= 5
            && self.servers <= self.count
            && self.buckets.len() <= 10_000
            && self
                .buckets
                .iter()
                .try_fold(0_u64, |total, bucket| total.checked_add(bucket.count))
                == Some(self.count)
    }
}

impl IssueResponse {
    /// Решённые отдаются только во вкладке решённых, остальные вкладки их не содержат.
    pub fn valid(&self, request: &CoreSearchRequest) -> bool {
        let resolved: BTreeSet<_> = request.resolved.iter().collect();
        let in_tab = |issue: &Issue| {
            resolved.contains(&issue.fingerprint) == (request.search.status == Status::Resolved)
        };
        self.algorithm_version == 1
            && self.watermark <= self.core_watermark
            && self.counts.regressed <= self.counts.open
            && self.counts.new.is_none_or(|new| new <= self.counts.open)
            && (self.history_complete || self.counts.new.is_none())
            && self.issues.len() <= request.search.limit
            && self
                .issues
                .iter()
                .all(|issue| issue.valid() && in_tab(issue))
            && self
                .issues
                .iter()
                .try_fold(0_u64, |total, issue| total.checked_add(issue.count))
                .is_some_and(|total| total <= self.status_events)
            && self
                .issues
                .iter()
                .map(|issue| &issue.fingerprint)
                .collect::<BTreeSet<_>>()
                .len()
                == self.issues.len()
    }
}

impl IssueDetails {
    pub fn valid(&self, fingerprint: &str) -> bool {
        self.issue.fingerprint == fingerprint
            && self.issue.valid()
            && self.servers.len() <= 5
            && self.examples.len() <= 3
            && self
                .servers
                .iter()
                .try_fold(self.other_servers, |total, server| {
                    total.checked_add(server.count)
                })
                == Some(self.issue.count)
    }
}

impl RecurrenceResponse {
    pub fn valid(&self, request: &RecurrenceRequest) -> bool {
        let watched: BTreeSet<_> = request
            .issues
            .iter()
            .map(|watch| &watch.fingerprint)
            .collect();
        self.watermark <= self.core_watermark
            && self
                .recurrences
                .iter()
                .all(|item| item.count > 0 && watched.contains(&item.fingerprint))
            && self
                .recurrences
                .iter()
                .map(|item| &item.fingerprint)
                .collect::<BTreeSet<_>>()
                .len()
                == self.recurrences.len()
    }
}

#[derive(Debug, Serialize)]
pub struct StateView {
    /// open, resolved или regressed; новизну сообщает is_new из core.
    pub status: String,
    pub resolved_at: Option<String>,
    pub resolved_by: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct IssueView {
    #[serde(flatten)]
    pub issue: Issue,
    pub state: StateView,
}

#[derive(Debug, Serialize)]
pub struct HistoryView {
    pub action: String,
    pub actor: Option<String>,
    pub at: String,
    pub event_at: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DetailsView {
    #[serde(flatten)]
    pub details: IssueDetails,
    pub state: StateView,
    pub history: Vec<HistoryView>,
}

fn format_ms(ms: i64) -> String {
    OffsetDateTime::from_unix_timestamp_nanos(ms as i128 * 1_000_000)
        .ok()
        .and_then(|time| time.format(&Rfc3339).ok())
        .unwrap_or_default()
}

fn state_view(state: Option<&IssueState>) -> StateView {
    StateView {
        status: state.map_or_else(|| "open".into(), |state| state.status.clone()),
        resolved_at: state.and_then(|state| state.resolved_at).map(format_ms),
        resolved_by: state.and_then(|state| state.resolved_by.clone()),
    }
}

fn core_error(error: CoreQueryError) -> ApiError {
    match error {
        CoreQueryError::NotFound => ApiError::IssueNotFound,
        CoreQueryError::NotReady => ApiError::IssuesNotReady,
        error => error.into(),
    }
}

fn fingerprint(path: Result<Path<String>, PathRejection>) -> Result<String, ApiError> {
    let Path(id) = path.map_err(|_| ApiError::BadRequest("invalid_request"))?;
    if !valid_id(&id) {
        return Err(ApiError::BadRequest("invalid_query"));
    }
    Ok(id)
}

pub async fn search(
    State(app): State<AppState>,
    _user: AuthUser,
    body: Result<Json<SearchRequest>, JsonRejection>,
) -> Result<Json<IssueResponse<IssueView>>, ApiError> {
    let Json(search) = body.map_err(|_| ApiError::BadRequest("invalid_request"))?;
    if !(1..=100).contains(&search.limit) || !(1..=86400).contains(&search.interval_seconds) {
        return Err(ApiError::BadRequest("invalid_query"));
    }
    let states = app.store.issue_states().await?;
    let with = |status: &str| -> Vec<String> {
        let mut ids: Vec<_> = states
            .iter()
            .filter(|(_, state)| state.status == status)
            .map(|(id, _)| id.clone())
            .collect();
        ids.sort();
        ids
    };
    let request = CoreSearchRequest {
        resolved: with("resolved"),
        regressed: with("regressed"),
        search,
    };
    let reply = app.core.issues(&request).await.map_err(core_error)?;
    Ok(Json(IssueResponse {
        issues: reply
            .issues
            .into_iter()
            .map(|issue| IssueView {
                state: state_view(states.get(&issue.fingerprint)),
                issue,
            })
            .collect(),
        algorithm_version: reply.algorithm_version,
        as_of: reply.as_of,
        watermark: reply.watermark,
        core_watermark: reply.core_watermark,
        history_from: reply.history_from,
        history_complete: reply.history_complete,
        counts: reply.counts,
        status_events: reply.status_events,
    }))
}

pub async fn details(
    State(app): State<AppState>,
    _user: AuthUser,
    path: Result<Path<String>, PathRejection>,
    body: Result<Json<DetailsRequest>, JsonRejection>,
) -> Result<Json<DetailsView>, ApiError> {
    let id = fingerprint(path)?;
    let Json(request) = body.map_err(|_| ApiError::BadRequest("invalid_request"))?;
    if !(1..=86400).contains(&request.interval_seconds) {
        return Err(ApiError::BadRequest("invalid_query"));
    }
    let details = app
        .core
        .issue_details(&id, &request)
        .await
        .map_err(core_error)?;
    let (state, history) = app.store.issue(id).await?;
    Ok(Json(DetailsView {
        details,
        state: state_view(state.as_ref()),
        history: history
            .into_iter()
            .map(|action: IssueAction| HistoryView {
                action: action.action,
                actor: action.actor,
                at: format_ms(action.at),
                event_at: action.event_at,
            })
            .collect(),
    }))
}

pub async fn resolve(
    State(app): State<AppState>,
    user: AuthUser,
    path: Result<Path<String>, PathRejection>,
) -> Result<StatusCode, ApiError> {
    let id = fingerprint(path)?;
    if !app.store.resolve_issue(id, user.username).await? {
        return Err(ApiError::Conflict("issue_limit"));
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn reopen(
    State(app): State<AppState>,
    user: AuthUser,
    path: Result<Path<String>, PathRejection>,
) -> Result<StatusCode, ApiError> {
    let id = fingerprint(path)?;
    app.store.reopen_issue(id, user.username).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("core: {0}")]
    Core(#[from] CoreQueryError),
    #[error("store: {0}")]
    Store(#[from] StoreError),
}

/// Один шаг наблюдателя по одному сегменту core; true — core ещё не догнан.
pub async fn sync(app: &App) -> Result<bool, SyncError> {
    let (checkpoint, resolved) = app.store.issue_watch().await?;
    let after = checkpoint.unwrap_or(0);
    let request = RecurrenceRequest {
        after,
        issues: resolved
            .into_iter()
            .map(|(fingerprint, resolved_at)| Watch {
                fingerprint,
                since: format_ms(resolved_at),
            })
            .collect(),
    };
    let reply = app.core.issue_recurrences(&request).await?;
    let mut found = vec![];
    for item in reply.recurrences {
        let first = OffsetDateTime::parse(&item.first_seen, &Rfc3339)
            .map_err(|_| CoreQueryError::Unexpected)?;
        found.push(Recurrence {
            fingerprint: item.fingerprint,
            first_ms: (first.unix_timestamp_nanos().div_euclid(1_000_000)) as i64,
            first_seen: item.first_seen,
        });
    }
    let regressed = app
        .store
        .record_recurrences(after, reply.watermark, found)
        .await?;
    if !regressed.is_empty() {
        tracing::info!(count = regressed.len(), "resolved issues regressed");
    }
    Ok(reply.watermark < reply.core_watermark)
}

/// Фоновый наблюдатель до остановки dock; ошибки повторяются на следующем цикле.
pub async fn watch(app: AppState) {
    loop {
        while !app.shutdown.is_cancelled() {
            match sync(&app).await {
                Ok(true) => continue,
                Ok(false) => break,
                Err(error) => {
                    tracing::warn!(%error, "issue watcher step failed");
                    break;
                }
            }
        }
        tokio::select! {
            _ = app.shutdown.cancelled() => return,
            _ = tokio::time::sleep(WATCH_INTERVAL) => {}
        }
    }
}
