//! Relay и агенты: dock проксирует управление учётными данными в admin API core.
//!
//! Admin-токен core остаётся в dock. Токен нового relay или агента core возвращает один
//! раз; dock передаёт его браузеру и нигде не хранит и не логирует.

use axum::Json;
use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::http::header::CACHE_CONTROL;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::Value;

use crate::api::{ApiError, AppState};
use crate::core_client::CoreError;
use crate::session::AuthUser;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewRelay {
    name: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewAgent {
    name: String,
    relay_id: i64,
}

impl From<CoreError> for ApiError {
    /// Известные коды core отдаются как есть, остальное скрывается за 502.
    fn from(err: CoreError) -> Self {
        if let CoreError::Rejected(status, code) = &err {
            match (status.as_u16(), code.as_str()) {
                (400, "invalid_name") => return Self::BadRequest("invalid_name"),
                (400, "invalid_request") => return Self::BadRequest("invalid_request"),
                (404, "relay_not_found") => return Self::RelayNotFound,
                (404, "not_found") => return Self::NotFound,
                (409, "name_taken") => return Self::Conflict("name_taken"),
                (409, "relay_has_agents") => return Self::Conflict("relay_has_agents"),
                _ => {}
            }
        }
        tracing::warn!("core admin request failed: {err}");
        Self::CoreUnavailable
    }
}

pub async fn list_relays(
    State(app): State<AppState>,
    _user: AuthUser,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(app.core.list_relays().await?))
}

pub async fn create_relay(
    State(app): State<AppState>,
    user: AuthUser,
    body: Result<Json<NewRelay>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(relay) = body.map_err(|_| ApiError::BadRequest("invalid_request"))?;
    let created = app.core.create_relay(&relay.name).await?;
    tracing::info!(username = %user.username, "relay created");
    Ok(issued(created))
}

pub async fn delete_relay(
    State(app): State<AppState>,
    user: AuthUser,
    id: Result<Path<i64>, PathRejection>,
) -> Result<StatusCode, ApiError> {
    let Path(id) = id.map_err(|_| ApiError::BadRequest("invalid_request"))?;
    app.core.delete_relay(id).await?;
    tracing::info!(username = %user.username, id, "relay deleted");
    Ok(StatusCode::NO_CONTENT)
}

pub async fn list_agents(
    State(app): State<AppState>,
    _user: AuthUser,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(app.core.list_agents().await?))
}

pub async fn create_agent(
    State(app): State<AppState>,
    user: AuthUser,
    body: Result<Json<NewAgent>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(agent) = body.map_err(|_| ApiError::BadRequest("invalid_request"))?;
    let created = app.core.create_agent(&agent.name, agent.relay_id).await?;
    tracing::info!(username = %user.username, relay_id = agent.relay_id, "agent created");
    Ok(issued(created))
}

pub async fn delete_agent(
    State(app): State<AppState>,
    user: AuthUser,
    id: Result<Path<i64>, PathRejection>,
) -> Result<StatusCode, ApiError> {
    let Path(id) = id.map_err(|_| ApiError::BadRequest("invalid_request"))?;
    app.core.delete_agent(id).await?;
    tracing::info!(username = %user.username, id, "agent deleted");
    Ok(StatusCode::NO_CONTENT)
}

/// Ответ с токеном не должен попадать в кэш.
fn issued(body: Value) -> Response {
    (
        StatusCode::CREATED,
        [(CACHE_CONTROL, "no-store")],
        Json(body),
    )
        .into_response()
}
