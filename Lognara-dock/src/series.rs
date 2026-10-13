//! Метрики core для браузера: серии через query-токен, определения через admin-токен.
//! Тела запросов передаются core как есть, проверяет их core.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde_json::Value;
use uuid::Uuid;

use crate::api::{ApiError, AppState};
use crate::session::AuthUser;

pub async fn list(
    State(app): State<AppState>,
    _user: AuthUser,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let (status, body) = app.core.series().await?;
    Ok((status, Json(body)))
}

pub async fn query(
    State(app): State<AppState>,
    _user: AuthUser,
    Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let (status, reply) = app.core.series_query(&body).await?;
    Ok((status, Json(reply)))
}

pub async fn definitions(
    State(app): State<AppState>,
    _user: AuthUser,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let (status, body) = app.core.definitions().await?;
    Ok((status, Json(body)))
}

pub async fn create_definition(
    State(app): State<AppState>,
    _user: AuthUser,
    Json(definition): Json<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let (status, body) = app.core.create_definition(&definition).await?;
    Ok((status, Json(body)))
}

pub async fn update_definition(
    State(app): State<AppState>,
    _user: AuthUser,
    Path(id): Path<String>,
    Json(definition): Json<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let (status, body) = app
        .core
        .update_definition(parse_id(&id)?, &definition)
        .await?;
    Ok((status, Json(body)))
}

pub async fn delete_definition(
    State(app): State<AppState>,
    _user: AuthUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let (status, _) = app.core.delete_definition(parse_id(&id)?).await?;
    Ok(status)
}

pub async fn preview(
    State(app): State<AppState>,
    _user: AuthUser,
    Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let (status, reply) = app.core.preview(&body).await?;
    Ok((status, Json(reply)))
}

fn parse_id(raw: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(raw).map_err(|_| ApiError::Invalid("id must be a UUID".into()))
}
