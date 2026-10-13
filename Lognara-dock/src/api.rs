//! HTTP-маршруты dock, общее состояние и ошибки API.

use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use axum::http::header::RETRY_AFTER;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router, middleware};
use serde_json::json;

use crate::account;
use crate::auth::{self, LoginLimiter};
use crate::config::Config;
use crate::htpasswd::Htpasswd;
use crate::session::{self, Sessions};
use crate::store::{Store, StoreError};

pub type AppState = Arc<App>;

pub struct App {
    pub config: Config,
    pub htpasswd: Htpasswd,
    pub store: Store,
    pub sessions: Sessions,
    pub limiter: LoginLimiter,
}

impl App {
    /// Читает htpasswd и открывает каталог данных; ошибки останавливают запуск.
    pub fn open(config: Config) -> Result<AppState, Box<dyn std::error::Error + Send + Sync>> {
        let htpasswd = Htpasswd::open(&config.htpasswd_file)?;
        let store = Store::open(&config.data_dir)?;
        let sessions = Sessions::new(store.clone(), config.session_ttl, config.session_idle);
        Ok(Arc::new(Self {
            config,
            htpasswd,
            store,
            sessions,
            limiter: LoginLimiter::default(),
        }))
    }
}

#[derive(Debug)]
pub enum ApiError {
    BadRequest(&'static str),
    Unauthorized,
    Forbidden,
    NotFound,
    TooManyRequests(Duration),
    Internal,
}

impl From<StoreError> for ApiError {
    fn from(err: StoreError) -> Self {
        tracing::error!("{err}");
        Self::Internal
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code) = match self {
            Self::BadRequest(code) => (StatusCode::BAD_REQUEST, code),
            Self::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized"),
            Self::Forbidden => (StatusCode::FORBIDDEN, "forbidden"),
            Self::NotFound => (StatusCode::NOT_FOUND, "not_found"),
            Self::TooManyRequests(wait) => {
                let body = Json(json!({ "error": "too_many_attempts" }));
                let retry = (wait.as_secs() + 1).to_string();
                return (StatusCode::TOO_MANY_REQUESTS, [(RETRY_AFTER, retry)], body)
                    .into_response();
            }
            Self::Internal => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
        };
        (status, Json(json!({ "error": code }))).into_response()
    }
}

pub fn router(app: AppState) -> Router {
    let api = Router::new()
        .route("/auth/login", post(auth::login))
        .route("/auth/logout", post(auth::logout))
        .route("/me", get(account::me))
        .layer(middleware::from_fn_with_state(
            app.clone(),
            session::require_origin,
        ));
    Router::new()
        .nest("/api", api)
        .route("/health/live", get(|| async { StatusCode::OK }))
        .route("/health/ready", get(ready))
        .with_state(app)
}

/// Готов, если htpasswd читается и корректен.
async fn ready(axum::extract::State(app): axum::extract::State<AppState>) -> StatusCode {
    if app.htpasswd.users().is_some() {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}
