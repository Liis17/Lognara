//! HTTP boundary: auth precedes body buffering and decompression.
use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

use axum::{
    Router,
    body::to_bytes,
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::{
    journal::{IngestError, Journal},
    wire::DecodeError,
};

pub fn router(journal: Arc<Journal>) -> Router {
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
