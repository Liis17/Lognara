//! Клиент core: поиск `POST /v1/logs/search` с query-токеном и управление relay и агентами
//! `/v1/admin/*` с admin-токеном.

use std::time::Duration;

use reqwest::Method;
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use url::Url;

use crate::config::Config;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("core request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("core answered {0}")]
    Status(reqwest::StatusCode),
    #[error("core answered without events")]
    Format,
    /// Admin-запрос отклонён; `code` из JSON `{"error": code}` или пустой.
    #[error("core rejected admin request with {0}: {1:?}")]
    Rejected(reqwest::StatusCode, String),
}

pub struct CoreClient {
    http: reqwest::Client,
    search_url: Url,
    admin_url: Url,
    token: String,
    admin_token: String,
}

impl CoreClient {
    pub fn new(config: &Config) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .build()
                .expect("http client"),
            search_url: config
                .core_url
                .join("v1/logs/search")
                .expect("core url joins path"),
            admin_url: config
                .core_url
                .join("v1/admin/")
                .expect("core url joins path"),
            token: config.core_token.clone(),
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
        let response = self
            .http
            .post(self.search_url.clone())
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(CoreError::Status(response.status()));
        }
        let mut reply: Value = response.json().await?;
        match reply.get_mut("events").map(Value::take) {
            Some(Value::Array(events)) => Ok(events),
            _ => Err(CoreError::Format),
        }
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
}
