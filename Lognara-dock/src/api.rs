//! HTTP-маршруты dock.

use axum::Router;
use axum::http::StatusCode;
use axum::routing::get;

pub fn router() -> Router {
    Router::new()
        .route("/health/live", get(|| async { StatusCode::OK }))
        .route("/health/ready", get(|| async { StatusCode::OK }))
}
