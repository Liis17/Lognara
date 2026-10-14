//! HTTP-маршруты dock, общее состояние и ошибки API.

use std::sync::Arc;
use std::time::Duration;

use axum::http::HeaderValue;
use axum::http::StatusCode;
use axum::http::header::{
    CACHE_CONTROL, REFERRER_POLICY, RETRY_AFTER, X_CONTENT_TYPE_OPTIONS, X_FRAME_OPTIONS,
};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router, middleware};
use serde_json::json;
use tokio_util::sync::CancellationToken;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::set_header::SetResponseHeaderLayer;

use crate::access;
use crate::account;
use crate::auth::{self, LoginLimiter};
use crate::config::Config;
use crate::core_client::CoreClient;
use crate::htpasswd::Htpasswd;
use crate::issues;
use crate::live::{self, Live};
use crate::passkey::{self, Challenges};
use crate::patterns;
use crate::query;
use crate::session::{self, Sessions};
use crate::store::{Store, StoreError};

pub type AppState = Arc<App>;

pub struct App {
    pub config: Config,
    pub htpasswd: Htpasswd,
    pub store: Store,
    pub sessions: Sessions,
    pub limiter: LoginLimiter,
    pub webauthn: webauthn_rs::Webauthn,
    pub challenges: Challenges,
    pub live: Arc<Live>,
    pub core: CoreClient,
    /// Отменяется при остановке: закрывает SSE-потоки и опрос core.
    pub shutdown: CancellationToken,
}

impl App {
    /// Читает htpasswd и открывает каталог данных; ошибки останавливают запуск.
    pub fn open(config: Config) -> Result<AppState, Box<dyn std::error::Error + Send + Sync>> {
        let webauthn = passkey::webauthn(&config)?;
        let htpasswd = Htpasswd::open(&config.htpasswd_file)?;
        let store = Store::open(&config.data_dir)?;
        let sessions = Sessions::new(store.clone(), config.session_ttl, config.session_idle);
        let shutdown = CancellationToken::new();
        Ok(Arc::new(Self {
            htpasswd,
            store,
            sessions,
            limiter: LoginLimiter::default(),
            webauthn,
            challenges: Challenges::default(),
            live: Live::new(&config, shutdown.clone()),
            core: CoreClient::new(&config),
            config,
            shutdown,
        }))
    }
}

#[derive(Debug)]
pub enum ApiError {
    BadRequest(&'static str),
    Unauthorized,
    Forbidden,
    NotFound,
    RelayNotFound,
    Conflict(&'static str),
    TooManyRequests(Duration),
    Internal,
    /// Core недоступен или ответил неожиданно.
    CoreUnavailable,
    CursorExpired,
    QueryUnavailable,
    QueryTimeout,
    PatternNotFound,
    PatternsNotReady,
    IssueNotFound,
    IssuesNotReady,
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
            Self::RelayNotFound => (StatusCode::NOT_FOUND, "relay_not_found"),
            Self::Conflict(code) => (StatusCode::CONFLICT, code),
            Self::TooManyRequests(wait) => {
                let body = Json(json!({ "error": "too_many_attempts" }));
                let retry = (wait.as_secs() + 1).to_string();
                return (StatusCode::TOO_MANY_REQUESTS, [(RETRY_AFTER, retry)], body)
                    .into_response();
            }
            Self::Internal => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
            Self::CoreUnavailable => (StatusCode::BAD_GATEWAY, "core_unavailable"),
            Self::CursorExpired => (StatusCode::GONE, "cursor_expired"),
            Self::QueryUnavailable => (StatusCode::SERVICE_UNAVAILABLE, "query_unavailable"),
            Self::QueryTimeout => (StatusCode::GATEWAY_TIMEOUT, "query_timeout"),
            Self::PatternNotFound => (StatusCode::NOT_FOUND, "pattern_not_found"),
            Self::PatternsNotReady => (StatusCode::SERVICE_UNAVAILABLE, "patterns_not_ready"),
            Self::IssueNotFound => (StatusCode::NOT_FOUND, "issue_not_found"),
            Self::IssuesNotReady => (StatusCode::SERVICE_UNAVAILABLE, "issues_not_ready"),
        };
        (status, Json(json!({ "error": code }))).into_response()
    }
}

pub fn router(app: AppState) -> Router {
    let api = Router::new()
        .route("/auth/login", post(auth::login))
        .route("/auth/logout", post(auth::logout))
        .route("/auth/passkey/start", post(passkey::login_start))
        .route("/auth/passkey/finish", post(passkey::login_finish))
        .route("/me", get(account::me))
        .route("/account/password", post(account::change_password))
        .route("/account/passkeys", get(passkey::list))
        .route("/account/passkeys/start", post(passkey::register_start))
        .route("/account/passkeys/finish", post(passkey::register_finish))
        .route("/account/passkeys/{id}", delete(passkey::delete))
        .route("/logs/stream", get(live::stream))
        .route("/logs/search", post(query::search))
        .route("/traces/{trace_id}", get(query::trace))
        .route("/stats/histogram", post(query::histogram))
        .route("/stats/group-by", post(query::group_by))
        .route("/stats/summary", post(query::summary))
        .route("/patterns/search", post(patterns::search))
        .route("/patterns/hidden", get(patterns::hidden))
        .route("/patterns/{id}/details", post(patterns::details))
        .route(
            "/patterns/{id}/hide",
            post(patterns::hide).delete(patterns::show),
        )
        .route("/issues/search", post(issues::search))
        .route("/issues/{id}/details", post(issues::details))
        .route("/issues/{id}/resolve", post(issues::resolve))
        .route("/issues/{id}/reopen", post(issues::reopen))
        .route(
            "/relays",
            get(access::list_relays).post(access::create_relay),
        )
        .route("/relays/{id}", delete(access::delete_relay))
        .route(
            "/agents",
            get(access::list_agents).post(access::create_agent),
        )
        .route("/agents/{id}", delete(access::delete_agent))
        .layer(middleware::from_fn_with_state(
            app.clone(),
            session::require_origin,
        ))
        .fallback(|| async { ApiError::NotFound })
        .layer(SetResponseHeaderLayer::if_not_present(
            CACHE_CONTROL,
            HeaderValue::from_static("no-store"),
        ));
    // Остальные пути отдаёт SPA: неизвестный путь получает index.html для react-router.
    let web = &app.config.web_dir;
    let spa = ServeDir::new(web).fallback(ServeFile::new(web.join("index.html")));
    Router::new()
        .nest("/api", api)
        .route("/health/live", get(|| async { StatusCode::OK }))
        .route("/health/ready", get(ready))
        .fallback_service(spa)
        .layer(SetResponseHeaderLayer::if_not_present(
            X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            X_FRAME_OPTIONS,
            HeaderValue::from_static("DENY"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            REFERRER_POLICY,
            HeaderValue::from_static("same-origin"),
        ))
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
