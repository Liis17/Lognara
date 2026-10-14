//! Сессии в cookie: в SQLite хранится только SHA-256 токена.

use axum::extract::{FromRequestParts, Request, State};
use axum::http::header::{COOKIE, ORIGIN};
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

    /// Удаляет все сессии пользователя, кроме `keep`.
    pub async fn revoke_others(&self, username: &str, keep: TokenHash) -> Result<(), StoreError> {
        let username = username.to_owned();
        self.store
            .call(move |conn| {
                conn.execute(
                    "DELETE FROM sessions WHERE username = ?1 AND token_hash != ?2",
                    params![username, keep],
                )
            })
            .await
            .map(drop)
    }
}

fn hash_token(token: &str) -> TokenHash {
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

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, app: &AppState) -> Result<Self, ApiError> {
        let token = read_cookie(&parts.headers, SESSION_COOKIE).ok_or(ApiError::Unauthorized)?;
        let (username, token_hash) = app
            .sessions
            .lookup(token)
            .await?
            .ok_or(ApiError::Unauthorized)?;
        if !app.htpasswd.contains(&username) {
            app.sessions.revoke(token_hash).await?;
            return Err(ApiError::Unauthorized);
        }
        Ok(Self {
            username,
            token_hash,
        })
    }
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
    if !safe && origin != Some(app.config.public_origin.as_str()) {
        return Err(ApiError::Forbidden);
    }
    Ok(next.run(request).await)
}
