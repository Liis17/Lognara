//! Сессии в cookie: в SQLite хранится только SHA-256 токена.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{FromRequestParts, Request, State};
use axum::http::header::{AUTHORIZATION, COOKIE, ORIGIN};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, Method};
use axum::middleware::Next;
use axum::response::Response;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore;
use rusqlite::{OptionalExtension, params};
use sha2::{Digest, Sha256};

use crate::api::{ApiError, AppState};
use crate::store::{Store, StoreError, now_ms};

pub const SESSION_COOKIE: &str = "lognara_dock_session";
/// `last_seen` обновляется не чаще раза в минуту.
const TOUCH_INTERVAL_MS: i64 = 60_000;

pub type TokenHash = [u8; 32];

#[derive(Clone)]
pub struct Sessions {
    store: Store,
    ttl_ms: i64,
    idle_ms: i64,
}

impl Sessions {
    pub fn new(store: Store, ttl: std::time::Duration, idle: std::time::Duration) -> Self {
        Self {
            store,
            ttl_ms: ttl.as_millis() as i64,
            idle_ms: idle.as_millis() as i64,
        }
    }

    pub fn ttl_seconds(&self) -> i64 {
        self.ttl_ms / 1000
    }

    /// Создаёт сессию и возвращает токен для cookie. Попутно удаляет истёкшие.
    pub async fn create(&self, username: &str) -> Result<String, StoreError> {
        let mut token = [0u8; 32];
        rand::rng().fill_bytes(&mut token);
        let hash = hash_token(&URL_SAFE_NO_PAD.encode(token));
        let (username, ttl, idle) = (username.to_owned(), self.ttl_ms, self.idle_ms);
        self.store
            .call(move |conn| {
                let now = now_ms();
                conn.execute(
                    "DELETE FROM sessions WHERE created_at <= ?1 OR last_seen <= ?2",
                    params![now - ttl, now - idle],
                )?;
                conn.execute(
                    "INSERT INTO sessions (token_hash, username, created_at, last_seen)
                     VALUES (?1, ?2, ?3, ?3)",
                    params![hash, username, now],
                )
            })
            .await?;
        Ok(URL_SAFE_NO_PAD.encode(token))
    }

    /// Число действующих сессий всех пользователей.
    pub async fn active(&self) -> Result<u64, StoreError> {
        let (ttl, idle) = (self.ttl_ms, self.idle_ms);
        self.store
            .call(move |conn| {
                let now = now_ms();
                conn.query_row(
                    "SELECT count(*) FROM sessions WHERE created_at > ?1 AND last_seen > ?2",
                    params![now - ttl, now - idle],
                    |row| row.get::<_, i64>(0),
                )
            })
            .await
            .map(|count| count as u64)
    }

    /// Находит действующую сессию и продлевает её активность.
    pub async fn lookup(&self, token: &str) -> Result<Option<(String, TokenHash)>, StoreError> {
        self.lookup_hash(hash_token(token)).await
    }

    /// То же по хэшу токена: для долгих соединений, которые перепроверяют сессию.
    pub async fn lookup_hash(
        &self,
        hash: TokenHash,
    ) -> Result<Option<(String, TokenHash)>, StoreError> {
        let (ttl, idle) = (self.ttl_ms, self.idle_ms);
        self.store
            .call(move |conn| {
                let now = now_ms();
                let row: Option<(String, i64, i64)> = conn
                    .query_row(
                        "SELECT username, created_at, last_seen FROM sessions WHERE token_hash = ?1",
                        [hash],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .optional()?;
                let Some((username, created_at, last_seen)) = row else {
                    return Ok(None);
                };
                if now - created_at >= ttl || now - last_seen >= idle {
                    conn.execute("DELETE FROM sessions WHERE token_hash = ?1", [hash])?;
                    return Ok(None);
                }
                if now - last_seen >= TOUCH_INTERVAL_MS {
                    conn.execute(
                        "UPDATE sessions SET last_seen = ?2 WHERE token_hash = ?1",
                        params![hash, now],
                    )?;
                }
                Ok(Some((username, hash)))
            })
            .await
    }

    pub async fn revoke(&self, hash: TokenHash) -> Result<(), StoreError> {
        self.store
            .call(move |conn| conn.execute("DELETE FROM sessions WHERE token_hash = ?1", [hash]))
            .await
            .map(drop)
    }

    /// Завершает остальные сессии и отзывает устройства и незавершённые pairing.
    pub async fn revoke_after_password_change(
        &self,
        username: &str,
        keep: TokenHash,
    ) -> Result<(), StoreError> {
        let username = username.to_owned();
        self.store
            .call(move |conn| {
                let tx = conn.transaction()?;
                let now = now_ms();
                tx.execute(
                    "DELETE FROM sessions WHERE username = ?1 AND token_hash != ?2",
                    params![username, keep],
                )?;
                tx.execute(
                    "UPDATE devices SET revoked_at=?2 WHERE username=?1 AND revoked_at IS NULL",
                    params![username, now],
                )?;
                tx.execute(
                    "UPDATE device_pairings SET replaced_at=?2
                     WHERE username=?1 AND used_at IS NULL AND replaced_at IS NULL",
                    params![username, now],
                )?;
                tx.commit()
            })
            .await
            .map(drop)
    }
}

pub(crate) fn hash_token(token: &str) -> TokenHash {
    Sha256::digest(token.as_bytes()).into()
}

pub fn session_cookie(token: &str, max_age: i64) -> HeaderValue {
    cookie(SESSION_COOKIE, token, max_age)
}

/// Cookie только для HTTPS (и localhost), недоступна из JS и не уходит с чужих сайтов.
pub fn cookie(name: &str, value: &str, max_age: i64) -> HeaderValue {
    HeaderValue::from_str(&format!(
        "{name}={value}; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age={max_age}"
    ))
    .expect("cookie value is ASCII")
}

pub fn read_cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(key, _)| *key == name)
        .map(|(_, value)| value)
}

/// Вошедший пользователь, который всё ещё есть в htpasswd.
pub struct AuthUser {
    pub username: String,
    pub token_hash: TokenHash,
}

#[derive(Clone, Debug)]
pub enum ApiCredential {
    Session(TokenHash),
    Device { id: String, token_hash: TokenHash },
}

/// Учётные данные для маршрутов, которые разрешают и сессию, и устройство.
pub struct ApiUser {
    pub username: String,
    pub credential: ApiCredential,
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, app: &AppState) -> Result<Self, ApiError> {
        if parts.headers.contains_key(AUTHORIZATION) {
            return Err(ApiError::Unauthorized);
        }
        let (username, token_hash) = session_user(app, &parts.headers).await?;
        Ok(Self {
            username,
            token_hash,
        })
    }
}

impl FromRequestParts<AppState> for ApiUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, app: &AppState) -> Result<Self, ApiError> {
        let authorization = parts.headers.get_all(AUTHORIZATION);
        let mut values = authorization.iter();
        if let Some(value) = values.next() {
            if values.next().is_some() {
                return Err(ApiError::Unauthorized);
            }
            let value = value.to_str().map_err(|_| ApiError::Unauthorized)?;
            let bearer = value
                .strip_prefix("Bearer ")
                .ok_or(ApiError::Unauthorized)?;
            let token = bearer.strip_prefix("lgd_").ok_or(ApiError::Unauthorized)?;
            let decoded = URL_SAFE_NO_PAD
                .decode(token)
                .map_err(|_| ApiError::Unauthorized)?;
            if decoded.len() != 32 || URL_SAFE_NO_PAD.encode(&decoded) != token {
                return Err(ApiError::Unauthorized);
            }
            let users = htpasswd_users(app).await?.ok_or(ApiError::Unauthorized)?;
            let token_hash = hash_token(bearer);
            let device = app
                .store
                .authenticate_device(token_hash)
                .await?
                .ok_or(ApiError::Unauthorized)?;
            if !users.contains_key(&device.username) {
                app.store.revoke_user_devices(device.username).await?;
                return Err(ApiError::Unauthorized);
            }
            return Ok(Self {
                username: device.username,
                credential: ApiCredential::Device {
                    id: device.id,
                    token_hash,
                },
            });
        }

        let (username, token_hash) = session_user(app, &parts.headers).await?;
        Ok(Self {
            username,
            credential: ApiCredential::Session(token_hash),
        })
    }
}

async fn session_user(
    app: &AppState,
    headers: &HeaderMap,
) -> Result<(String, TokenHash), ApiError> {
    let token = read_cookie(headers, SESSION_COOKIE).ok_or(ApiError::Unauthorized)?;
    let (username, token_hash) = app
        .sessions
        .lookup(token)
        .await?
        .ok_or(ApiError::Unauthorized)?;
    let users = htpasswd_users(app).await?;
    if !users.is_some_and(|users| users.contains_key(&username)) {
        app.sessions.revoke(token_hash).await?;
        return Err(ApiError::Unauthorized);
    }
    Ok((username, token_hash))
}

pub(crate) async fn htpasswd_users(
    app: &AppState,
) -> Result<Option<Arc<HashMap<String, String>>>, ApiError> {
    let app = app.clone();
    tokio::task::spawn_blocking(move || app.htpasswd.users())
        .await
        .map_err(|_| ApiError::Internal)
}

/// Изменяющие запросы принимаются только со страниц самого dock (защита от CSRF).
pub async fn require_origin(
    State(app): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let safe = matches!(
        *request.method(),
        Method::GET | Method::HEAD | Method::OPTIONS
    );
    let origin = request.headers().get(ORIGIN).and_then(|v| v.to_str().ok());
    let pairing_exchange = *request.method() == Method::POST
        && matches!(request.uri().path(), "/devices/pair" | "/api/devices/pair");
    if !safe
        && !pairing_exchange
        && !request.headers().contains_key(AUTHORIZATION)
        && origin != Some(app.config.public_origin.as_str())
    {
        return Err(ApiError::Forbidden);
    }
    Ok(next.run(request).await)
}

/// Bearer устройства принимается только перечисленными read API и self-revoke.
pub async fn require_device_route_allowlist(
    request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let path = request.uri().path();
    if !path.starts_with("/api/")
        || !request.headers().contains_key(AUTHORIZATION)
        || is_device_route(request.method(), path)
    {
        return Ok(next.run(request).await);
    }
    Err(ApiError::Unauthorized)
}

fn is_device_route(method: &Method, path: &str) -> bool {
    let method = method.as_str();
    (method == "GET"
        && matches!(
            path,
            "/api/me"
                | "/api/logs/stream"
                | "/api/infrastructure"
                | "/api/series"
                | "/api/series/favorites"
                | "/api/series/sources"
        ))
        || (method == "POST"
            && matches!(
                path,
                "/api/logs/search"
                    | "/api/stats/histogram"
                    | "/api/stats/group-by"
                    | "/api/stats/summary"
                    | "/api/issues/search"
                    | "/api/series/lookup"
                    | "/api/series/query"
            ))
        || (method == "DELETE" && path == "/api/devices/current")
        || (method == "GET" && one_segment_after(path, "/api/traces/"))
        || issue_device_route(method, path)
}

fn issue_device_route(method: &str, path: &str) -> bool {
    for (suffix, expected_method) in [
        ("/details", "POST"),
        ("/resolve", "POST"),
        ("/reopen", "POST"),
    ] {
        if method == expected_method
            && path
                .strip_prefix("/api/issues/")
                .and_then(|value| value.strip_suffix(suffix))
                .is_some_and(|id| !id.is_empty() && !id.contains('/'))
        {
            return true;
        }
    }
    false
}

fn one_segment_after(path: &str, prefix: &str) -> bool {
    path.strip_prefix(prefix)
        .is_some_and(|value| !value.is_empty() && !value.contains('/'))
}
