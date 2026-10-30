//! Browser management for MCP personal tokens and OAuth grants.
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::RngCore;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    api::{ApiError, AppState},
    session::AuthUser,
    store,
};

const TOKEN_LIFETIME_MS: i64 = 30 * 24 * 60 * 60 * 1000;

pub async fn access(State(app): State<AppState>, _user: AuthUser) -> Json<Value> {
    Json(json!({
        "enabled": app.config.mcp_enabled,
        "url": app.config.public_url.join("mcp").expect("valid MCP URL").as_str(),
    }))
}

pub async fn tokens(State(app): State<AppState>, user: AuthUser) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({ "tokens": app.store.personal_tokens(user.username).await? }),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateTokenRequest {
    name: String,
}

pub async fn create_token(
    State(app): State<AppState>,
    user: AuthUser,
    body: Result<Json<CreateTokenRequest>, axum::extract::rejection::JsonRejection>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    if !app.config.mcp_enabled {
        return Err(ApiError::Conflict("mcp_disabled"));
    }
    let Json(body) = body.map_err(|_| ApiError::BadRequest("invalid_request"))?;
    let name = body.name.trim();
    if name.is_empty() || name.len() > 80 || name.chars().any(char::is_control) {
        return Err(ApiError::BadRequest("invalid_request"));
    }
    let secret = random_token();
    let hash: [u8; 32] = Sha256::digest(secret.as_bytes()).into();
    let now = store::now_ms();
    let token = app
        .store
        .create_personal_token(
            Uuid::new_v4().to_string(),
            user.username,
            name.to_owned(),
            hash,
            now + TOKEN_LIFETIME_MS,
        )
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "token": secret, "personal_token": token })),
    ))
}

pub async fn revoke_token(
    State(app): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    Uuid::parse_str(&id).map_err(|_| ApiError::BadRequest("invalid_request"))?;
    if !app.store.revoke_personal_token(user.username, id).await? {
        return Err(ApiError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn grants(State(app): State<AppState>, user: AuthUser) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({ "grants": app.store.oauth_grants(user.username).await? }),
    ))
}

pub async fn revoke_grant(
    State(app): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    Uuid::parse_str(&id).map_err(|_| ApiError::BadRequest("invalid_request"))?;
    if !app.store.revoke_oauth_grant(user.username, id).await? {
        return Err(ApiError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn consent_details(
    State(app): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    if !app.config.mcp_enabled {
        return Err(ApiError::NotFound);
    }
    let pending = app
        .store
        .oauth_pending(id, user.username)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(json!({
        "client_name": pending.client_name,
        "scope": pending.scope,
        "resource": pending.resource,
    })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsentDecision {
    allow: bool,
}

pub async fn consent_decision(
    State(app): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
    body: Result<Json<ConsentDecision>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    if !app.config.mcp_enabled {
        return Err(ApiError::NotFound);
    }
    let Json(body) = body.map_err(|_| ApiError::BadRequest("invalid_request"))?;
    let code = body.allow.then(random_token);
    let code_hash = code
        .as_deref()
        .map(|value| Sha256::digest(value.as_bytes()).into());
    let pending = app
        .store
        .complete_oauth_pending(id, user.username, code_hash)
        .await?
        .ok_or(ApiError::NotFound)?;
    let mut redirect = url::Url::parse(&pending.redirect_uri).map_err(|_| ApiError::Internal)?;
    {
        let mut query = redirect.query_pairs_mut();
        if let Some(code) = code {
            query.append_pair("code", &code);
        } else {
            query.append_pair("error", "access_denied");
            query.append_pair("error_description", "The user denied access");
        }
        query.append_pair("state", &pending.state);
        query.append_pair("iss", &app.config.public_origin);
    }
    Ok(Json(json!({ "redirect_to": redirect.as_str() })))
}

pub fn random_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    format!("lnm_{}", URL_SAFE_NO_PAD.encode(bytes))
}
