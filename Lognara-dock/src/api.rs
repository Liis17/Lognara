//! HTTP-маршруты dock, общее состояние и ошибки API.

use std::sync::Arc;
use std::time::Duration;

use axum::http::HeaderValue;
use axum::http::StatusCode;
use axum::http::header::{REFERRER_POLICY, RETRY_AFTER, X_CONTENT_TYPE_OPTIONS, X_FRAME_OPTIONS};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router, middleware};
use serde_json::json;
use tokio_util::sync::CancellationToken;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::set_header::SetResponseHeaderLayer;

use crate::account;
use crate::auth::{self, LoginLimiter};
use crate::config::Config;
use crate::core_client::{CoreClient, CoreError};
use crate::htpasswd::Htpasswd;
use crate::live::{self, Live};
use crate::passkey::{self, Challenges};
use crate::series;
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
    /// Общий клиент core: query-токен для логов и метрик, admin-токен для определений.
    pub core: Arc<CoreClient>,
    pub live: Arc<Live>,
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
        let core = Arc::new(CoreClient::new(&config));
        Ok(Arc::new(Self {
            htpasswd,
            store,
            sessions,
            limiter: LoginLimiter::default(),
            webauthn,
            challenges: Challenges::default(),
            live: Live::new(&config, core.clone(), shutdown.clone()),
            core,
            config,
            shutdown,
        }))
    }
}

#[derive(Debug)]
pub enum ApiError {
    BadRequest(&'static str),
    /// Ошибка валидации из core; `detail` - текст core для пользователя.
    Invalid(String),
    Unauthorized,
    Forbidden,
    NotFound,
    TooManyRequests(Duration),
    /// Admin-токен не задан в конфигурации.
    AdminDisabled,
    CoreUnavailable,
    Internal,
}

impl From<StoreError> for ApiError {
    fn from(err: StoreError) -> Self {
        tracing::error!("{err}");
        Self::Internal
    }
}

impl From<CoreError> for ApiError {
    fn from(err: CoreError) -> Self {
        match err {
            CoreError::AdminDisabled => Self::AdminDisabled,
            CoreError::Status {
                status: StatusCode::BAD_REQUEST,
                detail,
            } => Self::Invalid(detail.unwrap_or_else(|| "core rejected the request".into())),
            CoreError::Status {
                status: StatusCode::NOT_FOUND,
                ..
            } => Self::NotFound,
            CoreError::Status {
                status: StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN,
                ..
            } => {
                tracing::warn!(
                    "core rejected a token: check LOGNARA_DOCK_CORE_TOKEN and LOGNARA_DOCK_CORE_ADMIN_TOKEN"
                );
                Self::Internal
            }
            CoreError::Status { status, .. } if status.is_server_error() => {
                tracing::warn!("core unavailable: {status}");
                Self::CoreUnavailable
            }
            CoreError::Request(err) => {
                tracing::warn!("core unavailable: {err}");
                Self::CoreUnavailable
            }
            err => {
                tracing::error!("unexpected core answer: {err}");
                Self::Internal
            }
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code) = match self {
            Self::BadRequest(code) => (StatusCode::BAD_REQUEST, code),
            Self::Invalid(detail) => {
                let body = Json(json!({ "error": "invalid", "detail": detail }));
                return (StatusCode::BAD_REQUEST, body).into_response();
            }
            Self::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized"),
            Self::Forbidden => (StatusCode::FORBIDDEN, "forbidden"),
            Self::NotFound => (StatusCode::NOT_FOUND, "not_found"),
            Self::TooManyRequests(wait) => {
                let body = Json(json!({ "error": "too_many_attempts" }));
                let retry = (wait.as_secs() + 1).to_string();
                return (StatusCode::TOO_MANY_REQUESTS, [(RETRY_AFTER, retry)], body)
                    .into_response();
            }
            Self::AdminDisabled => (StatusCode::NOT_FOUND, "admin_disabled"),
            Self::CoreUnavailable => (StatusCode::SERVICE_UNAVAILABLE, "core_unavailable"),
            Self::Internal => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
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
        .route("/series", get(series::list))
        .route("/series/query", post(series::query))
        .route(
            "/series/definitions",
            get(series::definitions).post(series::create_definition),
        )
        .route(
            "/series/definitions/{id}",
            put(series::update_definition).delete(series::delete_definition),
        )
        .route("/series/preview", post(series::preview))
        .layer(middleware::from_fn_with_state(
            app.clone(),
            session::require_origin,
        ))
        .fallback(|| async { ApiError::NotFound });
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
