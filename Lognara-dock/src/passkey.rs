//! Passkey (WebAuthn): регистрация в настройках и вход без ввода логина.
//!
//! Пользователи по-прежнему задаются htpasswd: passkey удалённого из файла пользователя
//! не открывает сессию.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{Path, State};
use axum::http::header::SET_COOKIE;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{AppendHeaders, IntoResponse};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use webauthn_rs::prelude::{
    DiscoverableAuthentication, DiscoverableKey, Passkey, PasskeyRegistration, PublicKeyCredential,
    RegisterPublicKeyCredential, Url, Uuid, Webauthn, WebauthnBuilder,
};
use webauthn_rs_proto::{AuthenticatorSelectionCriteria, ResidentKeyRequirement};

use crate::account::check_current_password;
use crate::api::{ApiError, AppState};
use crate::auth::start_session;
use crate::config::Config;
use crate::session::{AuthUser, cookie, read_cookie};
use crate::store::{StoreError, now_ms};

pub const CHALLENGE_COOKIE: &str = "lognara_dock_challenge";
const CHALLENGE_TTL: Duration = Duration::from_secs(300);
/// Предел незавершённых церемоний: вход начинается без сессии.
const MAX_CHALLENGES: usize = 1000;
const MAX_NAME_CHARS: usize = 64;

pub fn webauthn(config: &Config) -> Result<Webauthn, String> {
    let origin = Url::parse(&config.public_origin).map_err(|err| err.to_string())?;
    WebauthnBuilder::new(&config.rp_id, &origin)
        .and_then(|builder| builder.rp_name("Lognara").build())
        .map_err(|err| format!("invalid passkey settings: {err}"))
}

/// Состояние церемоний между start и finish, в памяти процесса.
#[derive(Default)]
pub struct Challenges {
    logins: Mutex<HashMap<String, (Instant, DiscoverableAuthentication)>>,
    registrations: Mutex<HashMap<String, (Instant, Registration)>>,
}

struct Registration {
    username: String,
    name: String,
    state: PasskeyRegistration,
}

fn put<T>(map: &Mutex<HashMap<String, (Instant, T)>>, value: T) -> Result<String, ApiError> {
    let mut map = map.lock().unwrap();
    map.retain(|_, (created, _)| created.elapsed() < CHALLENGE_TTL);
    if map.len() >= MAX_CHALLENGES {
        return Err(ApiError::TooManyRequests(CHALLENGE_TTL));
    }
    let mut id = [0u8; 24];
    rand::rng().fill_bytes(&mut id);
    let id = URL_SAFE_NO_PAD.encode(id);
    map.insert(id.clone(), (Instant::now(), value));
    Ok(id)
}

/// Забирает состояние один раз; повтор finish с тем же challenge невозможен.
fn take<T>(map: &Mutex<HashMap<String, (Instant, T)>>, headers: &HeaderMap) -> Result<T, ApiError> {
    let id =
        read_cookie(headers, CHALLENGE_COOKIE).ok_or(ApiError::BadRequest("challenge_expired"))?;
    match map.lock().unwrap().remove(id) {
        Some((created, value)) if created.elapsed() < CHALLENGE_TTL => Ok(value),
        _ => Err(ApiError::BadRequest("challenge_expired")),
    }
}

fn challenge_cookie(id: &str) -> [(axum::http::HeaderName, axum::http::HeaderValue); 1] {
    [(
        SET_COOKIE,
        cookie(CHALLENGE_COOKIE, id, CHALLENGE_TTL.as_secs() as i64),
    )]
}

fn clear_challenge() -> (axum::http::HeaderName, axum::http::HeaderValue) {
    (SET_COOKIE, cookie(CHALLENGE_COOKIE, "", 0))
}

fn rejected(err: impl std::fmt::Display) -> ApiError {
    tracing::warn!("passkey rejected: {err}");
    ApiError::BadRequest("passkey_rejected")
}

pub async fn login_start(State(app): State<AppState>) -> Result<impl IntoResponse, ApiError> {
    let (options, state) = app
        .webauthn
        .start_discoverable_authentication()
        .map_err(|err| {
            tracing::error!("passkey login start failed: {err}");
            ApiError::Internal
        })?;
    let id = put(&app.challenges.logins, state)?;
    Ok((challenge_cookie(&id), Json(options)))
}

pub async fn login_finish(
    State(app): State<AppState>,
    headers: HeaderMap,
    Json(credential): Json<PublicKeyCredential>,
) -> Result<impl IntoResponse, ApiError> {
    let state = take(&app.challenges.logins, &headers)?;
    let (user_id, _) = app
        .webauthn
        .identify_discoverable_authentication(&credential)
        .map_err(rejected)?;
    let found = app
        .store
        .call(move |conn| {
            let Some(username) = conn
                .query_row(
                    "SELECT username FROM users WHERE webauthn_id = ?1",
                    [user_id.as_bytes()],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
            else {
                return Ok(None);
            };
            Ok(Some((username.clone(), load_passkeys(conn, &username)?)))
        })
        .await?;
    let Some((username, passkeys)) = found else {
        return Err(rejected("unknown user handle"));
    };
    if !app.htpasswd.contains(&username) {
        return Err(rejected(format_args!("{username} is not in htpasswd")));
    }
    let keys: Vec<DiscoverableKey> = passkeys.iter().map(|(_, key)| key.into()).collect();
    let result = app
        .webauthn
        .finish_discoverable_authentication(&credential, state, &keys)
        .map_err(rejected)?;

    if let Some((id, mut key)) = passkeys
        .into_iter()
        .find(|(_, key)| key.cred_id() == result.cred_id())
    {
        key.update_credential(&result);
        app.store
            .call(move |conn| {
                conn.execute(
                    "UPDATE passkeys SET passkey = ?2, last_used_at = ?3 WHERE id = ?1",
                    params![id, to_json(&key), now_ms()],
                )
            })
            .await?;
    }
    tracing::info!(%username, "passkey login");
    let session = start_session(&app, username).await?;
    Ok((AppendHeaders([clear_challenge()]), session))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegisterStart {
    name: String,
    password: String,
}

/// Начинает регистрацию. Текущий пароль не даёт украденной сессии привязать свой ключ.
pub async fn register_start(
    State(app): State<AppState>,
    user: AuthUser,
    Json(request): Json<RegisterStart>,
) -> Result<impl IntoResponse, ApiError> {
    let name = request.name.trim().to_owned();
    if name.is_empty() || name.chars().count() > MAX_NAME_CHARS {
        return Err(ApiError::BadRequest("invalid_name"));
    }
    check_current_password(&app, &user, request.password).await?;

    let username = user.username.clone();
    let (user_id, existing) = app
        .store
        .call(move |conn| {
            let user_id = webauthn_id(conn, &username)?;
            Ok((user_id, load_passkeys(conn, &username)?))
        })
        .await?;
    let exclude = existing
        .iter()
        .map(|(_, key)| key.cred_id().clone())
        .collect();
    let (mut options, state) = app
        .webauthn
        .start_passkey_registration(user_id, &user.username, &user.username, Some(exclude))
        .map_err(|err| {
            tracing::error!("passkey registration start failed: {err}");
            ApiError::Internal
        })?;
    // Вход без логина находит только ключи, сохранённые на устройстве (resident key).
    let selection = options
        .public_key
        .authenticator_selection
        .get_or_insert_with(AuthenticatorSelectionCriteria::default);
    selection.resident_key = Some(ResidentKeyRequirement::Required);
    selection.require_resident_key = true;

    let id = put(
        &app.challenges.registrations,
        Registration {
            username: user.username,
            name,
            state,
        },
    )?;
    Ok((challenge_cookie(&id), Json(options)))
}

pub async fn register_finish(
    State(app): State<AppState>,
    user: AuthUser,
    headers: HeaderMap,
    Json(credential): Json<RegisterPublicKeyCredential>,
) -> Result<impl IntoResponse, ApiError> {
    let registration = take(&app.challenges.registrations, &headers)?;
    if registration.username != user.username {
        return Err(ApiError::BadRequest("challenge_expired"));
    }
    let key = app
        .webauthn
        .finish_passkey_registration(&credential, &registration.state)
        .map_err(rejected)?;
    let (username, name) = (user.username.clone(), registration.name);
    let info = app
        .store
        .call(move |conn| {
            let now = now_ms();
            conn.execute(
                "INSERT INTO passkeys (username, credential_id, name, passkey, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![username, key.cred_id().as_ref(), name, to_json(&key), now],
            )?;
            Ok(PasskeyInfo {
                id: conn.last_insert_rowid(),
                name,
                created_at: now,
                last_used_at: None,
            })
        })
        .await
        .map_err(|err| match err {
            StoreError::Sqlite(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                ApiError::BadRequest("passkey_exists")
            }
            err => err.into(),
        })?;
    tracing::info!(username = %user.username, name = %info.name, "passkey registered");
    Ok((AppendHeaders([clear_challenge()]), Json(info)))
}

#[derive(Serialize)]
pub struct PasskeyInfo {
    id: i64,
    name: String,
    /// Миллисекунды Unix.
    created_at: i64,
    last_used_at: Option<i64>,
}

pub async fn list(State(app): State<AppState>, user: AuthUser) -> Result<Json<Value>, ApiError> {
    let passkeys = app
        .store
        .call(move |conn| {
            let mut statement = conn.prepare_cached(
                "SELECT id, name, created_at, last_used_at FROM passkeys
                 WHERE username = ?1 ORDER BY created_at",
            )?;
            statement
                .query_map([user.username], |row| {
                    Ok(PasskeyInfo {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        created_at: row.get(2)?,
                        last_used_at: row.get(3)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .await?;
    Ok(Json(json!({ "passkeys": passkeys })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Confirm {
    password: String,
}

pub async fn delete(
    State(app): State<AppState>,
    user: AuthUser,
    Path(id): Path<i64>,
    Json(confirm): Json<Confirm>,
) -> Result<StatusCode, ApiError> {
    check_current_password(&app, &user, confirm.password).await?;
    let username = user.username.clone();
    let deleted = app
        .store
        .call(move |conn| {
            conn.execute(
                "DELETE FROM passkeys WHERE id = ?1 AND username = ?2",
                params![id, username],
            )
        })
        .await?;
    if deleted == 0 {
        return Err(ApiError::NotFound);
    }
    tracing::info!(username = %user.username, id, "passkey deleted");
    Ok(StatusCode::NO_CONTENT)
}

pub async fn count(app: &AppState, username: &str) -> Result<i64, ApiError> {
    let username = username.to_owned();
    Ok(app
        .store
        .call(move |conn| {
            conn.query_row(
                "SELECT count(*) FROM passkeys WHERE username = ?1",
                [username],
                |row| row.get(0),
            )
        })
        .await?)
}

/// Стабильный user handle пользователя; создаётся при первой регистрации ключа.
fn webauthn_id(conn: &Connection, username: &str) -> rusqlite::Result<Uuid> {
    let existing: Option<Vec<u8>> = conn
        .query_row(
            "SELECT webauthn_id FROM users WHERE username = ?1",
            [username],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(id) = existing.and_then(|bytes| Uuid::from_slice(&bytes).ok()) {
        return Ok(id);
    }
    let id = Uuid::new_v4();
    conn.execute(
        "INSERT INTO users (username, webauthn_id) VALUES (?1, ?2)",
        params![username, id.as_bytes()],
    )?;
    Ok(id)
}

fn load_passkeys(conn: &Connection, username: &str) -> rusqlite::Result<Vec<(i64, Passkey)>> {
    let mut statement =
        conn.prepare_cached("SELECT id, passkey FROM passkeys WHERE username = ?1")?;
    statement
        .query_map([username], |row| {
            let json: String = row.get(1)?;
            let key = serde_json::from_str(&json).map_err(|err| {
                rusqlite::Error::FromSqlConversionFailure(
                    1,
                    rusqlite::types::Type::Text,
                    err.into(),
                )
            })?;
            Ok((row.get(0)?, key))
        })?
        .collect()
}

fn to_json(key: &Passkey) -> String {
    serde_json::to_string(key).expect("passkey serializes")
}
