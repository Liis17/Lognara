//! HTTP boundary: auth precedes body buffering and decompression.
use std::{
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant},
};

use axum::{
    Json, Router,
    body::to_bytes,
    extract::{Path, Query, Request, State},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::{
    analytics::{self, GroupRequest, HistogramRequest},
    journal::{IngestError, Journal},
    query::{self, QueryError, SearchRequest},
    storage::Core,
    wire::DecodeError,
};

pub fn router(core: Arc<Core>) -> Router {
    let queries = Router::new()
        .route("/v1/logs/search", post(search))
        .route("/v1/traces/{trace_id}", get(trace))
        .route("/v1/stats/histogram", post(histogram))
        .route("/v1/stats/group-by", post(group_by))
        .route_layer(middleware::from_fn_with_state(
            token_hash(&core.journal.config.query_token),
            authorize,
        ))
        .with_state(core.clone());
    ingest_router(core.journal.clone()).merge(queries)
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

async fn group_by(
    State(core): State<Arc<Core>>,
    request: Result<Json<GroupRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    match request {
        Ok(Json(request)) => run_analytics(&core, analytics::group_by(&core, request)).await,
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

pub fn ingest_router(journal: Arc<Journal>) -> Router {
    let ingest = Router::new()
        .route("/v1/batches", post(ingest))
        .route_layer(middleware::from_fn_with_state(
            token_hash(&journal.config.ingest_token),
            authorize,
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
        let _permit = permit;
        query::search(&worker, request, ascending, started + timeout)
    });
    core.journal.metrics.queries.fetch_add(1, Ordering::Relaxed);
    let result = match tokio::time::timeout(timeout, task).await {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => Err(QueryError::Storage(error.into())),
        Err(_) => Err(QueryError::Timeout),
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
            Self::Busy => StatusCode::SERVICE_UNAVAILABLE,
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
    let token = request
        .headers()
        .get("authorization")
        .and_then(|header| header.to_str().ok())
        .and_then(|value| value.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("Bearer"))
        .map(|(_, token)| token);
    if !token.is_some_and(|token| bool::from(token_hash(token).ct_eq(&expected))) {
        return (
            StatusCode::UNAUTHORIZED,
            [("www-authenticate", "Bearer")],
            "unauthorized",
        )
            .into_response();
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
    let Ok(permit) = journal.ingest_slots.clone().try_acquire_owned() else {
        return unavailable();
    };
    let body = match tokio::time::timeout(
        Duration::from_secs(30),
        to_bytes(request.into_body(), journal.config.max_body_bytes),
    )
    .await
    {
        Ok(Ok(body)) => body,
        Ok(Err(_)) => {
            return (StatusCode::PAYLOAD_TOO_LARGE, "request body exceeds limit").into_response();
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
