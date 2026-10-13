//! Клиент core: поиск логов и метрики с query-токеном, определения метрик с admin-токеном.

use std::time::Duration;

use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use url::Url;
use uuid::Uuid;

use crate::config::Config;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Сколько символов текста ошибки core передаётся дальше.
const DETAIL_LIMIT: usize = 1000;

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("core request failed: {0}")]
    Request(#[from] reqwest::Error),
    /// `detail` - текст поля `error` из ответа core. В Display его нет.
    #[error("core answered {status}")]
    Status {
        status: StatusCode,
        detail: Option<String>,
    },
    #[error("core answered unexpected data")]
    Format,
    #[error("admin token is not configured")]
    AdminDisabled,
}

/// Успешный ответ core: статус и JSON-тело (`null`, если тела нет).
pub type CoreReply = (StatusCode, Value);

pub struct CoreClient {
    http: reqwest::Client,
    base: Url,
    query_token: String,
    admin_token: Option<String>,
}

impl CoreClient {
    pub fn new(config: &Config) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .build()
                .expect("http client"),
            base: config.core_url.clone(),
            query_token: config.core_token.clone(),
            admin_token: config.core_admin_token.clone(),
        }
    }

    pub fn admin_enabled(&self) -> bool {
        self.admin_token.is_some()
    }

    /// Самые свежие события `[from, to)`, по убыванию времени, как их отдаёт core.
    pub async fn search(
        &self,
        from: OffsetDateTime,
        to: OffsetDateTime,
        limit: usize,
    ) -> Result<Vec<Value>, CoreError> {
        let body = json!({
            "from": from.format(&Rfc3339).expect("RFC3339 time"),
            "to": to.format(&Rfc3339).expect("RFC3339 time"),
            "limit": limit,
        });
        let (_, mut reply) = self
            .call(
                Method::POST,
                "v1/logs/search",
                &self.query_token,
                Some(&body),
            )
            .await?;
        match reply.get_mut("events").map(Value::take) {
            Some(Value::Array(events)) => Ok(events),
            _ => Err(CoreError::Format),
        }
    }

    /// Список метрик: `GET /v1/series`.
    pub async fn series(&self) -> Result<CoreReply, CoreError> {
        self.call(Method::GET, "v1/series", &self.query_token, None)
            .await
    }

    /// Серии за период: `POST /v1/series/query`.
    pub async fn series_query(&self, query: &Value) -> Result<CoreReply, CoreError> {
        self.call(
            Method::POST,
            "v1/series/query",
            &self.query_token,
            Some(query),
        )
        .await
    }

    /// Определения метрик: `GET /v1/admin/series/definitions`.
    pub async fn definitions(&self) -> Result<CoreReply, CoreError> {
        let token = self.admin_token()?;
        self.call(Method::GET, "v1/admin/series/definitions", token, None)
            .await
    }

    /// Новое определение: `POST /v1/admin/series/definitions`.
    pub async fn create_definition(&self, definition: &Value) -> Result<CoreReply, CoreError> {
        let token = self.admin_token()?;
        self.call(
            Method::POST,
            "v1/admin/series/definitions",
            token,
            Some(definition),
        )
        .await
    }

    /// Замена определения: `PUT /v1/admin/series/definitions/{id}`.
    pub async fn update_definition(
        &self,
        id: Uuid,
        definition: &Value,
    ) -> Result<CoreReply, CoreError> {
        let token = self.admin_token()?;
        let path = format!("v1/admin/series/definitions/{id}");
        self.call(Method::PUT, &path, token, Some(definition)).await
    }

    /// Удаление определения: `DELETE /v1/admin/series/definitions/{id}`.
    pub async fn delete_definition(&self, id: Uuid) -> Result<CoreReply, CoreError> {
        let token = self.admin_token()?;
        let path = format!("v1/admin/series/definitions/{id}");
        self.call(Method::DELETE, &path, token, None).await
    }

    /// Проверка определения на выборке: `POST /v1/admin/series/preview`.
    pub async fn preview(&self, request: &Value) -> Result<CoreReply, CoreError> {
        let token = self.admin_token()?;
        self.call(
            Method::POST,
            "v1/admin/series/preview",
            token,
            Some(request),
        )
        .await
    }

    /// Без admin-токена запрос к core не отправляется.
    fn admin_token(&self) -> Result<&str, CoreError> {
        self.admin_token.as_deref().ok_or(CoreError::AdminDisabled)
    }

    async fn call(
        &self,
        method: Method,
        path: &str,
        token: &str,
        body: Option<&Value>,
    ) -> Result<CoreReply, CoreError> {
        let url = self.base.join(path).expect("core url joins path");
        let mut request = self.http.request(method, url).bearer_auth(token);
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().await?;
        let status = response.status();
        if !status.is_success() {
            return Err(status_error(response).await);
        }
        let bytes = response.bytes().await?;
        if bytes.is_empty() {
            return Ok((status, Value::Null));
        }
        let value = serde_json::from_slice(&bytes).map_err(|_| CoreError::Format)?;
        Ok((status, value))
    }
}

/// Ошибка по ответу core с не-2xx статусом. Текст поля `error` сохраняется, но обрезается.
async fn status_error(response: reqwest::Response) -> CoreError {
    let status = response.status();
    let body = response.bytes().await.unwrap_or_default();
    let detail: Option<String> = serde_json::from_slice::<Value>(&body)
        .ok()
        .and_then(|value| {
            value
                .get("error")?
                .as_str()
                .map(|text| text.chars().take(DETAIL_LIMIT).collect())
        });
    CoreError::Status { status, detail }
}
