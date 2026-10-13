//! Данные вошедшего пользователя.

use axum::Json;
use serde_json::{Value, json};

use crate::session::AuthUser;

pub async fn me(user: AuthUser) -> Json<Value> {
    Json(json!({ "username": user.username }))
}
