//! Серверные паттерны и персональная видимость без передачи core-токенов браузеру.

use std::collections::BTreeSet;

use axum::Json;
use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};

use crate::api::{ApiError, AppState};
use crate::core_client::CoreQueryError;
use crate::query::{Bucket, Filters, LogEvent, LogLevel, TextQuery};
use crate::session::AuthUser;

pub fn valid_id(id: &str) -> bool {
    id.len() == 67
        && id.starts_with("p1_")
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

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PatternRequest {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub filters: Filters,
    pub text: Option<TextQuery>,
    #[serde(default = "interval")]
    pub interval_seconds: u64,
    #[serde(default = "limit")]
    pub limit: usize,
    #[serde(default)]
    pub sort: Sort,
    #[serde(default)]
    pub exclude_patterns: Vec<String>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Sort {
    #[default]
    Count,
    Growth,
    LastSeen,
}

#[derive(Debug, Deserialize, Serialize)]
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

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PatternResponse {
    pub algorithm_version: u8,
    pub as_of: String,
    pub watermark: u64,
    pub core_watermark: u64,
    #[serde(deserialize_with = "required_nullable")]
    pub history_from: Option<String>,
    pub history_complete: bool,
    pub total_events: u64,
    pub total_patterns: u64,
    pub visible_patterns: u64,
    #[serde(deserialize_with = "required_nullable")]
    pub new_patterns: Option<u64>,
    pub patterns: Vec<Pattern>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Pattern {
    pub pattern_id: String,
    #[serde(deserialize_with = "required_nullable")]
    pub environment: Option<String>,
    pub service: String,
    pub level: LogLevel,
    pub template: String,
    pub parts: Vec<Part>,
    pub exact: bool,
    pub count: u64,
    pub share: f64,
    pub previous_count: u64,
    #[serde(deserialize_with = "required_nullable")]
    pub growth_percent: Option<f64>,
    #[serde(deserialize_with = "required_nullable")]
    pub is_new: Option<bool>,
    pub first_seen: String,
    pub last_seen: String,
    pub buckets: Vec<Bucket>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum Part {
    Literal {
        text: String,
    },
    Variable {
        name: String,
        variable_type: VariableType,
    },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum VariableType {
    Ip,
    Uuid,
    Id,
    Duration,
    Hex,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DetailsResponse {
    pub as_of: String,
    pub pattern: Pattern,
    pub variables: Vec<Variable>,
    pub examples: Vec<LogEvent>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Variable {
    pub name: String,
    pub variable_type: VariableType,
    pub total: u64,
    pub values: Vec<VariableValue>,
    pub other_count: u64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VariableValue {
    pub value: String,
    pub count: u64,
}

pub(crate) fn required_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

impl Pattern {
    pub fn valid(&self) -> bool {
        valid_id(&self.pattern_id)
            && self.template.len() <= 4096
            && self
                .parts
                .iter()
                .filter(|part| matches!(part, Part::Variable { .. }))
                .count()
                <= 64
            && self.share.is_finite()
            && (0.0..=1.0).contains(&self.share)
            && self.growth_percent.is_none_or(f64::is_finite)
            && (self.previous_count != 0 || self.growth_percent.is_none())
            && self.buckets.len() <= 10_000
            && self
                .buckets
                .iter()
                .try_fold(0_u64, |total, bucket| total.checked_add(bucket.count))
                == Some(self.count)
    }
}

impl PatternResponse {
    pub fn valid(&self, limit: usize) -> bool {
        self.algorithm_version == 1
            && self.watermark <= self.core_watermark
            && self.visible_patterns <= self.total_patterns
            && self.total_patterns <= self.total_events
            && self.patterns.len() <= limit
            && self.patterns.len() as u64 <= self.visible_patterns
            && self
                .new_patterns
                .is_none_or(|count| count <= self.visible_patterns)
            && (self.history_complete || self.new_patterns.is_none())
            && self
                .patterns
                .iter()
                .all(|pattern| pattern.valid() && pattern.count <= self.total_events)
            && self
                .patterns
                .iter()
                .map(|pattern| &pattern.pattern_id)
                .collect::<BTreeSet<_>>()
                .len()
                == self.patterns.len()
    }
}

impl DetailsResponse {
    pub fn valid(&self, pattern_id: &str) -> bool {
        self.pattern.pattern_id == pattern_id
            && self.pattern.valid()
            && self.variables.len() <= 64
            && self.examples.len() <= 3
            && self.variables.iter().all(|variable| {
                variable.values.len() <= 5
                    && variable
                        .values
                        .iter()
                        .try_fold(variable.other_count, |total, value| {
                            total.checked_add(value.count)
                        })
                        == Some(variable.total)
            })
    }
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchParams {
    #[serde(default)]
    pub show_hidden: bool,
}

pub async fn search(
    State(app): State<AppState>,
    user: AuthUser,
    params: Result<Query<SearchParams>, QueryRejection>,
    body: Result<Json<PatternRequest>, JsonRejection>,
) -> Result<Json<PatternResponse>, ApiError> {
    let Query(params) = params.map_err(|_| ApiError::BadRequest("invalid_request"))?;
    let Json(mut request) = body.map_err(|_| ApiError::BadRequest("invalid_request"))?;
    if !(1..=100).contains(&request.limit)
        || !(1..=86400).contains(&request.interval_seconds)
        || request.exclude_patterns.len() > 1000
        || request.exclude_patterns.iter().any(|id| !valid_id(id))
    {
        return Err(ApiError::BadRequest("invalid_query"));
    }
    if !params.show_hidden {
        request
            .exclude_patterns
            .extend(app.store.hidden_patterns(user.username).await?);
        request.exclude_patterns.sort();
        request.exclude_patterns.dedup();
        if request.exclude_patterns.len() > 1000 {
            return Err(ApiError::BadRequest("invalid_query"));
        }
    }
    Ok(Json(app.core.patterns(&request).await.map_err(core_error)?))
}

fn core_error(error: CoreQueryError) -> ApiError {
    match error {
        CoreQueryError::NotFound => ApiError::PatternNotFound,
        CoreQueryError::NotReady => ApiError::PatternsNotReady,
        error => error.into(),
    }
}

fn pattern_id(path: Result<Path<String>, PathRejection>) -> Result<String, ApiError> {
    let Path(id) = path.map_err(|_| ApiError::BadRequest("invalid_request"))?;
    if !valid_id(&id) {
        return Err(ApiError::BadRequest("invalid_query"));
    }
    Ok(id)
}

pub async fn details(
    State(app): State<AppState>,
    _user: AuthUser,
    path: Result<Path<String>, PathRejection>,
    body: Result<Json<DetailsRequest>, JsonRejection>,
) -> Result<Json<DetailsResponse>, ApiError> {
    let id = pattern_id(path)?;
    let Json(request) = body.map_err(|_| ApiError::BadRequest("invalid_request"))?;
    if !(1..=86400).contains(&request.interval_seconds) {
        return Err(ApiError::BadRequest("invalid_query"));
    }
    Ok(Json(
        app.core
            .pattern_details(&id, &request)
            .await
            .map_err(core_error)?,
    ))
}

#[derive(Serialize)]
pub struct HiddenResponse {
    pub pattern_ids: Vec<String>,
}

pub async fn hidden(
    State(app): State<AppState>,
    user: AuthUser,
) -> Result<Json<HiddenResponse>, ApiError> {
    Ok(Json(HiddenResponse {
        pattern_ids: app.store.hidden_patterns(user.username).await?,
    }))
}

pub async fn hide(
    State(app): State<AppState>,
    user: AuthUser,
    path: Result<Path<String>, PathRejection>,
) -> Result<StatusCode, ApiError> {
    let id = pattern_id(path)?;
    if !app.store.hide_pattern(user.username, id).await? {
        return Err(ApiError::Conflict("pattern_limit"));
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn show(
    State(app): State<AppState>,
    user: AuthUser,
    path: Result<Path<String>, PathRejection>,
) -> Result<StatusCode, ApiError> {
    let id = pattern_id(path)?;
    app.store.show_pattern(user.username, id).await?;
    Ok(StatusCode::NO_CONTENT)
}
