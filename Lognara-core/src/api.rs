//! HTTP boundary: auth precedes body buffering and decompression.
use std::{
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant},
};

use axum::{
    Extension, Json, Router,
    body::to_bytes,
    extract::{Path, Query, Request, State},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::{
    access::{Access, AccessError},
    analytics::{self, GroupRequest, HistogramRequest, PreviewRequest},
    definitions::{self, DefinitionInput},
    issues::{self, IssueDetailsRequest, IssueRequest, RecurrenceRequest},
    journal::{IngestError, Journal},
    patterns::{self, DetailsRequest, PatternRequest},
    query::{self, QueryError, SearchRequest},
    series_query::{self, QueryRequest},
    storage::Core,
    summary::{self, SummaryRequest},
    telemetry::{self, RelayHeartbeat},
    wire::DecodeError,
};

pub fn router(core: Arc<Core>) -> Router {
    let queries = Router::new()
        .route("/v1/logs/search", post(search))
        .route("/v1/traces/{trace_id}", get(trace))
        .route("/v1/stats/histogram", post(histogram))
        .route("/v1/stats/group-by", post(group_by))
        .route("/v1/stats/summary", post(summary))
        .route("/v1/patterns/search", post(pattern_search))
        .route("/v1/patterns/{pattern_id}/details", post(pattern_details))
        .route("/v1/issues/search", post(issue_search))
        .route("/v1/issues/recurrences", post(issue_recurrences))
        .route("/v1/issues/{fingerprint}/details", post(issue_details))
        .route("/v1/status", get(status))
        .route("/v1/series", get(list_series))
        .route("/v1/series/query", post(query_series))
        .route_layer(middleware::from_fn_with_state(
            token_hash(&core.journal.config.query_token),
            authorize,
        ))
        .with_state(core.clone());
    let admin = Router::new()
        .route("/v1/admin/relays", get(list_relays).post(create_relay))
        .route("/v1/admin/relays/{id}", axum::routing::delete(delete_relay))
        .route("/v1/admin/agents", get(list_agents).post(create_agent))
        .route("/v1/admin/agents/{id}", axum::routing::delete(delete_agent))
        .route_layer(middleware::from_fn_with_state(
            token_hash(&core.journal.config.admin_token),
            authorize,
        ))
        .with_state(core.journal.access.clone());
    ingest_router(core.journal.clone())
        .merge(queries)
        .merge(admin)
        .merge(admin_router(core.clone(), &core.journal.config.admin_token))
}

async fn histogram(
    State(core): State<Arc<Core>>,
    request: Result<Json<HistogramRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    match request {
        Ok(Json(request)) => run_analytics(&core, analytics::histogram(&core, request)).await,
        Err(error) => QueryError::Invalid(error.body_text()).into_response(),
    }
}

async fn summary(
    State(core): State<Arc<Core>>,
    request: Result<Json<SummaryRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    match request {
        Ok(Json(request)) => run_query(&core, summary::summary(core.clone(), request)).await,
        Err(error) => QueryError::Invalid(error.body_text()).into_response(),
    }
}

async fn pattern_search(
    State(core): State<Arc<Core>>,
    request: Result<Json<PatternRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    match request {
        Ok(Json(request)) => run_analytics(&core, patterns::search(&core, request)).await,
        Err(error) => QueryError::Invalid(error.body_text()).into_response(),
    }
}

async fn pattern_details(
    State(core): State<Arc<Core>>,
    Path(pattern_id): Path<String>,
    request: Result<Json<DetailsRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    match request {
        Ok(Json(request)) => {
            run_analytics(&core, patterns::details(&core, &pattern_id, request)).await
        }
        Err(error) => QueryError::Invalid(error.body_text()).into_response(),
    }
}

async fn issue_search(
    State(core): State<Arc<Core>>,
    request: Result<Json<IssueRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    match request {
        Ok(Json(request)) => run_analytics(&core, issues::search(&core, request)).await,
        Err(error) => QueryError::Invalid(error.body_text()).into_response(),
    }
}

async fn issue_details(
    State(core): State<Arc<Core>>,
    Path(fingerprint): Path<String>,
    request: Result<Json<IssueDetailsRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    match request {
        Ok(Json(request)) => {
            run_analytics(&core, issues::details(&core, &fingerprint, request)).await
        }
        Err(error) => QueryError::Invalid(error.body_text()).into_response(),
    }
}

async fn issue_recurrences(
    State(core): State<Arc<Core>>,
    request: Result<Json<RecurrenceRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    match request {
        Ok(Json(request)) => run_analytics(&core, issues::recurrences(&core, request)).await,
        Err(error) => QueryError::Invalid(error.body_text()).into_response(),
    }
}

/// Не требует готовности: размеры и lag нужны и при недоступном хранилище.
async fn status(State(core): State<Arc<Core>>) -> Response {
    match tokio::task::spawn_blocking(move || crate::status::status(&core)).await {
        Ok(Ok(status)) => Json(status).into_response(),
        Ok(Err(error)) => QueryError::Storage(error.into()).into_response(),
        Err(error) => QueryError::Storage(error.into()).into_response(),
    }
}

async fn group_by(
    State(core): State<Arc<Core>>,
    request: Result<Json<GroupRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    match request {
        Ok(Json(request)) => run_analytics(&core, analytics::group_by(&core, request)).await,
        Err(error) => QueryError::Invalid(error.body_text()).into_response(),
    }
}

async fn list_series(State(core): State<Arc<Core>>) -> Response {
    run_analytics(&core, series_query::list(core.clone())).await
}

async fn query_series(
    State(core): State<Arc<Core>>,
    request: Result<Json<QueryRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    match request {
        Ok(Json(request)) => run_analytics(&core, series_query::query(core.clone(), request)).await,
        Err(error) => QueryError::Invalid(error.body_text()).into_response(),
    }
}

async fn run_analytics<T: serde::Serialize>(
    core: &Core,
    future: impl std::future::Future<Output = Result<T, QueryError>>,
) -> Response {
    if !core.journal.ready() {
        return unavailable();
    }
    let Ok(_permit) = core.analytics_slots.clone().try_acquire_owned() else {
        return QueryError::Busy.into_response();
    };
    run_query(core, future).await
}

async fn run_query<T: serde::Serialize>(
    core: &Core,
    future: impl std::future::Future<Output = Result<T, QueryError>>,
) -> Response {
    if !core.journal.ready() {
        return unavailable();
    }
    let started = Instant::now();
    core.journal.metrics.queries.fetch_add(1, Ordering::Relaxed);
    let result = tokio::time::timeout(core.journal.config.query_timeout, future)
        .await
        .unwrap_or(Err(QueryError::Timeout));
    core.journal
        .metrics
        .query_duration_us
        .fetch_add(started.elapsed().as_micros() as u64, Ordering::Relaxed);
    match result {
        Ok(response) => Json(response).into_response(),
        Err(error) => {
            core.journal
                .metrics
                .query_errors
                .fetch_add(1, Ordering::Relaxed);
            error.into_response()
        }
    }
}

fn admin_router(core: Arc<Core>, token: &str) -> Router {
    Router::new()
        .route(
            "/v1/admin/series/definitions",
            get(list_definitions).post(create_definition),
        )
        .route(
            "/v1/admin/series/definitions/{id}",
            put(update_definition).delete(delete_definition),
        )
        .route("/v1/admin/series/preview", post(preview))
        .route_layer(middleware::from_fn_with_state(token_hash(token), authorize))
        .with_state(core)
}

async fn list_definitions(State(core): State<Arc<Core>>) -> Response {
    match blocking(core, definitions::list).await {
        Ok(definitions) => Json(serde_json::json!({"definitions": definitions})).into_response(),
        Err(error) => error.into_response(),
    }
}

async fn create_definition(
    State(core): State<Arc<Core>>,
    request: Result<Json<DefinitionInput>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let input = match request {
        Ok(Json(input)) => input,
        Err(error) => return QueryError::Invalid(error.body_text()).into_response(),
    };
    match blocking(core, move |core| definitions::create(core, input)).await {
        Ok(definition) => (StatusCode::CREATED, Json(definition)).into_response(),
        Err(error) => error.into_response(),
    }
}

async fn update_definition(
    State(core): State<Arc<Core>>,
    Path(id): Path<String>,
    request: Result<Json<DefinitionInput>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let input = match request {
        Ok(Json(input)) => input,
        Err(error) => return QueryError::Invalid(error.body_text()).into_response(),
    };
    match blocking(core, move |core| definitions::update(core, &id, input)).await {
        Ok(Some(definition)) => Json(definition).into_response(),
        Ok(None) => definition_not_found(),
        Err(error) => error.into_response(),
    }
}

async fn delete_definition(State(core): State<Arc<Core>>, Path(id): Path<String>) -> Response {
    match blocking(core, move |core| definitions::delete(core, &id)).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => definition_not_found(),
        Err(error) => error.into_response(),
    }
}

async fn preview(
    State(core): State<Arc<Core>>,
    request: Result<Json<PreviewRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    match request {
        Ok(Json(request)) => run_analytics(&core, analytics::preview(&core, request)).await,
        Err(error) => QueryError::Invalid(error.body_text()).into_response(),
    }
}

/// Работа с каталогом вне async executor; сбой задачи считается ошибкой хранилища.
async fn blocking<T: Send + 'static>(
    core: Arc<Core>,
    work: impl FnOnce(&Core) -> Result<T, QueryError> + Send + 'static,
) -> Result<T, QueryError> {
    tokio::task::spawn_blocking(move || work(&core))
        .await
        .map_err(|error| QueryError::Storage(error.into()))?
}

fn definition_not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({"error": "definition not found"})),
    )
        .into_response()
}

pub fn ingest_router(journal: Arc<Journal>) -> Router {
    let ingest = Router::new()
        .route("/v1/batches", post(ingest))
        .route("/v1/agents", get(agent_hashes))
        .route("/v1/heartbeat", post(heartbeat))
        .route_layer(middleware::from_fn_with_state(
            journal.access.clone(),
            authorize_relay,
        ));
    let queries =
        Router::new()
            .route("/metrics", get(metrics))
            .route_layer(middleware::from_fn_with_state(
                token_hash(&journal.config.query_token),
                authorize,
            ));
    Router::new()
        .merge(ingest)
        .merge(queries)
        .route("/health/live", get(|| async { StatusCode::OK }))
        .route("/health/ready", get(ready))
        .with_state(journal)
}

async fn search(
    State(core): State<Arc<Core>>,
    request: Result<Json<SearchRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    match request {
        Ok(Json(request)) => run_search(core, request, false).await,
        Err(error) => QueryError::Invalid(error.body_text()).into_response(),
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct TraceParams {
    from: String,
    to: String,
    #[serde(default = "query::default_limit")]
    limit: usize,
    cursor: Option<String>,
}

async fn trace(
    State(core): State<Arc<Core>>,
    Path(trace_id): Path<String>,
    Query(params): Query<TraceParams>,
) -> Response {
    run_search(
        core,
        SearchRequest {
            from: params.from,
            to: params.to,
            filters: std::collections::BTreeMap::from([("trace_id".into(), vec![trace_id])]),
            text: None,
            limit: params.limit,
            cursor: params.cursor,
        },
        true,
    )
    .await
}

async fn run_search(core: Arc<Core>, request: SearchRequest, ascending: bool) -> Response {
    if !core.journal.ready() {
        return unavailable();
    }
    let Ok(permit) = core.search_slots.clone().try_acquire_owned() else {
        return QueryError::Busy.into_response();
    };
    let started = Instant::now();
    let timeout = core.journal.config.query_timeout;
    let worker = core.clone();
    let task = tokio::task::spawn_blocking(move || {
        // Permit остаётся у blocking task даже после таймаута HTTP.
        let result = query::search(&worker, request, ascending, started + timeout);
        (result, permit)
    });
    core.journal.metrics.queries.fetch_add(1, Ordering::Relaxed);
    let (result, _permit) = match tokio::time::timeout(timeout, task).await {
        Ok(Ok((result, permit))) => (result, Some(permit)),
        Ok(Err(error)) => (Err(QueryError::Storage(error.into())), None),
        Err(_) => (Err(QueryError::Timeout), None),
    };
    core.journal
        .metrics
        .query_duration_us
        .fetch_add(started.elapsed().as_micros() as u64, Ordering::Relaxed);
    match result {
        Ok(response) => Json(response).into_response(),
        Err(error) => {
            core.journal
                .metrics
                .query_errors
                .fetch_add(1, Ordering::Relaxed);
            error.into_response()
        }
    }
}

impl IntoResponse for QueryError {
    fn into_response(self) -> Response {
        let status = match &self {
            Self::Invalid(_) => StatusCode::BAD_REQUEST,
            Self::Gone => StatusCode::GONE,
            Self::Timeout => StatusCode::GATEWAY_TIMEOUT,
            Self::Busy | Self::NotReady => StatusCode::SERVICE_UNAVAILABLE,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Storage(error) => {
                tracing::error!(%error, "query failed");
                StatusCode::SERVICE_UNAVAILABLE
            }
        };
        (status, Json(serde_json::json!({"error": self.to_string()}))).into_response()
    }
}

fn token_hash(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

async fn authorize(State(expected): State<[u8; 32]>, request: Request, next: Next) -> Response {
    if !bearer(&request).is_some_and(|token| bool::from(token_hash(token).ct_eq(&expected))) {
        return unauthorized();
    }
    next.run(request).await
}

async fn ingest(State(journal): State<Arc<Journal>>, request: Request) -> Response {
    if !header_is(request.headers(), "content-type", "application/msgpack")
        || !header_is(request.headers(), "content-encoding", "zstd")
    {
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "expected application/msgpack and zstd",
        )
            .into_response();
    }
    if !journal.ready() {
        return unavailable();
    }
    // Короткое ограниченное ожидание позволяет конкурентному повтору дождаться
    // первой записи. Тела ожидающих запросов ещё не читаются в память приложения.
    let Ok(_waiting) = journal.ingest_waiters.clone().try_acquire_owned() else {
        return unavailable();
    };
    let Ok(Ok(permit)) = tokio::time::timeout(
        Duration::from_secs(1),
        journal.ingest_slots.clone().acquire_owned(),
    )
    .await
    else {
        return unavailable();
    };
    let body = match tokio::time::timeout(
        Duration::from_secs(30),
        to_bytes(request.into_body(), journal.config.max_body_bytes),
    )
    .await
    {
        Ok(Ok(body)) => body,
        Ok(Err(error)) => {
            use std::error::Error;
            if error
                .source()
                .is_some_and(|source| source.is::<http_body_util::LengthLimitError>())
            {
                return (StatusCode::PAYLOAD_TOO_LARGE, "request body exceeds limit")
                    .into_response();
            }
            return unavailable();
        }
        Err(_) => return StatusCode::REQUEST_TIMEOUT.into_response(),
    };
    let worker = journal.clone();
    let result = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        worker.accept(&body)
    })
    .await;
    match result {
        Ok(Ok(_)) => StatusCode::NO_CONTENT.into_response(),
        Ok(Err(error)) => {
            journal
                .metrics
                .rejected_batches
                .fetch_add(1, Ordering::Relaxed);
            let status = match error {
                IngestError::TooLarge | IngestError::Invalid(DecodeError::TooLarge) => {
                    StatusCode::PAYLOAD_TOO_LARGE
                }
                IngestError::Invalid(_) => StatusCode::BAD_REQUEST,
                IngestError::Unavailable => return unavailable(),
            };
            (status, error.to_string()).into_response()
        }
        Err(error) => {
            tracing::error!(%error, "ingest task failed");
            journal.healthy.store(false, Ordering::Release);
            unavailable()
        }
    }
}

fn header_is(headers: &HeaderMap, name: &str, expected: &str) -> bool {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case(expected))
}

pub(crate) fn unavailable() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [("retry-after", "1")],
        "storage temporarily unavailable",
    )
        .into_response()
}

async fn ready(State(journal): State<Arc<Journal>>) -> StatusCode {
    if journal.ready() {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

async fn metrics(State(journal): State<Arc<Journal>>) -> Response {
    (
        [("content-type", "text/plain; version=0.0.4; charset=utf-8")],
        journal.metrics.render(),
    )
        .into_response()
}

fn bearer(request: &Request) -> Option<&str> {
    let mut headers = request.headers().get_all("authorization").iter();
    let header = headers.next()?;
    if headers.next().is_some() {
        return None;
    }
    let (scheme, token) = header.to_str().ok()?.split_once(' ')?;
    (scheme.eq_ignore_ascii_case("Bearer") && !token.is_empty()).then_some(token)
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [("www-authenticate", "Bearer")],
        Json(serde_json::json!({"error":"unauthorized"})),
    )
        .into_response()
}

async fn authorize_relay(
    State(access): State<Arc<Access>>,
    mut request: Request,
    next: Next,
) -> Response {
    let relay_id = bearer(&request).and_then(|token| access.relay_for_token(&token_hash(token)));
    let Some(relay_id) = relay_id else {
        return unauthorized();
    };
    request.extensions_mut().insert(relay_id);
    next.run(request).await
}

impl IntoResponse for AccessError {
    fn into_response(self) -> Response {
        let (status, code) = match self {
            Self::InvalidName => (StatusCode::BAD_REQUEST, "invalid_name"),
            Self::NameTaken => (StatusCode::CONFLICT, "name_taken"),
            Self::RelayNotFound => (StatusCode::NOT_FOUND, "relay_not_found"),
            Self::NotFound => (StatusCode::NOT_FOUND, "not_found"),
            Self::RelayHasAgents => (StatusCode::CONFLICT, "relay_has_agents"),
            Self::Storage(error) => {
                tracing::error!(%error, "access operation failed");
                (StatusCode::SERVICE_UNAVAILABLE, "storage_unavailable")
            }
        };
        (status, Json(serde_json::json!({"error":code}))).into_response()
    }
}

async fn access_task<T: serde::Serialize + Send + 'static>(
    task: impl FnOnce() -> Result<T, AccessError> + Send + 'static,
    status: StatusCode,
) -> Response {
    match tokio::task::spawn_blocking(task).await {
        Ok(Ok(_)) if status == StatusCode::NO_CONTENT => status.into_response(),
        Ok(Ok(value)) => (status, Json(value)).into_response(),
        Ok(Err(error)) => error.into_response(),
        Err(error) => AccessError::Storage(error.into()).into_response(),
    }
}

/// `as_of` — часы core, относительно которых считается свежесть heartbeat.
async fn list_relays(State(access): State<Arc<Access>>) -> Response {
    access_task(
        move || Ok(serde_json::json!({"relays":access.list_relays()?, "as_of":as_of()})),
        StatusCode::OK,
    )
    .await
}
async fn list_agents(State(access): State<Arc<Access>>) -> Response {
    access_task(
        move || Ok(serde_json::json!({"agents":access.list_agents()?, "as_of":as_of()})),
        StatusCode::OK,
    )
    .await
}
fn as_of() -> String {
    crate::model::format_timestamp(crate::model::now_nanos())
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RelayName {
    name: String,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentName {
    name: String,
    relay_id: i64,
}
fn invalid_request() -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({"error":"invalid_request"})),
    )
        .into_response()
}
async fn create_relay(
    State(access): State<Arc<Access>>,
    body: Result<Json<RelayName>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Ok(Json(body)) = body else {
        return invalid_request();
    };
    access_task(move || access.create_relay(&body.name), StatusCode::CREATED).await
}
async fn create_agent(
    State(access): State<Arc<Access>>,
    body: Result<Json<AgentName>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Ok(Json(body)) = body else {
        return invalid_request();
    };
    access_task(
        move || access.create_agent(&body.name, body.relay_id),
        StatusCode::CREATED,
    )
    .await
}
async fn delete_relay(
    State(access): State<Arc<Access>>,
    id: Result<Path<i64>, axum::extract::rejection::PathRejection>,
) -> Response {
    let Ok(Path(id)) = id else {
        return invalid_request();
    };
    access_task(move || access.delete_relay(id), StatusCode::NO_CONTENT).await
}
async fn delete_agent(
    State(access): State<Arc<Access>>,
    id: Result<Path<i64>, axum::extract::rejection::PathRejection>,
) -> Response {
    let Ok(Path(id)) = id else {
        return invalid_request();
    };
    access_task(move || access.delete_agent(id), StatusCode::NO_CONTENT).await
}
async fn agent_hashes(
    State(journal): State<Arc<Journal>>,
    Extension(relay_id): Extension<i64>,
) -> Response {
    access_task(
        move || Ok(serde_json::json!({"agents":journal.access.agent_hashes(relay_id)?})),
        StatusCode::OK,
    )
    .await
}

async fn heartbeat(
    State(journal): State<Arc<Journal>>,
    Extension(relay_id): Extension<i64>,
    request: Request,
) -> Response {
    let body = match to_bytes(request.into_body(), telemetry::MAX_BODY).await {
        Ok(body) => body,
        Err(_) => {
            return (StatusCode::PAYLOAD_TOO_LARGE, "heartbeat exceeds limit").into_response();
        }
    };
    let heartbeat = match serde_json::from_slice::<RelayHeartbeat>(&body) {
        Ok(heartbeat) if heartbeat.valid() => heartbeat,
        _ => return invalid_request(),
    };
    access_task(
        move || journal.access.record_heartbeat(relay_id, heartbeat),
        StatusCode::NO_CONTENT,
    )
    .await
}
