//! Настройки вошедшего пользователя: данные аккаунта и смена пароля.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::api::{ApiError, AppState};
use crate::auth::check_password;
use crate::htpasswd::UpdateError;
use crate::session::AuthUser;

/// bcrypt учитывает только первые 72 байта; длиннее пароль не принимается.
const MAX_NEW_PASSWORD_BYTES: usize = 71;

pub async fn me(State(app): State<AppState>, user: AuthUser) -> Result<Json<Value>, ApiError> {
    let passkeys = crate::passkey::count(&app, &user.username).await?;
    Ok(Json(json!({
        "username": user.username,
        "passkeys": passkeys,
    })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PasswordChange {
    current: String,
    new: String,
}

/// Проверяет текущий пароль, переписывает строку в htpasswd и завершает другие сессии.
pub async fn change_password(
    State(app): State<AppState>,
    user: AuthUser,
    Json(change): Json<PasswordChange>,
) -> Result<StatusCode, ApiError> {
    if change.new.chars().count() < app.config.password_min_length {
        return Err(ApiError::BadRequest("password_too_short"));
    }
    if change.new.len() > MAX_NEW_PASSWORD_BYTES {
        return Err(ApiError::BadRequest("password_too_long"));
    }
    check_current_password(&app, &user, change.current).await?;

    let writer = app.clone();
    let (username, cost) = (user.username.clone(), app.config.bcrypt_cost);
    tokio::task::spawn_blocking(move || writer.htpasswd.set_password(&username, &change.new, cost))
        .await
        .map_err(|_| ApiError::Internal)?
        .map_err(|err| match err {
            UpdateError::UnknownUser => ApiError::Unauthorized,
            err => {
                tracing::error!("password change failed: {err}");
                ApiError::Internal
            }
        })?;
    app.sessions
        .revoke_others(&user.username, user.token_hash)
        .await?;
    tracing::info!(username = %user.username, "password changed");
    Ok(StatusCode::NO_CONTENT)
}

/// Подтверждение действия текущим паролем; ошибка не завершает сессию.
pub async fn check_current_password(
    app: &AppState,
    user: &AuthUser,
    password: String,
) -> Result<(), ApiError> {
    check_password(app, &user.username, password)
        .await
        .map_err(|err| match err {
            ApiError::Unauthorized => ApiError::BadRequest("invalid_current_password"),
            err => err,
        })
}
