//! Одноразовое подключение мобильных устройств и отзыв их credential.

use axum::Json;
use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::api::{ApiError, AppState};
use crate::session::{ApiCredential, ApiUser, AuthUser, hash_token};
use crate::store::{DevicePairingInfo, PairDeviceOutcome, now_ms};

const PAIRING_TTL_MS: i64 = 120_000;

#[derive(Serialize)]
struct PairingPayload<'a> {
    v: u8,
    url: &'a str,
    code: &'a str,
}

#[derive(Serialize)]
pub(crate) struct PairingCreated {
    pairing_id: String,
    payload: String,
    expires_at: i64,
}

pub(crate) async fn create_pairing(
    State(app): State<AppState>,
    user: AuthUser,
) -> Result<(StatusCode, Json<PairingCreated>), ApiError> {
    let mut random = [0u8; 32];
    rand::rng().fill_bytes(&mut random);
    let code = URL_SAFE_NO_PAD.encode(random);
    let pairing_id = Uuid::new_v4().to_string();
    let created_at = now_ms();
    let expires_at = created_at + PAIRING_TTL_MS;
    app.store
        .create_device_pairing(
            pairing_id.clone(),
            user.username,
            hash_token(&code),
            created_at,
            expires_at,
        )
        .await?;

    let url = app.config.public_url.as_str().trim_end_matches('/');
    let payload = URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&PairingPayload {
            v: 1,
            url,
            code: &code,
        })
        .expect("pairing payload serializes"),
    );
    Ok((
        StatusCode::CREATED,
        Json(PairingCreated {
            pairing_id,
            payload,
            expires_at,
        }),
    ))
}

#[derive(Serialize)]
pub(crate) struct PairingStatus {
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    device_id: Option<String>,
}

pub(crate) async fn pairing_status(
    State(app): State<AppState>,
    user: AuthUser,
    path: Result<Path<String>, PathRejection>,
) -> Result<Json<PairingStatus>, ApiError> {
    let Path(id) = path.map_err(|_| ApiError::BadRequest("invalid_request"))?;
    let pairing = app
        .store
        .device_pairing(id, user.username)
        .await?
        .ok_or(ApiError::NotFound)?;
    let (status, device_id) = pairing_status_value(&pairing, now_ms());
    Ok(Json(PairingStatus { status, device_id }))
}

fn pairing_status_value(info: &DevicePairingInfo, now: i64) -> (&'static str, Option<String>) {
    if info.replaced_at.is_some() {
        ("replaced", None)
    } else if info.used_at.is_some() {
        ("used", info.device_id.clone())
    } else if info.expires_at <= now {
        ("expired", None)
    } else {
        ("pending", None)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PairRequest {
    code: String,
    name: String,
}

#[derive(Serialize)]
pub(crate) struct PairedDevice {
    token: String,
    device_id: String,
    username: String,
}

pub(crate) async fn pair(
    State(app): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<PairRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<PairedDevice>), ApiError> {
    let limiter_key = pairing_limiter_key(&headers);
    if let Some(wait) = app.device_pairing_limiter.locked_for(&limiter_key) {
        return Err(ApiError::TooManyRequests(wait));
    }
    let Json(request) = body.map_err(|_| ApiError::BadRequest("invalid_request"))?;
    let name = request.name.trim();
    let name_len = name.chars().count();
    if !(1..=64).contains(&name_len) || name.chars().any(char::is_control) {
        return Err(ApiError::BadRequest("invalid_request"));
    }

    let mut random = [0u8; 32];
    rand::rng().fill_bytes(&mut random);
    let token = format!("lgd_{}", URL_SAFE_NO_PAD.encode(random));
    let token_hash = hash_token(&token);
    let device_id = Uuid::new_v4().to_string();
    let htpasswd = app.clone();
    let users = tokio::task::spawn_blocking(move || htpasswd.htpasswd.users())
        .await
        .map_err(|_| ApiError::Internal)?;
    let result = app
        .store
        .pair_device(
            hash_token(&request.code),
            device_id.clone(),
            name.to_owned(),
            token_hash,
            users,
        )
        .await?;
    match result {
        PairDeviceOutcome::Created(username) => {
            app.device_pairing_limiter.success(&limiter_key);
            Ok((
                StatusCode::CREATED,
                Json(PairedDevice {
                    token,
                    device_id,
                    username,
                }),
            ))
        }
        PairDeviceOutcome::InvalidCode => {
            app.device_pairing_limiter.failure(&limiter_key);
            Err(ApiError::InvalidCode)
        }
        PairDeviceOutcome::DeviceLimit => {
            app.device_pairing_limiter.success(&limiter_key);
            Err(ApiError::Conflict("device_limit"))
        }
    }
}

fn pairing_limiter_key(headers: &HeaderMap) -> String {
    let ip = headers
        .get("x-real-ip")
        .and_then(|value| value.to_str().ok())
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 64
                && value.bytes().all(|byte| byte.is_ascii_graphic())
        })
        .unwrap_or("unknown");
    format!("device-pair:{ip}")
}

pub(crate) async fn list(
    State(app): State<AppState>,
    user: AuthUser,
) -> Result<Json<Value>, ApiError> {
    let devices = app.store.devices(user.username).await?;
    Ok(Json(serde_json::json!({ "devices": devices })))
}

pub(crate) async fn revoke(
    State(app): State<AppState>,
    user: AuthUser,
    path: Result<Path<String>, PathRejection>,
) -> Result<StatusCode, ApiError> {
    let Path(id) = path.map_err(|_| ApiError::BadRequest("invalid_request"))?;
    if !app.store.revoke_device(user.username, id).await? {
        return Err(ApiError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn revoke_current(
    State(app): State<AppState>,
    user: ApiUser,
) -> Result<StatusCode, ApiError> {
    let ApiCredential::Device { id, token_hash } = user.credential else {
        return Err(ApiError::Unauthorized);
    };
    app.store.revoke_current_device(id, token_hash).await?;
    Ok(StatusCode::NO_CONTENT)
}
