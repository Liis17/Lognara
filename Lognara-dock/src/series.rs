//! Метрики core: query/admin-прокси и общее избранное в локальном Store.
//! Тела проксируемых запросов передаются core как есть, проверяет их core.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde_json::Value;
use uuid::Uuid;

use crate::api::{ApiError, AppState};
use crate::session::AuthUser;

pub async fn list(
    State(app): State<AppState>,
    Query(query): Query<std::collections::HashMap<String, String>>,
    _user: AuthUser,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let (status, body) = match query.get("group_by").map(String::as_str) {
        None => app.core.series().await?,
        Some("server") => {
            app.core
                .scoped_series(
                    query.get("server").map(String::as_str),
                    query.get("service").map(String::as_str),
                )
                .await?
        }
        _ => return Err(ApiError::Invalid("group_by must be server".into())),
    };
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

pub async fn sources(
    State(app): State<AppState>,
    _user: AuthUser,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let (status, body) = app.core.series_sources().await?;
    Ok((status, Json(body)))
}
pub async fn import_check(
    State(app): State<AppState>,
    _user: AuthUser,
    Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let (status, body) = app.core.import_check(&body).await?;
    Ok((status, Json(body)))
}
pub async fn import_apply(
    State(app): State<AppState>,
    _user: AuthUser,
    Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let (status, body) = app.core.import_apply(&body).await?;
    Ok((status, Json(body)))
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MetricKey {
    pub server: String,
    pub service: String,
    pub name: String,
    pub origin: String,
}
impl MetricKey {
    fn validate(&self) -> Result<(), ApiError> {
        if self.server.len() > 128
            || self.service.is_empty()
            || self.service.len() > 128
            || self.name.is_empty()
            || self.name.len() > 128
            || !matches!(self.origin.as_str(), "counter" | "gauge" | "rule")
        {
            return Err(ApiError::Invalid("invalid metric identity".into()));
        }
        Ok(())
    }
}
pub async fn lookup(
    State(app): State<AppState>,
    _user: AuthUser,
    Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let (status, reply) = app.core.series_lookup(&body).await?;
    Ok((status, Json(reply)))
}
pub async fn favorites(
    State(app): State<AppState>,
    _user: AuthUser,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        serde_json::json!({ "keys": app.store.favorite_metrics().await? }),
    ))
}
pub async fn add_favorite(
    State(app): State<AppState>,
    _user: AuthUser,
    Json(key): Json<MetricKey>,
) -> Result<StatusCode, ApiError> {
    key.validate()?;
    app.store.set_favorite_metric(key, true).await?;
    Ok(StatusCode::NO_CONTENT)
}
pub async fn remove_favorite(
    State(app): State<AppState>,
    _user: AuthUser,
    Json(key): Json<MetricKey>,
) -> Result<StatusCode, ApiError> {
    key.validate()?;
    app.store.set_favorite_metric(key, false).await?;
    Ok(StatusCode::NO_CONTENT)
}
