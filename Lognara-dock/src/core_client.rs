//! Клиент поиска core: `POST /v1/logs/search` с query-токеном.

use std::time::Duration;

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
}

pub struct CoreClient {
    http: reqwest::Client,
    search_url: Url,
    token: String,
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
            token: config.core_token.clone(),
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
}
