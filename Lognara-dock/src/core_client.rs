//! Клиент core: поиск логов и метрики с query-токеном, определения метрик и управление relay
//! и агентами `/v1/admin/*` с admin-токеном.

use std::time::Duration;

use reqwest::{Method, RequestBuilder, StatusCode};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use url::Url;
use uuid::Uuid;

use crate::config::Config;
use crate::infra::CoreStatus;
use crate::issues::{
    CoreSearchRequest, DetailsRequest as IssueDetailsRequest, IssueDetails, IssueResponse,
    RecurrenceRequest, RecurrenceResponse,
};
use crate::patterns::{DetailsRequest, DetailsResponse, PatternRequest, PatternResponse};
use crate::query::{
    DurationStats, GroupRequest, GroupResponse, HistogramRequest, HistogramResponse, SearchRequest,
    SearchResponse, SummaryRequest, SummaryResponse, TraceParams,
};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Core по умолчанию выполняет запрос до 30 секунд.
const QUERY_TIMEOUT: Duration = Duration::from_secs(35);

#[derive(Debug, thiserror::Error)]
pub enum CoreQueryError {
    #[error("invalid core query")]
    Invalid,
    #[error("cursor snapshot expired")]
    Gone,
    #[error("core query unavailable")]
    Unavailable,
    #[error("core query timed out")]
    Timeout,
    #[error("core query capacity is exhausted")]
    Busy,
    #[error("unexpected core response")]
    Unexpected,
    #[error("not found in range")]
    NotFound,
    #[error("pattern index is not ready")]
    NotReady,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct LogSourcesRequest {
    pub field: String,
    pub filters: LogSourceFilters,
    pub limit: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct LogSourceFilters {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct LogSourcesPage {
    pub values: Vec<String>,
    pub next_cursor: Option<String>,
    pub as_of: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AlertSeriesQuery {
    pub server: String,
    pub service: String,
    pub name: String,
    pub origin: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    pub windows_seconds: Vec<u64>,
    pub need_increase: bool,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct AlertSeriesReply {
    pub as_of: String,
    pub last_point_at: Option<String>,
    pub windows: Vec<AlertSeriesWindow>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct AlertSeriesWindow {
    pub seconds: u64,
    pub count: u64,
    pub sum: f64,
    pub avg: Option<f64>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub increase: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertSeriesStatus {
    Busy,
    Timeout,
    Unavailable,
    Misconfigured,
}

impl From<reqwest::Error> for CoreQueryError {
    fn from(error: reqwest::Error) -> Self {
        if error.is_timeout() {
            Self::Timeout
        } else {
            Self::Unexpected
        }
    }
}
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
    search_url: Url,
    query_url: Url,
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
            search_url: config
                .core_url
                .join("v1/logs/search")
                .expect("core url joins path"),
            query_url: config.core_url.join("v1/").expect("core url joins path"),
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

    /// Исторический поиск с фильтрами, текстом и курсором; порядок core сохраняется.
    pub async fn search_page(
        &self,
        request: &SearchRequest,
    ) -> Result<SearchResponse, CoreQueryError> {
        let reply: SearchResponse = self
            .query(self.http.post(self.search_url.clone()).json(request))
            .await?;
        if reply.events.len() > request.limit {
            return Err(CoreQueryError::Unexpected);
        }
        Ok(reply)
    }

    /// Связанные логи trace по возрастанию времени, без модели spans.
    pub async fn trace(
        &self,
        trace_id: &str,
        params: &TraceParams,
    ) -> Result<SearchResponse, CoreQueryError> {
        let mut url = self.query_url.join("traces/").expect("core url joins path");
        url.path_segments_mut()
            .expect("core url has path segments")
            .pop_if_empty()
            .push(trace_id);
        let reply: SearchResponse = self.query(self.http.get(url).query(params)).await?;
        if reply.events.len() > params.limit {
            return Err(CoreQueryError::Unexpected);
        }
        Ok(reply)
    }

    /// Exact, paginated distinct names found in retained log sources.
    pub async fn log_sources(
        &self,
        request: &LogSourcesRequest,
    ) -> Result<LogSourcesPage, CoreQueryError> {
        let url = self
            .query_url
            .join("logs/sources")
            .expect("core url joins path");
        let reply: LogSourcesPage = self.query(self.http.post(url).json(request)).await?;
        if reply.values.len() > request.limit {
            return Err(CoreQueryError::Unexpected);
        }
        Ok(reply)
    }

    pub async fn histogram(
        &self,
        request: &HistogramRequest,
    ) -> Result<HistogramResponse, CoreQueryError> {
        let url = self
            .query_url
            .join("stats/histogram")
            .expect("core url joins path");
        let reply: HistogramResponse = self.query(self.http.post(url).json(request)).await?;
        if reply.buckets.len() > 10_000 {
            return Err(CoreQueryError::Unexpected);
        }
        Ok(reply)
    }

    pub async fn group_by(&self, request: &GroupRequest) -> Result<GroupResponse, CoreQueryError> {
        let url = self
            .query_url
            .join("stats/group-by")
            .expect("core url joins path");
        let reply: GroupResponse = self.query(self.http.post(url).json(request)).await?;
        if reply.groups.len() > request.limit
            || reply.groups.iter().any(|group| {
                group.values.len() != request.group_by.len()
                    || !request
                        .group_by
                        .iter()
                        .all(|field| group.values.contains_key(field))
            })
        {
            return Err(CoreQueryError::Unexpected);
        }
        Ok(reply)
    }

    /// Агрегаты и необязательная первая страница одного снимка core.
    pub async fn summary(
        &self,
        request: &SummaryRequest,
    ) -> Result<SummaryResponse, CoreQueryError> {
        let url = self
            .query_url
            .join("stats/summary")
            .expect("core url joins path");
        let reply: SummaryResponse = self.query(self.http.post(url).json(request)).await?;
        let valid_page = match (request.search_limit, &reply.page) {
            (Some(limit), Some(page)) => page.events.len() <= limit && page.as_of == reply.as_of,
            (None, None) => true,
            _ => false,
        };
        let bucket_total = reply
            .buckets
            .iter()
            .try_fold(0_u64, |total, bucket| total.checked_add(bucket.count));
        if reply.buckets.len() > 10_000
            || reply.error_services.len() > 5
            || reply.actions.len() > 20
            || !valid_page
            || reply.levels.total() != Some(reply.total)
            || bucket_total != Some(reply.total)
            || reply
                .buckets
                .iter()
                .any(|bucket| bucket.levels.total() != Some(bucket.count))
            || !valid_duration(&reply.duration, reply.total)
            || reply
                .error_services
                .iter()
                .any(|service| service.count > reply.levels.error)
            || reply.actions.iter().any(|action| {
                action.count > reply.total || !valid_duration(&action.duration, action.count)
            })
        {
            return Err(CoreQueryError::Unexpected);
        }
        Ok(reply)
    }

    /// Паттерны обработанного снимка; скрытые ID не должны вернуться в списке.
    pub async fn patterns(
        &self,
        request: &PatternRequest,
    ) -> Result<PatternResponse, CoreQueryError> {
        let url = self
            .query_url
            .join("patterns/search")
            .expect("core url joins path");
        let reply: PatternResponse = self.query(self.http.post(url).json(request)).await?;
        if !reply.valid(request.limit)
            || reply
                .patterns
                .iter()
                .any(|pattern| request.exclude_patterns.contains(&pattern.pattern_id))
        {
            return Err(CoreQueryError::Unexpected);
        }
        Ok(reply)
    }

    pub async fn pattern_details(
        &self,
        pattern_id: &str,
        request: &DetailsRequest,
    ) -> Result<DetailsResponse, CoreQueryError> {
        let mut url = self
            .query_url
            .join("patterns/")
            .expect("core url joins path");
        url.path_segments_mut()
            .expect("core url has path segments")
            .pop_if_empty()
            .push(pattern_id)
            .push("details");
        let reply: DetailsResponse = self.query(self.http.post(url).json(request)).await?;
        if !reply.valid(pattern_id) {
            return Err(CoreQueryError::Unexpected);
        }
        Ok(reply)
    }

    /// Проблемы выбранного статуса; core получает статусы Dock в запросе.
    pub async fn issues(
        &self,
        request: &CoreSearchRequest,
    ) -> Result<IssueResponse, CoreQueryError> {
        let url = self
            .query_url
            .join("issues/search")
            .expect("core url joins path");
        let reply: IssueResponse = self.query(self.http.post(url).json(request)).await?;
        if !reply.valid(request) {
            return Err(CoreQueryError::Unexpected);
        }
        Ok(reply)
    }

    pub async fn issue_details(
        &self,
        fingerprint: &str,
        request: &IssueDetailsRequest,
    ) -> Result<IssueDetails, CoreQueryError> {
        let mut url = self.query_url.join("issues/").expect("core url joins path");
        url.path_segments_mut()
            .expect("core url has path segments")
            .pop_if_empty()
            .push(fingerprint)
            .push("details");
        let reply: IssueDetails = self.query(self.http.post(url).json(request)).await?;
        if !reply.valid(fingerprint) {
            return Err(CoreQueryError::Unexpected);
        }
        Ok(reply)
    }

    /// Случаи решённых проблем после checkpoint наблюдателя, по одному сегменту core.
    pub async fn issue_recurrences(
        &self,
        request: &RecurrenceRequest,
    ) -> Result<RecurrenceResponse, CoreQueryError> {
        let url = self
            .query_url
            .join("issues/recurrences")
            .expect("core url joins path");
        let reply: RecurrenceResponse = self.query(self.http.post(url).json(request)).await?;
        if !reply.valid(request) {
            return Err(CoreQueryError::Unexpected);
        }
        Ok(reply)
    }

    /// Версия, lag публикации и хранилище core; доступно и до готовности хранилища.
    pub async fn status(&self) -> Result<CoreStatus, CoreQueryError> {
        let url = self.query_url.join("status").expect("core url joins path");
        let request = self.http.get(url).timeout(REQUEST_TIMEOUT);
        let response = request.bearer_auth(&self.query_token).send().await?;
        if response.status() != StatusCode::OK {
            return Err(CoreQueryError::Unavailable);
        }
        Ok(response.json().await?)
    }

    /// Передаёт только типизированный успешный ответ и статическую классификацию ошибок.
    async fn query<T: DeserializeOwned>(
        &self,
        request: RequestBuilder,
    ) -> Result<T, CoreQueryError> {
        let response = request
            .timeout(QUERY_TIMEOUT)
            .bearer_auth(&self.query_token)
            .send()
            .await?;
        match response.status() {
            StatusCode::OK => Ok(response.json().await?),
            StatusCode::BAD_REQUEST => Err(CoreQueryError::Invalid),
            StatusCode::GONE => Err(CoreQueryError::Gone),
            StatusCode::NOT_FOUND => Err(CoreQueryError::NotFound),
            StatusCode::SERVICE_UNAVAILABLE => {
                // Только неготовый каталог паттернов отличается от общей недоступности.
                let reply: Value = response.json().await.unwrap_or(Value::Null);
                match reply["error"].as_str() {
                    Some("pattern index is not ready") => Err(CoreQueryError::NotReady),
                    Some("query capacity is exhausted") => Err(CoreQueryError::Busy),
                    _ => Err(CoreQueryError::Unavailable),
                }
            }
            StatusCode::GATEWAY_TIMEOUT => Err(CoreQueryError::Timeout),
            _ => Err(CoreQueryError::Unexpected),
        }
    }

    /// Список метрик: `GET /v1/series`.
    pub async fn series(&self) -> Result<CoreReply, CoreError> {
        self.call(Method::GET, "v1/series", &self.query_token, None)
            .await
    }

    pub async fn scoped_series(
        &self,
        server: Option<&str>,
        service: Option<&str>,
    ) -> Result<CoreReply, CoreError> {
        let mut url = Url::parse("http://core/v1/series?group_by=server").expect("static URL");
        if let Some(server) = server {
            url.query_pairs_mut().append_pair("server", server);
        }
        if let Some(service) = service {
            url.query_pairs_mut().append_pair("service", service);
        }
        let path = format!("v1/series?{}", url.query().unwrap());
        self.call(Method::GET, &path, &self.query_token, None).await
    }
    pub async fn series_lookup(&self, body: &Value) -> Result<CoreReply, CoreError> {
        self.call(
            Method::POST,
            "v1/series/lookup",
            &self.query_token,
            Some(body),
        )
        .await
    }
    pub async fn series_sources(&self) -> Result<CoreReply, CoreError> {
        self.call(Method::GET, "v1/series/sources", &self.query_token, None)
            .await
    }
    pub async fn import_check(&self, body: &Value) -> Result<CoreReply, CoreError> {
        self.call(
            Method::POST,
            "v1/admin/series/import/check",
            &self.admin_token,
            Some(body),
        )
        .await
    }
    pub async fn import_apply(&self, body: &Value) -> Result<CoreReply, CoreError> {
        self.call(
            Method::POST,
            "v1/admin/series/import/apply",
            &self.admin_token,
            Some(body),
        )
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

    /// Exact raw aggregates for metric alerts. Capacity is managed by Core's dedicated slot.
    pub async fn alert_series_query(
        &self,
        query: &AlertSeriesQuery,
    ) -> Result<AlertSeriesReply, AlertSeriesStatus> {
        let url = self
            .query_url
            .join("series/alerts/query")
            .expect("core url joins path");
        let response = self
            .http
            .post(url)
            .bearer_auth(&self.query_token)
            .timeout(SERIES_QUERY_TIMEOUT)
            .json(query)
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() {
                    AlertSeriesStatus::Timeout
                } else {
                    AlertSeriesStatus::Unavailable
                }
            })?;
        let status = response.status();
        let bytes = response.bytes().await.map_err(|error| {
            if error.is_timeout() {
                AlertSeriesStatus::Timeout
            } else {
                AlertSeriesStatus::Unavailable
            }
        })?;
        if !status.is_success() {
            let reply: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            return Err(classify_alert_series_status(status, &reply));
        }
        let reply: AlertSeriesReply =
            serde_json::from_slice(&bytes).map_err(|_| AlertSeriesStatus::Misconfigured)?;
        let mut returned: Vec<_> = reply.windows.iter().map(|window| window.seconds).collect();
        let mut expected = query.windows_seconds.clone();
        returned.sort_unstable();
        expected.sort_unstable();
        if returned != expected
            || reply.windows.iter().any(|window| {
                !window.sum.is_finite()
                    || window.avg.is_some_and(|value| !value.is_finite())
                    || window.min.is_some_and(|value| !value.is_finite())
                    || window.max.is_some_and(|value| !value.is_finite())
                    || window.increase.is_some_and(|value| !value.is_finite())
            })
        {
            return Err(AlertSeriesStatus::Misconfigured);
        }
        Ok(reply)
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

    pub async fn rotate_agent_token(&self, id: i64) -> Result<Value, CoreError> {
        self.admin(Method::POST, format!("agents/{id}/rotate-token"), None)
            .await
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

fn classify_alert_series_status(status: StatusCode, reply: &Value) -> AlertSeriesStatus {
    match status {
        StatusCode::TOO_MANY_REQUESTS => AlertSeriesStatus::Busy,
        StatusCode::GATEWAY_TIMEOUT => AlertSeriesStatus::Timeout,
        StatusCode::SERVICE_UNAVAILABLE
            if reply["error"].as_str() == Some("query capacity is exhausted") =>
        {
            AlertSeriesStatus::Busy
        }
        StatusCode::SERVICE_UNAVAILABLE
            if reply["error"].as_str() == Some("query deadline exceeded") =>
        {
            AlertSeriesStatus::Timeout
        }
        StatusCode::BAD_REQUEST
        | StatusCode::UNAUTHORIZED
        | StatusCode::FORBIDDEN
        | StatusCode::NOT_FOUND => AlertSeriesStatus::Misconfigured,
        status if status.is_client_error() => AlertSeriesStatus::Misconfigured,
        _ => AlertSeriesStatus::Unavailable,
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

fn valid_duration(duration: &DurationStats, total: u64) -> bool {
    duration.measured <= total
        && duration.coverage.is_finite()
        && (0.0..=1.0).contains(&duration.coverage)
        && if duration.measured == 0 {
            duration.p95_ms.is_none() && duration.coverage == 0.0
        } else {
            duration
                .p95_ms
                .is_some_and(|value| value.is_finite() && value >= 0.0)
        }
}

#[cfg(test)]
mod alert_status_tests {
    use super::*;

    #[test]
    fn alert_query_http_status_and_error_body_are_classified() {
        assert_eq!(
            classify_alert_series_status(StatusCode::TOO_MANY_REQUESTS, &Value::Null),
            AlertSeriesStatus::Busy
        );
        assert_eq!(
            classify_alert_series_status(StatusCode::GATEWAY_TIMEOUT, &Value::Null),
            AlertSeriesStatus::Timeout
        );
        assert_eq!(
            classify_alert_series_status(
                StatusCode::SERVICE_UNAVAILABLE,
                &json!({"error":"query capacity is exhausted"}),
            ),
            AlertSeriesStatus::Busy
        );
        assert_eq!(
            classify_alert_series_status(
                StatusCode::SERVICE_UNAVAILABLE,
                &json!({"error":"query deadline exceeded"}),
            ),
            AlertSeriesStatus::Timeout
        );
        assert_eq!(
            classify_alert_series_status(
                StatusCode::SERVICE_UNAVAILABLE,
                &json!({"error":"storage unavailable"}),
            ),
            AlertSeriesStatus::Unavailable
        );
        assert_eq!(
            classify_alert_series_status(StatusCode::UNAUTHORIZED, &Value::Null),
            AlertSeriesStatus::Misconfigured
        );
    }
}
