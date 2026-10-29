use crate::{
    api::{ApiError, AppState},
    session::AuthUser,
};
use axum::{Json, extract::State, http::StatusCode};
use lognara_selflog::Settings;

pub async fn settings(
    State(app): State<AppState>,
    _user: AuthUser,
) -> Result<Json<Settings>, ApiError> {
    Ok(Json(app.core.self_log_settings().await?))
}
pub async fn update(
    State(app): State<AppState>,
    _user: AuthUser,
    Json(value): Json<Settings>,
) -> Result<StatusCode, ApiError> {
    app.core.update_self_log_settings(&value).await?;
    Ok(StatusCode::NO_CONTENT)
}
