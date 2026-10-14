//! Клиент core: поиск логов и метрики с query-токеном, определения метрик и управление relay
//! и агентами `/v1/admin/*` с admin-токеном.

use std::time::Duration;

use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use url::Url;
use uuid::Uuid;

use crate::config::Config;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Серия за год на стенде core (examples/series_bench.rs) считалась около 16 с; таймаут core 30 с (LOGNARA_QUERY_TIMEOUT_SECONDS).
const SERIES_QUERY_TIMEOUT: Duration = Duration::from_secs(35);
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
    /// Admin-запрос отклонён; `code` из JSON `{"error": code}` или пустой.
    #[error("core rejected admin request with {0}: {1:?}")]
    Rejected(reqwest::StatusCode, String),
}

/// Успешный ответ core: статус и JSON-тело (`null`, если тела нет).
pub type CoreReply = (StatusCode, Value);

pub struct CoreClient {
    http: reqwest::Client,
    base: Url,
    admin_url: Url,
    query_token: String,
    admin_token: String,
}

impl CoreClient {
    pub fn new(config: &Config) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .build()
                .expect("http client"),
            base: config.core_url.clone(),
            admin_url: config
                .core_url
                .join("v1/admin/")
                .expect("core url joins path"),
            query_token: config.core_token.clone(),
            admin_token: config.core_admin_token.clone(),
        }
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
        self.call_within(
            Method::POST,
            "v1/series/query",
            &self.query_token,
            Some(query),
            SERIES_QUERY_TIMEOUT,
        )
        .await
    }

    /// Определения метрик: `GET /v1/admin/series/definitions`.
    pub async fn definitions(&self) -> Result<CoreReply, CoreError> {
        self.call(
            Method::GET,
            "v1/admin/series/definitions",
            &self.admin_token,
            None,
        )
        .await
    }

    /// Новое определение: `POST /v1/admin/series/definitions`.
    pub async fn create_definition(&self, definition: &Value) -> Result<CoreReply, CoreError> {
        self.call(
            Method::POST,
            "v1/admin/series/definitions",
            &self.admin_token,
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
        let path = format!("v1/admin/series/definitions/{id}");
        self.call(Method::PUT, &path, &self.admin_token, Some(definition))
            .await
    }

    /// Удаление определения: `DELETE /v1/admin/series/definitions/{id}`.
    pub async fn delete_definition(&self, id: Uuid) -> Result<CoreReply, CoreError> {
        let path = format!("v1/admin/series/definitions/{id}");
        self.call(Method::DELETE, &path, &self.admin_token, None)
            .await
    }

    /// Проверка определения на выборке: `POST /v1/admin/series/preview`.
    pub async fn preview(&self, request: &Value) -> Result<CoreReply, CoreError> {
        self.call(
            Method::POST,
            "v1/admin/series/preview",
            &self.admin_token,
            Some(request),
        )
        .await
    }

    /// `GET /v1/admin/relays`: `{"relays": [...]}`.
    pub async fn list_relays(&self) -> Result<Value, CoreError> {
        self.admin(Method::GET, "relays".into(), None).await
    }

    /// `POST /v1/admin/relays`: `{"relay": {...}, "token": "lnr_..."}`.
    pub async fn create_relay(&self, name: &str) -> Result<Value, CoreError> {
        let body = json!({ "name": name });
        self.admin(Method::POST, "relays".into(), Some(body)).await
    }

    pub async fn delete_relay(&self, id: i64) -> Result<(), CoreError> {
        let path = format!("relays/{id}");
        self.admin(Method::DELETE, path, None).await.map(drop)
    }

    /// `GET /v1/admin/agents`: `{"agents": [...]}`.
    pub async fn list_agents(&self) -> Result<Value, CoreError> {
        self.admin(Method::GET, "agents".into(), None).await
    }

    /// `POST /v1/admin/agents`: `{"agent": {...}, "token": "lna_..."}`.
    pub async fn create_agent(&self, name: &str, relay_id: i64) -> Result<Value, CoreError> {
        let body = json!({ "name": name, "relay_id": relay_id });
        self.admin(Method::POST, "agents".into(), Some(body)).await
    }

    pub async fn delete_agent(&self, id: i64) -> Result<(), CoreError> {
        let path = format!("agents/{id}");
        self.admin(Method::DELETE, path, None).await.map(drop)
    }

    /// Ответ core как JSON; `Null` для 204.
    async fn admin(
        &self,
        method: Method,
        path: String,
        body: Option<Value>,
    ) -> Result<Value, CoreError> {
        let url = self.admin_url.join(&path).expect("core url joins path");
        let mut request = self
            .http
            .request(method, url)
            .bearer_auth(&self.admin_token);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await?;
        let status = response.status();
        if !status.is_success() {
            let reply: Value = response.json().await.unwrap_or(Value::Null);
            let code = reply["error"].as_str().unwrap_or_default().to_owned();
            return Err(CoreError::Rejected(status, code));
        }
        if status == reqwest::StatusCode::NO_CONTENT {
            return Ok(Value::Null);
        }
        Ok(response.json().await?)
    }

    async fn call(
        &self,
        method: Method,
        path: &str,
        token: &str,
        body: Option<&Value>,
    ) -> Result<CoreReply, CoreError> {
        self.call_within(method, path, token, body, REQUEST_TIMEOUT)
            .await
    }

    async fn call_within(
        &self,
        method: Method,
        path: &str,
        token: &str,
        body: Option<&Value>,
        timeout: Duration,
    ) -> Result<CoreReply, CoreError> {
        let url = self.base.join(path).expect("core url joins path");
        let mut request = self
            .http
            .request(method, url)
            .bearer_auth(token)
            .timeout(timeout);
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
