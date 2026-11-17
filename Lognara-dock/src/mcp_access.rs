//! Browser management for MCP personal tokens and OAuth grants.
use std::collections::BTreeMap;

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
    token_access::{Permissions, Rights, Scope},
};

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

fn token_lifetime_ms(days: Option<u16>) -> Option<i64> {
    match days.unwrap_or(30) {
        7 => Some(7 * DAY_MS),
        30 => Some(30 * DAY_MS),
        90 => Some(90 * DAY_MS),
        _ => None,
    }
}

/// Момент истечения токена; бессрочный токен хранится с `store::NEVER_EXPIRES`.
fn token_expires_at(now: i64, days: Option<u16>, never_expires: bool) -> Option<i64> {
    match (never_expires, days) {
        (true, None) => Some(store::NEVER_EXPIRES),
        (true, Some(_)) => None,
        (false, days) => token_lifetime_ms(days).map(|lifetime| now + lifetime),
    }
}

pub async fn access(State(app): State<AppState>, _user: AuthUser) -> Json<Value> {
    Json(json!({
        "enabled": app.config.mcp_enabled,
        "url": app.config.public_url.join("mcp").expect("valid MCP URL").as_str(),
    }))
}

pub async fn tokens(State(app): State<AppState>, user: AuthUser) -> Result<Json<Value>, ApiError> {
    let names = node_names(&app).await;
    let tokens = app.store.personal_tokens(user.username).await?;
    let tokens = tokens
        .iter()
        .map(|token| with_node_name(&names, token, token.permissions.node_scope))
        .collect::<Vec<_>>();
    Ok(Json(json!({ "tokens": tokens })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateTokenRequest {
    name: String,
    logs: Option<bool>,
    metrics: Option<bool>,
    alerts_write: Option<bool>,
    node_scope: Option<Scope>,
    expires_in_days: Option<u16>,
    #[serde(default)]
    never_expires: bool,
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
    let permissions = permissions(body.logs, body.metrics, body.alerts_write, body.node_scope)
        .ok_or(ApiError::BadRequest("invalid_request"))?;
    ensure_node_exists(&app, permissions.node_scope).await?;
    let secret = random_token();
    let hash: [u8; 32] = Sha256::digest(secret.as_bytes()).into();
    let now = store::now_ms();
    let expires_at = token_expires_at(now, body.expires_in_days, body.never_expires)
        .ok_or(ApiError::BadRequest("invalid_request"))?;
    let token = app
        .store
        .create_personal_token(
            Uuid::new_v4().to_string(),
            user.username,
            name.to_owned(),
            hash,
            expires_at,
            permissions,
        )
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "token": secret, "personal_token": token })),
    ))
}

#[cfg(test)]
mod tests {
    use super::{DAY_MS, token_expires_at, token_lifetime_ms};
    use crate::store::NEVER_EXPIRES;

    #[test]
    fn token_lifetime_defaults_to_thirty_days_and_accepts_supported_values() {
        assert_eq!(token_lifetime_ms(None), Some(30 * DAY_MS));
        assert_eq!(token_lifetime_ms(Some(7)), Some(7 * DAY_MS));
        assert_eq!(token_lifetime_ms(Some(30)), Some(30 * DAY_MS));
        assert_eq!(token_lifetime_ms(Some(90)), Some(90 * DAY_MS));
    }

    #[test]
    fn never_expiring_token_uses_sentinel_and_rejects_explicit_days() {
        assert_eq!(token_expires_at(1_000, None, true), Some(NEVER_EXPIRES));
        assert_eq!(token_expires_at(1_000, Some(30), true), None);
        assert_eq!(
            token_expires_at(1_000, Some(7), false),
            Some(1_000 + 7 * DAY_MS)
        );
        assert_eq!(token_expires_at(1_000, Some(14), false), None);
    }

    #[test]
    fn token_lifetime_rejects_unsupported_values() {
        assert_eq!(token_lifetime_ms(Some(0)), None);
        assert_eq!(token_lifetime_ms(Some(14)), None);
        assert_eq!(token_lifetime_ms(Some(365)), None);
    }
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
    let names = node_names(&app).await;
    let grants = app.store.oauth_grants(user.username).await?;
    let grants = grants
        .iter()
        .map(|grant| with_node_name(&names, grant, grant.permissions.node_scope))
        .collect::<Vec<_>>();
    Ok(Json(json!({ "grants": grants })))
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
    // Выбор прав и узла делает пользователь; relay и агентов форма берёт из /api/relays и /api/agents.
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
    logs: Option<bool>,
    metrics: Option<bool>,
    alerts_write: Option<bool>,
    node_scope: Option<Scope>,
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
    let permissions = if body.allow {
        let permissions = permissions(body.logs, body.metrics, body.alerts_write, body.node_scope)
            .ok_or(ApiError::BadRequest("invalid_request"))?;
        ensure_node_exists(&app, permissions.node_scope).await?;
        Some(permissions)
    } else {
        None
    };
    let code = permissions.map(|_| random_token());
    let code_hash: Option<[u8; 32]> = code
        .as_deref()
        .map(|value| Sha256::digest(value.as_bytes()).into());
    let decision = permissions
        .zip(code_hash)
        .map(|(permissions, hash)| (hash, permissions));
    let pending = app
        .store
        .complete_oauth_pending(id, user.username, decision)
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

/// Права из тела запроса. Без полей выдаются прежние права: логи и метрики, без записи, весь узел.
pub fn permissions(
    logs: Option<bool>,
    metrics: Option<bool>,
    alerts_write: Option<bool>,
    node_scope: Option<Scope>,
) -> Option<Permissions> {
    let rights = Rights {
        logs: logs.unwrap_or(true),
        metrics: metrics.unwrap_or(true),
        alerts_write: alerts_write.unwrap_or(false),
    };
    let node_scope = node_scope.unwrap_or_default();
    (rights.is_valid() && node_scope.is_valid()).then_some(Permissions { rights, node_scope })
}

/// Relay или агент из скоупа должны существовать в core; иначе запрос отклоняется.
async fn ensure_node_exists(app: &AppState, scope: Scope) -> Result<(), ApiError> {
    let (reply, list, id) = match scope {
        Scope::All => return Ok(()),
        Scope::Relay { id } => (app.core.list_relays().await?, "relays", id),
        Scope::Agent { id } => (app.core.list_agents().await?, "agents", id),
    };
    let exists = reply[list]
        .as_array()
        .is_some_and(|items| items.iter().any(|item| item["id"].as_i64() == Some(id)));
    if exists {
        Ok(())
    } else {
        Err(ApiError::BadRequest("invalid_scope"))
    }
}

/// Имена relay и агентов для подписей в списках. Если core недоступен, подписей нет, сами токены остаются.
async fn node_names(app: &AppState) -> BTreeMap<String, String> {
    let mut names = BTreeMap::new();
    if let Ok(reply) = app.core.list_relays().await {
        for relay in reply["relays"].as_array().into_iter().flatten() {
            if let (Some(id), Some(name)) = (relay["id"].as_i64(), relay["name"].as_str()) {
                names.insert(format!("relay:{id}"), name.to_owned());
            }
        }
    }
    if let Ok(reply) = app.core.list_agents().await {
        for agent in reply["agents"].as_array().into_iter().flatten() {
            if let (Some(id), Some(name)) = (agent["id"].as_i64(), agent["name"].as_str()) {
                names.insert(format!("agent:{id}"), name.to_owned());
            }
        }
    }
    names
}

/// Запись токена или гранта с полем `node_name`, если имя узла известно.
fn with_node_name<T: serde::Serialize>(
    names: &BTreeMap<String, String>,
    record: &T,
    scope: Scope,
) -> Value {
    let mut value = serde_json::to_value(record).unwrap_or(Value::Null);
    let key = match scope {
        Scope::All => None,
        Scope::Relay { id } => Some(format!("relay:{id}")),
        Scope::Agent { id } => Some(format!("agent:{id}")),
    };
    value["node_name"] = key
        .and_then(|key| names.get(&key).cloned())
        .map_or(Value::Null, Value::String);
    value
}

pub fn random_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    format!("lnm_{}", URL_SAFE_NO_PAD.encode(bytes))
}
