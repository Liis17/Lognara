//! Авторизованный поиск и аналитика: wire-контракты core без зависимости на хранилище.

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::api::{ApiError, AppState};
use crate::core_client::CoreQueryError;
use crate::session::AuthUser;

pub type Filters = BTreeMap<String, Vec<String>>;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SearchRequest {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub filters: Filters,
    pub text: Option<TextQuery>,
    #[serde(default = "default_limit")]
    pub limit: usize,
    pub cursor: Option<String>,
}

fn default_limit() -> usize {
    100
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TextQuery {
    pub query: String,
    #[serde(default)]
    pub mode: TextMode,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TextMode {
    #[default]
    All,
    Phrase,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TraceParams {
    pub from: String,
    pub to: String,
    #[serde(default = "default_limit")]
    pub limit: usize,
    pub cursor: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
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

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GroupRequest {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub filters: Filters,
    pub group_by: Vec<String>,
    #[serde(default = "default_limit")]
    pub limit: usize,
}

#[derive(Debug, Deserialize, Serialize)]
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

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SummaryResponse {
    pub as_of: String,
    pub total: u64,
    pub buckets: Vec<SummaryBucket>,
    pub levels: LevelCounts,
    pub error_services: Vec<ErrorService>,
    pub duration: DurationStats,
    pub actions: Vec<ActionStats>,
    #[serde(deserialize_with = "required_nullable")]
    pub page: Option<SearchResponse>,
}

fn required_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

/// Семь уровней обязательны, включая нулевые счётчики.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LevelCounts {
    pub trace: u64,
    pub debug: u64,
    pub info: u64,
    pub warn: u64,
    pub error: u64,
    pub fatal: u64,
    pub unknown: u64,
}

impl LevelCounts {
    pub fn total(&self) -> Option<u64> {
        [
            self.trace,
            self.debug,
            self.info,
            self.warn,
            self.error,
            self.fatal,
            self.unknown,
        ]
        .into_iter()
        .try_fold(0_u64, u64::checked_add)
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SummaryBucket {
    pub timestamp: String,
    pub count: u64,
    pub levels: LevelCounts,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ErrorService {
    #[serde(deserialize_with = "required_nullable")]
    pub service: Option<String>,
    pub count: u64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DurationStats {
    #[serde(deserialize_with = "required_nullable")]
    pub p95_ms: Option<f64>,
    pub measured: u64,
    pub coverage: f64,
    pub method: DurationMethod,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DurationMethod {
    Tdigest,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ActionStats {
    #[serde(deserialize_with = "required_nullable")]
    pub action: Option<String>,
    pub count: u64,
    pub duration: DurationStats,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SearchResponse {
    pub events: Vec<LogEvent>,
    #[serde(deserialize_with = "required_option")]
    pub next_cursor: Option<String>,
    pub as_of: String,
}

fn required_option<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    Option::<String>::deserialize(deserializer)
}

/// JSON core содержит поля источника непосредственно в событии.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LogEvent {
    pub id: String,
    pub timestamp: String,
    pub ingested_at: String,
    pub environment: Option<String>,
    pub server: String,
    pub backend: String,
    pub service: String,
    pub service_instance: Option<String>,
    pub action: Option<String>,
    pub level: LogLevel,
    pub message: String,
    pub trace_id: Option<String>,
    pub span_id: Option<String>,
    pub parent_span_id: Option<String>,
    pub request_id: Option<String>,
    pub attributes: BTreeMap<String, Value>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
    Fatal,
    Unknown,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HistogramResponse {
    pub buckets: Vec<Bucket>,
    pub as_of: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Bucket {
    pub timestamp: String,
    pub count: u64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GroupResponse {
    pub groups: Vec<Group>,
    pub as_of: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Group {
    pub values: BTreeMap<String, Option<String>>,
    pub count: u64,
}

impl From<CoreQueryError> for ApiError {
    fn from(error: CoreQueryError) -> Self {
        match error {
            CoreQueryError::Invalid => Self::BadRequest("invalid_query"),
            CoreQueryError::Gone => Self::CursorExpired,
            CoreQueryError::Unavailable => Self::QueryUnavailable,
            CoreQueryError::Timeout => Self::QueryTimeout,
            CoreQueryError::Unexpected => Self::CoreUnavailable,
            // Эти ответы ожидаются только от маршрутов паттернов, см. patterns::core_error.
            CoreQueryError::NotFound => Self::CoreUnavailable,
            CoreQueryError::NotReady => Self::QueryUnavailable,
        }
    }
}

pub async fn search(
    State(app): State<AppState>,
    _user: AuthUser,
    body: Result<Json<SearchRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(request) = body.map_err(|_| ApiError::BadRequest("invalid_request"))?;
    Ok(Json(app.core.search_page(&request).await?).into_response())
}

pub async fn trace(
    State(app): State<AppState>,
    _user: AuthUser,
    path: Result<Path<String>, PathRejection>,
    params: Result<Query<TraceParams>, QueryRejection>,
) -> Result<Response, ApiError> {
    let Path(trace_id) = path.map_err(|_| ApiError::BadRequest("invalid_request"))?;
    let Query(params) = params.map_err(|_| ApiError::BadRequest("invalid_request"))?;
    Ok(Json(app.core.trace(&trace_id, &params).await?).into_response())
}

pub async fn histogram(
    State(app): State<AppState>,
    _user: AuthUser,
    body: Result<Json<HistogramRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(request) = body.map_err(|_| ApiError::BadRequest("invalid_request"))?;
    Ok(Json(app.core.histogram(&request).await?).into_response())
}

pub async fn group_by(
    State(app): State<AppState>,
    _user: AuthUser,
    body: Result<Json<GroupRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(request) = body.map_err(|_| ApiError::BadRequest("invalid_request"))?;
    Ok(Json(app.core.group_by(&request).await?).into_response())
}

pub async fn summary(
    State(app): State<AppState>,
    _user: AuthUser,
    body: Result<Json<SummaryRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(request) = body.map_err(|_| ApiError::BadRequest("invalid_request"))?;
    Ok(Json(app.core.summary(&request).await?).into_response())
}
