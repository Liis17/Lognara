//! Вход по паролю из htpasswd и выход.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::http::header::SET_COOKIE;
use axum::response::IntoResponse;
use serde::Deserialize;
use serde_json::json;

use crate::api::{ApiError, AppState};
use crate::session::{AuthUser, SESSION_COOKIE, cookie, session_cookie};

/// Столько неудачных попыток подряд блокируют вход по паролю для логина.
const MAX_FAILURES: u32 = 5;
const LOCKOUT: Duration = Duration::from_secs(300);
/// Предел записей, чтобы перебор логинов не занимал память без границ.
const MAX_TRACKED: usize = 10_000;
pub const MAX_USERNAME_BYTES: usize = 256;
pub const MAX_PASSWORD_BYTES: usize = 1024;

#[derive(Default)]
pub struct LoginLimiter {
    entries: Mutex<HashMap<String, Failures>>,
}

struct Failures {
    count: u32,
    last: Instant,
}

impl LoginLimiter {
    /// Сколько ещё ждать, если логин заблокирован.
    pub fn locked_for(&self, username: &str) -> Option<Duration> {
        let entries = self.entries.lock().unwrap();
        let entry = entries.get(username)?;
        let elapsed = entry.last.elapsed();
        (entry.count >= MAX_FAILURES && elapsed < LOCKOUT).then(|| LOCKOUT - elapsed)
    }

    pub fn failure(&self, username: &str) {
        let mut entries = self.entries.lock().unwrap();
        if entries.len() >= MAX_TRACKED {
            entries.retain(|_, entry| entry.last.elapsed() < LOCKOUT);
        }
        if entries.len() >= MAX_TRACKED && !entries.contains_key(username) {
            return;
        }
        let entry = entries.entry(username.to_owned()).or_insert(Failures {
            count: 0,
            last: Instant::now(),
        });
        if entry.last.elapsed() >= LOCKOUT {
            entry.count = 0;
        }
        entry.count += 1;
        entry.last = Instant::now();
    }

    pub fn success(&self, username: &str) {
        self.entries.lock().unwrap().remove(username);
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoginRequest {
    username: String,
    password: String,
}

pub async fn login(
    State(app): State<AppState>,
    Json(request): Json<LoginRequest>,
) -> Result<impl IntoResponse, ApiError> {
    if request.username.len() > MAX_USERNAME_BYTES || request.password.len() > MAX_PASSWORD_BYTES {
        return Err(ApiError::Unauthorized);
    }
    if let Some(wait) = app.limiter.locked_for(&request.username) {
        return Err(ApiError::TooManyRequests(wait));
    }
    let checker = app.clone();
    let username = request.username.clone();
    let valid =
        tokio::task::spawn_blocking(move || checker.htpasswd.verify(&username, &request.password))
            .await
            .map_err(|_| ApiError::Internal)?;
    if !valid {
        app.limiter.failure(&request.username);
        tracing::warn!(username = %request.username, "password login failed");
        return Err(ApiError::Unauthorized);
    }
    app.limiter.success(&request.username);
    start_session(&app, request.username).await
}

/// Создаёт сессию и отдаёт её cookie.
pub async fn start_session(
    app: &AppState,
    username: String,
) -> Result<impl IntoResponse + use<>, ApiError> {
    let token = app.sessions.create(&username).await?;
    tracing::info!(%username, "session started");
    Ok((
        [(
            SET_COOKIE,
            session_cookie(&token, app.sessions.ttl_seconds()),
        )],
        Json(json!({ "username": username })),
    ))
}

pub async fn logout(
    State(app): State<AppState>,
    user: AuthUser,
) -> Result<impl IntoResponse, ApiError> {
    app.sessions.revoke(user.token_hash).await?;
    Ok((
        StatusCode::NO_CONTENT,
        [(SET_COOKIE, cookie(SESSION_COOKIE, "", 0))],
    ))
}
