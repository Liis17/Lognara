//! Stateless, bearer-authenticated MCP endpoint with token-gated tools.
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::Instant,
};

use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::State,
    http::{
        HeaderMap, HeaderValue, Request, StatusCode,
        header::{
            AUTHORIZATION, CACHE_CONTROL, CONTENT_LENGTH, CONTENT_TYPE, RETRY_AFTER,
            WWW_AUTHENTICATE,
        },
    },
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::any,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use rmcp::{
    ErrorData, RoleServer, ServerHandler,
    handler::server::{router::tool::ToolRouter, tool::ToolCallContext, wrapper::Parameters},
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
        ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
    },
    schemars::{self, JsonSchema},
    service::RequestContext,
    tool, tool_router,
    transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use time::{Duration, OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::{
    alerts::{AlertToolError, DefinitionInput, TriggerInput},
    api::AppState,
    core_client::{CoreError, CoreQueryError, LogSourceFilters, LogSourcesRequest},
    query::{
        Filters, GroupRequest, HistogramRequest, SearchRequest, SummaryRequest, TextMode,
        TextQuery, TraceParams,
    },
    series::MetricKey,
    store,
    token_access::{READ_SCOPE, Rights, Scope, TokenAccess, WRITE_SCOPE},
};

const MAX_REQUEST_BYTES: usize = 1024 * 1024;
const MAX_RESULT_BYTES: usize = 2 * 1024 * 1024;
const RESULT_HEADROOM: usize = 4096;
const PAGE_TTL_MS: i64 = 10 * 60 * 1000;
/// Версия 2 добавляет скоуп токена; курсоры версии 1 больше не принимаются.
const CURSOR_VERSION: u8 = 2;
const LOG_TOOLS: &[&str] = &[
    "list_applications",
    "list_servers",
    "list_environments",
    "search_logs",
    "get_trace",
    "summarize_logs",
];
const METRIC_TOOLS: &[&str] = &["list_metrics", "query_metrics"];
const ALERT_GROUP_TOOLS: &[&str] = &["list_alert_groups"];
const METRIC_ALERT_TOOLS: &[&str] = &[
    "list_alerts",
    "create_alert",
    "update_alert",
    "delete_alert",
];
const LOG_ALERT_TOOLS: &[&str] = &[
    "list_log_alerts",
    "create_log_alert",
    "update_log_alert",
    "delete_log_alert",
];
const ALERT_WRITE_TOOLS: &[&str] = &[
    "create_alert",
    "update_alert",
    "delete_alert",
    "create_log_alert",
    "update_log_alert",
    "delete_log_alert",
];
type HmacSha256 = Hmac<Sha256>;

pub struct McpLimiter {
    global: Arc<Semaphore>,
    users: Mutex<HashMap<String, UserLimiter>>,
}

struct UserLimiter {
    inflight: Arc<Semaphore>,
    calls: VecDeque<Instant>,
}

pub struct McpPermit {
    _global: OwnedSemaphorePermit,
    _user: OwnedSemaphorePermit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LimitError {
    Busy,
    RateLimited,
}

impl Default for UserLimiter {
    fn default() -> Self {
        Self {
            inflight: Arc::new(Semaphore::new(2)),
            calls: VecDeque::new(),
        }
    }
}

impl Default for McpLimiter {
    fn default() -> Self {
        Self {
            global: Arc::new(Semaphore::new(8)),
            users: Mutex::new(HashMap::new()),
        }
    }
}

impl McpLimiter {
    fn try_enter(&self, username: &str) -> Result<McpPermit, LimitError> {
        let global = self
            .global
            .clone()
            .try_acquire_owned()
            .map_err(|_| LimitError::Busy)?;
        let mut users = self.users.lock().unwrap();
        users.retain(|_, user| {
            while user
                .calls
                .front()
                .is_some_and(|called| called.elapsed() >= std::time::Duration::from_secs(60))
            {
                user.calls.pop_front();
            }
            !user.calls.is_empty() || user.inflight.available_permits() < 2
        });
        let user = users.entry(username.to_owned()).or_default();
        if user.calls.len() >= 60 {
            return Err(LimitError::RateLimited);
        }
        let permit = user
            .inflight
            .clone()
            .try_acquire_owned()
            .map_err(|_| LimitError::Busy)?;
        user.calls.push_back(Instant::now());
        Ok(McpPermit {
            _global: global,
            _user: permit,
        })
    }
}

pub fn router(app: AppState) -> Router<AppState> {
    if !app.config.mcp_enabled {
        return Router::new().route("/mcp", any(disabled));
    }
    let authority = authority(&app);
    let config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true)
        .with_max_request_body_bytes(MAX_REQUEST_BYTES)
        .with_cancellation_token(app.shutdown.child_token())
        .with_allowed_hosts([authority])
        .with_allowed_origins([
            app.config.public_origin.clone(),
            "https://chatgpt.com".to_owned(),
            "https://claude.ai".to_owned(),
        ])
        .enforce_origin_validation();
    let server_app = app.clone();
    let service: StreamableHttpService<McpServer, LocalSessionManager> = StreamableHttpService::new(
        move || Ok(McpServer::new(server_app.clone())),
        Arc::new(LocalSessionManager::default()),
        config,
    );
    Router::new()
        .nest_service("/mcp", service)
        .layer(middleware::from_fn_with_state(app.clone(), authenticate))
        .layer(
            tower_http::set_header::SetResponseHeaderLayer::if_not_present(
                CACHE_CONTROL,
                HeaderValue::from_static("no-store"),
            ),
        )
}

async fn disabled() -> StatusCode {
    StatusCode::NOT_FOUND
}

fn authority(app: &AppState) -> String {
    let host = app.config.public_url.host_str().unwrap_or_default();
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    app.config
        .public_url
        .port()
        .map_or(host.clone(), |port| format!("{host}:{port}"))
}

async fn authenticate(State(app): State<AppState>, request: Request<Body>, next: Next) -> Response {
    if request.method() != axum::http::Method::POST {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    if request
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|length| length.to_str().ok())
        .and_then(|length| length.parse::<usize>().ok())
        .is_some_and(|length| length > MAX_REQUEST_BYTES)
    {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    }
    let auth = bearer_token(request.headers());
    let (mut parts, body) = request.into_parts();
    let bytes = match to_bytes(body, MAX_REQUEST_BYTES).await {
        Ok(bytes) => bytes,
        Err(_) => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
    };
    let request_value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    let is_tools_list = request_value["method"] == "tools/list";

    let Some(token) = auth else {
        return authentication_required(&app, &request_value, "");
    };
    let hash: [u8; 32] = Sha256::digest(token.as_bytes()).into();
    let access = match app
        .store
        .authenticate_mcp_token(hash, resource_uri(&app))
        .await
    {
        Ok(Some(access)) => access,
        Ok(None) => return authentication_required(&app, &request_value, "invalid_token"),
        Err(error) => {
            tracing::error!(%error, "MCP credential lookup failed");
            return service_error("unavailable", "MCP credential storage is unavailable");
        }
    };
    let username = access.username.clone();
    if !app.htpasswd.contains(&username) {
        if let Err(error) = app.store.revoke_user_mcp_credentials(username).await {
            tracing::error!(%error, "failed to revoke credentials for a removed user");
            return service_error("unavailable", "MCP credential storage is unavailable");
        }
        return authentication_required(&app, &request_value, "invalid_token");
    }
    let _permit = match app.mcp_limiter.try_enter(&username) {
        Ok(permit) => permit,
        Err(LimitError::Busy) => return limited("busy", 1),
        Err(LimitError::RateLimited) => return limited("rate_limited", 60),
    };
    // Права и скоуп передаются инструментам через extensions HTTP-запроса, которые rmcp сохраняет.
    parts.extensions.insert(access);
    let request = Request::from_parts(parts, Body::from(bytes));
    let response = next.run(request).await;
    let response = if is_tools_list {
        add_oauth_security_schemes(response).await
    } else {
        response
    };
    bound_response(
        response,
        request_value.get("id").cloned().unwrap_or(Value::Null),
    )
    .await
}

// Check the complete envelope, including the caller-controlled JSON-RPC id.
async fn bound_response(response: Response, id: Value) -> Response {
    let (mut parts, body) = response.into_parts();
    match to_bytes(body, MAX_RESULT_BYTES).await {
        Ok(bytes) => Response::from_parts(parts, Body::from(bytes)),
        Err(_) => {
            let result = failure_result(ToolFailure::new(
                "response_too_large",
                "The response exceeds 2 MiB; retry with a smaller limit or a larger metric step.",
            ));
            let bytes = serde_json::to_vec(&json!({"jsonrpc":"2.0", "id":id, "result":result}))
                .expect("JSON response is serializable");
            parts.headers.remove(CONTENT_LENGTH);
            parts
                .headers
                .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
            Response::from_parts(parts, Body::from(bytes))
        }
    }
}

fn bearer_token(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    (scheme.eq_ignore_ascii_case("bearer")
        && !token.is_empty()
        && !token.bytes().any(|byte| byte.is_ascii_whitespace()))
    .then(|| token.to_owned())
}

fn resource_uri(app: &AppState) -> String {
    app.config
        .public_url
        .join("mcp")
        .expect("valid MCP URL")
        .as_str()
        .to_owned()
}

fn metadata_uri(app: &AppState) -> String {
    app.config
        .public_url
        .join(".well-known/oauth-protected-resource/mcp")
        .expect("valid MCP URL")
        .as_str()
        .to_owned()
}

fn authentication_required(app: &AppState, body: &Value, error: &str) -> Response {
    let metadata = metadata_uri(app);
    let mut challenge = format!("Bearer resource_metadata=\"{metadata}\", scope=\"{READ_SCOPE}\"");
    if !error.is_empty() {
        challenge.push_str(", error=\"invalid_token\", error_description=\"The access token is invalid, expired, revoked, or has the wrong audience or scope\"");
    }
    if body["method"] == "tools/call" && body.get("id").is_some() {
        let mut meta = serde_json::Map::new();
        meta.insert("mcp/www_authenticate".into(), json!([challenge]));
        let result = CallToolResult::error(vec![ContentBlock::text(
            "Authentication is required to call this tool.",
        )])
        .with_meta(Some(rmcp::model::MetaObject(meta)));
        let response = json!({
            "jsonrpc": "2.0",
            "id": body["id"],
            "result": result,
        });
        return (
            StatusCode::OK,
            [
                (CACHE_CONTROL, "no-store"),
                (CONTENT_TYPE, "application/json"),
            ],
            Json(response),
        )
            .into_response();
    }
    let mut response = (
        StatusCode::UNAUTHORIZED,
        Json(json!({ "error": if error.is_empty() { "unauthorized" } else { "invalid_token" } })),
    )
        .into_response();
    response.headers_mut().insert(
        WWW_AUTHENTICATE,
        HeaderValue::from_str(&challenge).expect("static OAuth challenge is a header value"),
    );
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn limited(code: &str, retry_after: u64) -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        [(RETRY_AFTER, retry_after.to_string())],
        Json(json!({ "error": code, "message": "MCP request capacity is exhausted; retry later" })),
    )
        .into_response()
}

fn service_error(code: &str, message: &str) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({ "error": code, "message": message })),
    )
        .into_response()
}

async fn add_oauth_security_schemes(mut response: Response) -> Response {
    let (mut parts, body) = response.into_parts();
    let bytes = match to_bytes(body, 256 * 1024).await {
        Ok(bytes) => bytes,
        Err(_) => return service_error("unavailable", "MCP tool list response was too large"),
    };
    let Ok(mut value) = serde_json::from_slice::<Value>(&bytes) else {
        return Response::from_parts(parts, Body::from(bytes));
    };
    if let Some(tools) = value["result"]["tools"].as_array_mut() {
        for tool in tools {
            let scope = if tool["name"]
                .as_str()
                .is_some_and(|name| ALERT_WRITE_TOOLS.contains(&name))
            {
                WRITE_SCOPE
            } else {
                READ_SCOPE
            };
            tool["securitySchemes"] = json!([{
                "type": "oauth2",
                "scopes": [scope],
            }]);
        }
    }
    let encoded = match serde_json::to_vec(&value) {
        Ok(encoded) => encoded,
        Err(_) => {
            return service_error("unavailable", "MCP tool list response could not be encoded");
        }
    };
    parts.headers.remove(CONTENT_LENGTH);
    parts.headers.insert(
        CONTENT_LENGTH,
        HeaderValue::from_str(&encoded.len().to_string()).expect("content length is decimal"),
    );
    response = Response::from_parts(parts, Body::from(encoded));
    response
}

#[derive(Clone)]
struct McpServer {
    app: AppState,
    tool_router: ToolRouter<Self>,
    /// Заполняется в `call_tool` на копии сервера, когда токен прошёл проверку.
    access: Option<TokenAccess>,
}

impl McpServer {
    fn new(app: AppState) -> Self {
        Self {
            app,
            tool_router: Self::tool_router(),
            access: None,
        }
    }

    /// Права текущего вызова; без проверенного токена инструменты не отвечают данными.
    fn access(&self) -> Result<TokenAccess, ToolFailure> {
        self.access.clone().ok_or(ToolFailure::new(
            "forbidden",
            "Token access is not available",
        ))
    }
}

/// Инструмент виден и доступен, если права токена разрешают его; `get_versions` доступен всем.
fn tool_allowed(name: &str, access: &TokenAccess) -> bool {
    let rights: Rights = access.rights;
    name == "get_versions"
        || (rights.logs && LOG_TOOLS.contains(&name))
        || (rights.metrics && METRIC_TOOLS.contains(&name))
        || (rights.alerts_write && ALERT_GROUP_TOOLS.contains(&name))
        || (rights.alerts_write && rights.metrics && METRIC_ALERT_TOOLS.contains(&name))
        || (rights.alerts_write && rights.logs && LOG_ALERT_TOOLS.contains(&name))
}

fn access_from(context: &RequestContext<RoleServer>) -> Option<TokenAccess> {
    context
        .extensions
        .get::<axum::http::request::Parts>()?
        .extensions
        .get::<TokenAccess>()
        .cloned()
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
struct AlertTriggerInput {
    window_seconds: u64,
    aggregate: String,
    comparator: String,
    threshold: f64,
}

/// Правило метрики в том же виде, что в UI: окно, условия и получатели по ID групп.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
struct AlertInput {
    server: String,
    service: String,
    name: String,
    origin: String,
    /// Окружение; `null` означает все окружения.
    environment: Option<String>,
    enabled: bool,
    /// `normal` или `loss` для отсутствующих данных.
    no_data: String,
    /// `and` или `or`.
    combine: String,
    /// ID существующих групп получателей; создать группу через MCP нельзя.
    group_ids: Vec<String>,
    triggers: Vec<AlertTriggerInput>,
    /// Необязательные подписи и настройки Telegram. Старые вызовы продолжают использовать значения по умолчанию.
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    message_template: Option<String>,
    #[serde(default)]
    message_fields: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
struct UpdateAlertInput {
    id: String,
    alert: AlertInput,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct DeleteAlertInput {
    id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct McpLogAlertText {
    query: String,
    #[serde(default = "default_log_alert_mode")]
    mode: String,
}

fn default_log_alert_mode() -> String {
    "all".into()
}

/// Log alert settings follow the existing Dock log alert API schema.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct McpLogAlertInput {
    name: String,
    enabled: bool,
    query: String,
    #[serde(default)]
    filters: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    text: Option<McpLogAlertText>,
    threshold: u32,
    cooldown_seconds: u32,
    group_ids: Vec<String>,
    #[serde(default)]
    message_template: Option<String>,
    #[serde(default)]
    message_fields: Option<Vec<String>>,
}

impl From<McpLogAlertInput> for crate::log_alerts::LogAlertInput {
    fn from(input: McpLogAlertInput) -> Self {
        Self {
            name: input.name,
            enabled: input.enabled,
            query: input.query,
            filters: input.filters,
            text: input.text.map(|text| crate::log_alerts::LogAlertText {
                query: text.query,
                mode: text.mode,
            }),
            threshold: input.threshold,
            cooldown_seconds: input.cooldown_seconds,
            group_ids: input.group_ids,
            message_template: input.message_template,
            message_fields: input.message_fields,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct UpdateLogAlertInput {
    id: String,
    alert: McpLogAlertInput,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct DeleteLogAlertInput {
    id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct McpText {
    query: String,
    #[serde(default)]
    mode: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ListApplicationsInput {
    #[serde(default)]
    server: Option<String>,
    #[serde(default)]
    environment: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    cursor: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ListServersInput {
    #[serde(default)]
    app: Option<String>,
    #[serde(default)]
    environment: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    cursor: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ListEnvironmentsInput {
    #[serde(default)]
    app: Option<String>,
    #[serde(default)]
    server: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    cursor: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct SearchLogsInput {
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    to: Option<String>,
    #[serde(default)]
    duration_seconds: Option<u64>,
    #[serde(default)]
    filters: Option<Filters>,
    #[serde(default)]
    text: Option<McpText>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    cursor: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct GetTraceInput {
    trace_id: String,
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    to: Option<String>,
    #[serde(default)]
    duration_seconds: Option<u64>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    cursor: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct SummarizeLogsInput {
    operation: String,
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    to: Option<String>,
    #[serde(default)]
    duration_seconds: Option<u64>,
    #[serde(default)]
    filters: Option<Filters>,
    #[serde(default)]
    text: Option<McpText>,
    #[serde(default)]
    interval_seconds: Option<u64>,
    #[serde(default)]
    group_by: Option<Vec<String>>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    search_limit: Option<usize>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct QueryMetricsInput {
    #[serde(default)]
    server: Option<String>,
    service: String,
    name: String,
    from: String,
    to: String,
    step_seconds: u64,
    #[serde(default)]
    split: Option<String>,
    #[serde(default)]
    origin: Option<String>,
    #[serde(default)]
    environment: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PageCursor {
    version: u8,
    from: String,
    to: String,
    limit: usize,
    /// Скоуп токена, который создал курсор: курсор не переходит к другому скоупу.
    scope: Scope,
    query: CursorQuery,
    core_cursor: String,
    expires_at_ms: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum CursorQuery {
    Search {
        filters: Filters,
        text: Option<McpText>,
    },
    Trace {
        trace_id: String,
    },
}

#[derive(Debug)]
struct ToolFailure {
    code: &'static str,
    message: &'static str,
    guidance: Option<&'static str>,
}

impl ToolFailure {
    fn new(code: &'static str, message: &'static str) -> Self {
        Self {
            code,
            message,
            guidance: None,
        }
    }

    fn with_guidance(mut self, guidance: &'static str) -> Self {
        self.guidance = Some(guidance);
        self
    }
}

#[tool_router]
impl McpServer {
    #[tool(
        description = "List distinct application, project, or service names retained in logs. Exact source filters and opaque pagination cursor; no date range is required.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn list_applications(
        &self,
        Parameters(input): Parameters<ListApplicationsInput>,
    ) -> CallToolResult {
        let limit = match source_limit(input.limit) {
            Ok(limit) => limit,
            Err(error) => return failure_result(error),
        };
        let scope = match self.access() {
            Ok(access) => access.node_scope,
            Err(error) => return failure_result(error),
        };
        let request = LogSourcesRequest {
            field: "service".into(),
            filters: LogSourceFilters {
                server: input.server,
                environment: input.environment,
                ..source_scope(scope)
            },
            limit,
            cursor: input.cursor,
        };
        match self.app.core.log_sources(&request).await {
            Ok(page) => success_result(json!({
                "applications": page.values,
                "next_cursor": page.next_cursor,
                "as_of": page.as_of,
            })),
            Err(error) => failure_result(query_failure(error)),
        }
    }

    #[tool(
        description = "List distinct server names retained in logs. The optional app is an exact service/project/application name filter; use the returned cursor to page through the complete list.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn list_servers(
        &self,
        Parameters(input): Parameters<ListServersInput>,
    ) -> CallToolResult {
        let limit = match source_limit(input.limit) {
            Ok(limit) => limit,
            Err(error) => return failure_result(error),
        };
        let scope = match self.access() {
            Ok(access) => access.node_scope,
            Err(error) => return failure_result(error),
        };
        let request = LogSourcesRequest {
            field: "server".into(),
            filters: LogSourceFilters {
                service: input.app,
                environment: input.environment,
                ..source_scope(scope)
            },
            limit,
            cursor: input.cursor,
        };
        match self.app.core.log_sources(&request).await {
            Ok(page) => success_result(
                json!({ "servers": page.values, "next_cursor": page.next_cursor, "as_of": page.as_of }),
            ),
            Err(error) => failure_result(query_failure(error)),
        }
    }

    #[tool(
        description = "List distinct environments retained in logs, with optional exact app and server filters. Use the returned cursor to page through the complete list.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn list_environments(
        &self,
        Parameters(input): Parameters<ListEnvironmentsInput>,
    ) -> CallToolResult {
        let limit = match source_limit(input.limit) {
            Ok(limit) => limit,
            Err(error) => return failure_result(error),
        };
        let scope = match self.access() {
            Ok(access) => access.node_scope,
            Err(error) => return failure_result(error),
        };
        let request = LogSourcesRequest {
            field: "environment".into(),
            filters: LogSourceFilters {
                service: input.app,
                server: input.server,
                ..source_scope(scope)
            },
            limit,
            cursor: input.cursor,
        };
        match self.app.core.log_sources(&request).await {
            Ok(page) => success_result(
                json!({ "environments": page.values, "next_cursor": page.next_cursor, "as_of": page.as_of }),
            ),
            Err(error) => failure_result(query_failure(error)),
        }
    }

    #[tool(
        description = "Search logs newest first using the existing exact-field filters and all/phrase text semantics. Supply RFC3339 from and to, or duration_seconds; omitted bounds default to the last 24 hours. The first page returns resolved bounds. Pass next_cursor alone (or the same filters, text and limit) to preserve the frozen time window. Limits are 1..1000, default 100.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn search_logs(&self, Parameters(input): Parameters<SearchLogsInput>) -> CallToolResult {
        let access = match self.access() {
            Ok(access) => access,
            Err(error) => return failure_result(error),
        };
        match search_page(&self.app, &access, input).await {
            Ok(value) => success_result(value),
            Err(error) => failure_result(error),
        }
    }

    #[tool(
        description = "Get logs for one trace in oldest-first order, preserving the existing trace query semantics. Supply RFC3339 from and to, or duration_seconds. Pagination cursors freeze the original bounds for ten minutes.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn get_trace(&self, Parameters(input): Parameters<GetTraceInput>) -> CallToolResult {
        let access = match self.access() {
            Ok(access) => access,
            Err(error) => return failure_result(error),
        };
        match trace_page(&self.app, &access, input).await {
            Ok(value) => success_result(value),
            Err(error) => failure_result(error),
        }
    }

    #[tool(
        description = "Summarize logs using the existing summary, group, or histogram analytics. Set operation to summary, group, or histogram. Filters are the existing exact-field filters; no formulas or write operations are accepted.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn summarize_logs(
        &self,
        Parameters(input): Parameters<SummarizeLogsInput>,
    ) -> CallToolResult {
        let access = match self.access() {
            Ok(access) => access,
            Err(error) => return failure_result(error),
        };
        match summarize(&self.app, &access, input).await {
            Ok(value) => success_result(value),
            Err(error) => failure_result(error),
        }
    }

    #[tool(
        description = "List available metric series with their kind, unit, definition, server source and environment metadata. This is a metrics catalog; applications that only emit logs remain discoverable through list_applications.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn list_metrics(&self) -> CallToolResult {
        let access = match self.access() {
            Ok(access) => access,
            Err(error) => return failure_result(error),
        };
        match self
            .app
            .core
            .scoped_series(None, None, access.node_scope)
            .await
        {
            Ok((status, body)) if status.is_success() => success_result(body),
            Ok(_) => failure_result(ToolFailure::new(
                "unavailable",
                "Core returned an unexpected metric-list status",
            )),
            Err(error) => failure_result(core_failure(error)),
        }
    }

    #[tool(
        description = "Query one metric with existing server, service, name, time, step, split, origin, and environment semantics. Core preserves metric kind, unit, aggregation, null gaps and explicit series truncation metadata.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn query_metrics(
        &self,
        Parameters(input): Parameters<QueryMetricsInput>,
    ) -> CallToolResult {
        let split = input.split.as_deref().unwrap_or("none");
        if !matches!(split, "none" | "instance" | "labels") {
            return failure_result(ToolFailure::new(
                "invalid_argument",
                "split must be none, instance, or labels",
            ));
        }
        if input
            .origin
            .as_deref()
            .is_some_and(|origin| !matches!(origin, "counter" | "gauge" | "rule"))
        {
            return failure_result(ToolFailure::new(
                "invalid_argument",
                "origin must be counter, gauge, or rule",
            ));
        }
        let access = match self.access() {
            Ok(access) => access,
            Err(error) => return failure_result(error),
        };
        let mut body = json!({
            "server": input.server,
            "service": input.service,
            "name": input.name,
            "from": input.from,
            "to": input.to,
            "step_seconds": input.step_seconds,
            "split": split,
            "origin": input.origin,
            "environment": input.environment,
        });
        if let Some((key, id)) = access.node_scope.core_param() {
            body[key] = json!(id);
        }
        match self.app.core.series_query(&body).await {
            Ok((status, body)) if status.is_success() => success_result(body),
            Ok(_) => failure_result(ToolFailure::new(
                "unavailable",
                "Core returned an unexpected metric-query status",
            )),
            Err(error) => failure_result(core_failure(error)),
        }
    }

    #[tool(
        description = "List recipient groups for metric and log alerts. Groups are created only in the Dock UI; only id, kind and name are returned, never chat IDs or bot tokens.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn list_alert_groups(&self) -> CallToolResult {
        match crate::alerts::mcp_alert_groups(&self.app).await {
            Ok(groups) => success_result(json!({ "groups": groups })),
            Err(error) => failure_result(alert_failure(error)),
        }
    }

    #[tool(
        description = "List metric alerts visible to this token. An unrestricted token sees every alert; a relay or agent token sees only alerts on its node. Each alert includes its id, node scope and state.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn list_alerts(&self) -> CallToolResult {
        let access = match self.access() {
            Ok(access) => access,
            Err(error) => return failure_result(error),
        };
        match crate::alerts::mcp_alerts(&self.app, &access).await {
            Ok(alerts) => success_result(json!({ "alerts": alerts })),
            Err(error) => failure_result(alert_failure(error)),
        }
    }

    #[tool(
        description = "Create a metric alert. The alert takes the node scope of this token; recipients must be existing group IDs from list_alert_groups. Returns the saved alert.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn create_alert(&self, Parameters(input): Parameters<AlertInput>) -> CallToolResult {
        let access = match self.access() {
            Ok(access) => access,
            Err(error) => return failure_result(error),
        };
        match crate::alerts::mcp_save_alert(&self.app, &access, None, alert_definition(input)).await
        {
            Ok(alert) => success_result(json!({ "alert": alert })),
            Err(error) => failure_result(alert_failure(error)),
        }
    }

    #[tool(
        description = "Replace the settings of a metric alert visible to this token. The alert keeps its node scope; recipients must be existing group IDs. Returns the saved alert.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn update_alert(
        &self,
        Parameters(input): Parameters<UpdateAlertInput>,
    ) -> CallToolResult {
        let access = match self.access() {
            Ok(access) => access,
            Err(error) => return failure_result(error),
        };
        match crate::alerts::mcp_save_alert(
            &self.app,
            &access,
            Some(input.id),
            alert_definition(input.alert),
        )
        .await
        {
            Ok(alert) => success_result(json!({ "alert": alert })),
            Err(error) => failure_result(alert_failure(error)),
        }
    }

    #[tool(
        description = "Delete a metric alert visible to this token. Pending notifications for the alert are cancelled.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            open_world_hint = false
        )
    )]
    async fn delete_alert(
        &self,
        Parameters(input): Parameters<DeleteAlertInput>,
    ) -> CallToolResult {
        let access = match self.access() {
            Ok(access) => access,
            Err(error) => return failure_result(error),
        };
        match crate::alerts::mcp_delete_alert(&self.app, &access, input.id).await {
            Ok(()) => success_result(json!({ "deleted": true })),
            Err(error) => failure_result(alert_failure(error)),
        }
    }

    #[tool(
        description = "List log alerts visible to this token. A relay token sees rules scoped to that relay and its agents; an agent token sees only rules scoped to that agent.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn list_log_alerts(&self) -> CallToolResult {
        let access = match self.access() {
            Ok(access) => access,
            Err(error) => return failure_result(error),
        };
        match crate::log_alerts::mcp_log_alerts(&self.app, &access).await {
            Ok(alerts) => success_result(json!({ "alerts": alerts })),
            Err(error) => failure_result(log_alert_failure(error)),
        }
    }

    #[tool(
        description = "Create a log alert using the Dock log alert schema. Recipients must be existing group IDs from list_alert_groups. Optional message_template and message_fields customize the Telegram message. For a relay or agent token, Dock adds its node filter; a different value for that node is rejected.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn create_log_alert(
        &self,
        Parameters(input): Parameters<McpLogAlertInput>,
    ) -> CallToolResult {
        let access = match self.access() {
            Ok(access) => access,
            Err(error) => return failure_result(error),
        };
        match crate::log_alerts::mcp_save_log_alert(&self.app, &access, None, input.into()).await {
            Ok(alert) => success_result(json!({ "alert": alert })),
            Err(error) => failure_result(log_alert_failure(error)),
        }
    }

    #[tool(
        description = "Replace the settings of a log alert visible to this token. Optional message_template and message_fields customize the Telegram message; omitting them keeps the saved message settings. Existing relay_id and agent_id filters remain fixed: omitted filters are restored, changing or adding them is rejected. Concurrent browser or MCP changes return conflict. Recipients must be existing group IDs.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn update_log_alert(
        &self,
        Parameters(input): Parameters<UpdateLogAlertInput>,
    ) -> CallToolResult {
        let access = match self.access() {
            Ok(access) => access,
            Err(error) => return failure_result(error),
        };
        match crate::log_alerts::mcp_save_log_alert(
            &self.app,
            &access,
            Some(input.id),
            input.alert.into(),
        )
        .await
        {
            Ok(alert) => success_result(json!({ "alert": alert })),
            Err(error) => failure_result(log_alert_failure(error)),
        }
    }

    #[tool(
        description = "Delete a log alert visible to this token. Pending notifications for the alert are removed.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            open_world_hint = false
        )
    )]
    async fn delete_log_alert(
        &self,
        Parameters(input): Parameters<DeleteLogAlertInput>,
    ) -> CallToolResult {
        let access = match self.access() {
            Ok(access) => access,
            Err(error) => return failure_result(error),
        };
        match crate::log_alerts::mcp_delete_log_alert(&self.app, &access, input.id).await {
            Ok(()) => success_result(json!({ "deleted": true })),
            Err(error) => failure_result(log_alert_failure(error)),
        }
    }

    #[tool(
        description = "Get Dock and Core package versions plus optional published release version and git commit. A package version is not treated as an image release tag.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn get_versions(&self) -> CallToolResult {
        match self.app.core.status().await {
            Ok(core) => success_result(json!({
                "dock": {
                    "version": env!("CARGO_PKG_VERSION"),
                    "release_version": Some(crate::version::release_version()),
                    "git_commit": option_env!("LOGNARA_GIT_COMMIT").filter(|value| !value.is_empty()),
                },
                "core": {
                    "version": core.version,
                    "release_version": core.release_version,
                    "git_commit": core.git_commit,
                },
            })),
            Err(error) => failure_result(query_failure(error)),
        }
    }
}

impl ServerHandler for McpServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build()).with_server_info(
            Implementation::new("lognara-dock", env!("CARGO_PKG_VERSION")),
        )
    }

    /// Показывает только инструменты, которые разрешают права токена.
    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let access = access_from(&context);
        let tools = self
            .tool_router
            .list_all()
            .into_iter()
            .filter(|tool| {
                access
                    .as_ref()
                    .is_some_and(|access| tool_allowed(&tool.name, access))
            })
            .collect();
        Ok(ListToolsResult {
            tools,
            ..Default::default()
        })
    }

    /// Запрещённый инструмент отвечает ошибкой инструмента и не выполняет запрос к core.
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let Some(access) =
            access_from(&context).filter(|access| tool_allowed(&request.name, access))
        else {
            return Ok(failure_result(ToolFailure::new(
                "forbidden",
                "This token does not allow this tool",
            ))
            .into());
        };
        let server = Self {
            app: self.app.clone(),
            tool_router: self.tool_router.clone(),
            access: Some(access),
        };
        server
            .tool_router
            .call(ToolCallContext::new(&server, request, context))
            .await
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.tool_router.get(name).cloned()
    }
}

async fn search_page(
    app: &AppState,
    access: &TokenAccess,
    input: SearchLogsInput,
) -> Result<Value, ToolFailure> {
    let (from, to, filters, text, limit, core_cursor, mut page_cursor) = if let Some(encoded) =
        input.cursor.as_deref()
    {
        let cursor = decode_cursor(app, encoded)?;
        if cursor.scope != access.node_scope {
            return Err(ToolFailure::new(
                "invalid_cursor",
                "Cursor belongs to a different token scope",
            ));
        }
        let CursorQuery::Search { filters, text } = &cursor.query else {
            return Err(ToolFailure::new(
                "invalid_cursor",
                "Cursor belongs to a different query",
            ));
        };
        if input.filters.as_ref().is_some_and(|value| value != filters)
            || input
                .text
                .as_ref()
                .is_some_and(|value| Some(value) != text.as_ref())
            || input.limit.is_some_and(|value| value != cursor.limit)
        {
            return Err(ToolFailure::new(
                "invalid_cursor",
                "Cursor does not match the supplied search filters, text, or limit",
            ));
        }
        if input.duration_seconds.is_some() {
            return Err(ToolFailure::new(
                "invalid_cursor",
                "Omit duration_seconds when continuing a page",
            ));
        }
        if input.from.is_some() || input.to.is_some() {
            let (requested_from, requested_to) = resolve_period(input.from, input.to, None, false)?;
            if requested_from != cursor.from || requested_to != cursor.to {
                return Err(ToolFailure::new(
                    "invalid_cursor",
                    "Cursor is bound to different resolved time bounds",
                ));
            }
        }
        let core_cursor = cursor.core_cursor.clone();
        (
            cursor.from.clone(),
            cursor.to.clone(),
            filters.clone(),
            text.clone(),
            cursor.limit,
            Some(core_cursor),
            cursor,
        )
    } else {
        let (from, to) = resolve_period(input.from, input.to, input.duration_seconds, true)?;
        let filters = input.filters.unwrap_or_default();
        let text = input.text;
        let limit = input.limit.unwrap_or(100);
        if !(1..=1000).contains(&limit) {
            return Err(ToolFailure::new(
                "invalid_argument",
                "limit must be 1..1000",
            ));
        }
        let cursor = PageCursor {
            version: CURSOR_VERSION,
            from: from.clone(),
            to: to.clone(),
            limit,
            scope: access.node_scope,
            query: CursorQuery::Search {
                filters: filters.clone(),
                text: text.clone(),
            },
            core_cursor: String::new(),
            expires_at_ms: store::now_ms().saturating_add(PAGE_TTL_MS),
        };
        (from, to, filters, text, limit, None, cursor)
    };
    let text = match text_query(text) {
        Ok(text) => text,
        Err(error) => return Err(error),
    };
    let response = app
        .core
        .search_page(&SearchRequest {
            from: from.clone(),
            to: to.clone(),
            filters: scoped_filters(filters, access.node_scope)?,
            text,
            limit,
            cursor: core_cursor,
        })
        .await
        .map_err(query_failure)?;
    let next_cursor = match response.next_cursor {
        Some(core_cursor) => {
            page_cursor.core_cursor = core_cursor;
            Some(encode_cursor(app, &page_cursor)?)
        }
        None => None,
    };
    Ok(json!({
        "from": from,
        "to": to,
        "events": response.events,
        "next_cursor": next_cursor,
        "as_of": response.as_of,
    }))
}

async fn trace_page(
    app: &AppState,
    access: &TokenAccess,
    input: GetTraceInput,
) -> Result<Value, ToolFailure> {
    let (from, to, limit, core_cursor, mut page_cursor) = if let Some(encoded) =
        input.cursor.as_deref()
    {
        let cursor = decode_cursor(app, encoded)?;
        if cursor.scope != access.node_scope {
            return Err(ToolFailure::new(
                "invalid_cursor",
                "Cursor belongs to a different token scope",
            ));
        }
        let CursorQuery::Trace { trace_id } = &cursor.query else {
            return Err(ToolFailure::new(
                "invalid_cursor",
                "Cursor belongs to a different query",
            ));
        };
        if trace_id != &input.trace_id || input.limit.is_some_and(|value| value != cursor.limit) {
            return Err(ToolFailure::new(
                "invalid_cursor",
                "Cursor does not match the trace id or limit",
            ));
        }
        if input.duration_seconds.is_some() {
            return Err(ToolFailure::new(
                "invalid_cursor",
                "Omit duration_seconds when continuing a page",
            ));
        }
        if input.from.is_some() || input.to.is_some() {
            let (requested_from, requested_to) = resolve_period(input.from, input.to, None, false)?;
            if requested_from != cursor.from || requested_to != cursor.to {
                return Err(ToolFailure::new(
                    "invalid_cursor",
                    "Cursor is bound to different resolved time bounds",
                ));
            }
        }
        let core_cursor = cursor.core_cursor.clone();
        (
            cursor.from.clone(),
            cursor.to.clone(),
            cursor.limit,
            Some(core_cursor),
            cursor,
        )
    } else {
        let (from, to) = resolve_period(input.from, input.to, input.duration_seconds, false)?;
        let limit = input.limit.unwrap_or(100);
        if !(1..=1000).contains(&limit) {
            return Err(ToolFailure::new(
                "invalid_argument",
                "limit must be 1..1000",
            ));
        }
        let cursor = PageCursor {
            version: CURSOR_VERSION,
            from: from.clone(),
            to: to.clone(),
            limit,
            scope: access.node_scope,
            query: CursorQuery::Trace {
                trace_id: input.trace_id.clone(),
            },
            core_cursor: String::new(),
            expires_at_ms: store::now_ms().saturating_add(PAGE_TTL_MS),
        };
        (from, to, limit, None, cursor)
    };
    let response = app
        .core
        .trace(
            &input.trace_id,
            &TraceParams {
                from: from.clone(),
                to: to.clone(),
                limit,
                cursor: core_cursor,
            },
            access.node_scope,
        )
        .await
        .map_err(query_failure)?;
    let next_cursor = match response.next_cursor {
        Some(core_cursor) => {
            page_cursor.core_cursor = core_cursor;
            Some(encode_cursor(app, &page_cursor)?)
        }
        None => None,
    };
    Ok(json!({
        "trace_id": input.trace_id,
        "from": from,
        "to": to,
        "events": response.events,
        "next_cursor": next_cursor,
        "as_of": response.as_of,
        "order": "oldest_first",
    }))
}

async fn summarize(
    app: &AppState,
    access: &TokenAccess,
    input: SummarizeLogsInput,
) -> Result<Value, ToolFailure> {
    let (from, to) = resolve_period(input.from, input.to, input.duration_seconds, false)?;
    let filters = scoped_filters(input.filters.unwrap_or_default(), access.node_scope)?;
    let interval_seconds = input.interval_seconds.unwrap_or(60);
    match input.operation.as_str() {
        "summary" => {
            let response = app
                .core
                .summary(&SummaryRequest {
                    from,
                    to,
                    filters,
                    text: text_query(input.text)?,
                    interval_seconds,
                    search_limit: input.search_limit,
                })
                .await
                .map_err(query_failure)?;
            serde_json::to_value(response)
                .map_err(|_| ToolFailure::new("unavailable", "Core response could not be encoded"))
        }
        "group" => {
            let response = app
                .core
                .group_by(&GroupRequest {
                    from,
                    to,
                    filters,
                    group_by: input.group_by.unwrap_or_default(),
                    limit: input.limit.unwrap_or(100),
                })
                .await
                .map_err(query_failure)?;
            serde_json::to_value(response)
                .map_err(|_| ToolFailure::new("unavailable", "Core response could not be encoded"))
        }
        "histogram" => {
            let response = app
                .core
                .histogram(&HistogramRequest {
                    from,
                    to,
                    filters,
                    interval_seconds,
                })
                .await
                .map_err(query_failure)?;
            serde_json::to_value(response)
                .map_err(|_| ToolFailure::new("unavailable", "Core response could not be encoded"))
        }
        _ => Err(ToolFailure::new(
            "invalid_argument",
            "operation must be summary, group, or histogram",
        )),
    }
}

/// Фильтры поиска с узлом токена. Фильтр того же узла от пользователя отклоняется, а не переопределяется.
fn scoped_filters(mut filters: Filters, scope: Scope) -> Result<Filters, ToolFailure> {
    if let Some((key, id)) = scope.core_param() {
        if filters.contains_key(key) {
            return Err(ToolFailure::new(
                "invalid_argument",
                "filters cannot set the node of this token",
            ));
        }
        filters.insert(key.to_owned(), vec![id.to_string()]);
    }
    Ok(filters)
}

/// Фильтры источников: узел токена задаёт строковый `relay_id` или `agent_id`.
fn source_scope(scope: Scope) -> LogSourceFilters {
    let mut filters = LogSourceFilters::default();
    match scope {
        Scope::All => {}
        Scope::Relay { id } => filters.relay_id = Some(id.to_string()),
        Scope::Agent { id } => filters.agent_id = Some(id.to_string()),
    }
    filters
}

fn alert_definition(input: AlertInput) -> DefinitionInput {
    DefinitionInput {
        key: MetricKey {
            server: input.server,
            service: input.service,
            name: input.name,
            origin: input.origin,
        },
        // Узел задаёт сервис по токену, поэтому здесь значение не важно.
        node_scope: Scope::All,
        enabled: input.enabled,
        environment: input.environment,
        no_data: input.no_data,
        combine: input.combine,
        group_ids: input.group_ids,
        triggers: input
            .triggers
            .into_iter()
            .map(|trigger| TriggerInput {
                window_seconds: trigger.window_seconds,
                aggregate: trigger.aggregate,
                comparator: trigger.comparator,
                threshold: trigger.threshold,
            })
            .collect(),
        title: input.title,
        description: input.description,
        message_template: input.message_template,
        message_fields: input.message_fields,
    }
}

fn alert_failure(error: AlertToolError) -> ToolFailure {
    match error {
        AlertToolError::Invalid(message) => ToolFailure::new("invalid_argument", message),
        AlertToolError::Conflict("alert_exists") => ToolFailure::new(
            "conflict",
            "A metric alert with this key already exists in this scope",
        ),
        AlertToolError::Conflict("group_not_found") => ToolFailure::new(
            "group_not_found",
            "Recipient group does not exist; choose an existing group from list_alert_groups",
        ),
        AlertToolError::Conflict("global_disabled_or_alert_limit") => ToolFailure::new(
            "alerts_unavailable",
            "Metric alerts are disabled globally or the enabled alert limit is reached",
        ),
        AlertToolError::Conflict(_) => ToolFailure::new(
            "conflict",
            "The alert change conflicts with current settings",
        ),
        AlertToolError::NotFound => ToolFailure::new("not_found", "Metric alert was not found"),
        AlertToolError::Unavailable => {
            ToolFailure::new("unavailable", "Alert storage or Core is unavailable")
        }
    }
}

fn log_alert_failure(error: crate::log_alerts::LogAlertToolError) -> ToolFailure {
    use crate::log_alerts::LogAlertToolError;

    match error {
        LogAlertToolError::Invalid(message) => ToolFailure::new("invalid_argument", message),
        LogAlertToolError::Conflict("group_not_found") => ToolFailure::new(
            "group_not_found",
            "Recipient group does not exist; choose an existing group from list_alert_groups",
        ),
        LogAlertToolError::Conflict("log_alert_limit") => ToolFailure::new(
            "alerts_unavailable",
            "The enabled log alert limit has been reached",
        ),
        LogAlertToolError::Conflict(_) => ToolFailure::new(
            "conflict",
            "The log alert change conflicts with current settings",
        ),
        LogAlertToolError::NotFound => ToolFailure::new("not_found", "Log alert was not found"),
        LogAlertToolError::Unavailable => {
            ToolFailure::new("unavailable", "Log alert storage or Core is unavailable")
        }
    }
}

fn source_limit(limit: Option<usize>) -> Result<usize, ToolFailure> {
    let limit = limit.unwrap_or(100);
    if (1..=1000).contains(&limit) {
        Ok(limit)
    } else {
        Err(ToolFailure::new(
            "invalid_argument",
            "limit must be 1..1000",
        ))
    }
}

fn resolve_period(
    from: Option<String>,
    to: Option<String>,
    duration_seconds: Option<u64>,
    default_last_24h: bool,
) -> Result<(String, String), ToolFailure> {
    let period = match (from, to, duration_seconds) {
        (Some(from), Some(to), None) => (parse_rfc3339(&from)?, parse_rfc3339(&to)?),
        (None, None, Some(seconds)) if seconds > 0 && seconds <= i64::MAX as u64 => {
            let to = OffsetDateTime::now_utc();
            let from = to - Duration::seconds(seconds as i64);
            (from, to)
        }
        (None, None, None) if default_last_24h => {
            let to = OffsetDateTime::now_utc();
            (to - Duration::hours(24), to)
        }
        _ => {
            return Err(ToolFailure::new(
                "invalid_argument",
                "Provide both RFC3339 from and to, or a positive duration_seconds",
            ));
        }
    };
    if period.0 >= period.1 {
        return Err(ToolFailure::new(
            "invalid_argument",
            "from must be earlier than to",
        ));
    }
    Ok((format_time(period.0)?, format_time(period.1)?))
}

fn parse_rfc3339(value: &str) -> Result<OffsetDateTime, ToolFailure> {
    OffsetDateTime::parse(value, &Rfc3339)
        .map_err(|_| ToolFailure::new("invalid_argument", "Time bounds must be RFC3339 timestamps"))
}

fn format_time(value: OffsetDateTime) -> Result<String, ToolFailure> {
    value
        .to_offset(time::UtcOffset::UTC)
        .format(&Rfc3339)
        .map_err(|_| {
            ToolFailure::new(
                "invalid_argument",
                "Time bound is outside the RFC3339 range",
            )
        })
}

fn text_query(text: Option<McpText>) -> Result<Option<TextQuery>, ToolFailure> {
    text.map(|text| {
        let mode = match text.mode.as_deref() {
            None | Some("all") => TextMode::All,
            Some("phrase") => TextMode::Phrase,
            _ => {
                return Err(ToolFailure::new(
                    "invalid_argument",
                    "text.mode must be all or phrase",
                ));
            }
        };
        Ok(TextQuery {
            query: text.query,
            mode,
        })
    })
    .transpose()
}

fn encode_cursor(app: &AppState, cursor: &PageCursor) -> Result<String, ToolFailure> {
    encode_page_cursor(&app.mcp_cursor_key, cursor)
}

fn encode_page_cursor(key: &[u8; 32], cursor: &PageCursor) -> Result<String, ToolFailure> {
    let payload = serde_json::to_vec(cursor)
        .map_err(|_| ToolFailure::new("unavailable", "Cursor could not be encoded"))?;
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts the configured cursor key");
    mac.update(&payload);
    let signature = mac.finalize().into_bytes();
    let mut encoded = payload;
    encoded.extend_from_slice(&signature);
    Ok(URL_SAFE_NO_PAD.encode(encoded))
}

fn decode_cursor(app: &AppState, encoded: &str) -> Result<PageCursor, ToolFailure> {
    decode_page_cursor(&app.mcp_cursor_key, encoded)
}

fn decode_page_cursor(key: &[u8; 32], encoded: &str) -> Result<PageCursor, ToolFailure> {
    let bytes = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| ToolFailure::new("invalid_cursor", "Cursor is not valid"))?;
    if bytes.len() <= 32 {
        return Err(ToolFailure::new("invalid_cursor", "Cursor is not valid"));
    }
    let (payload, signature) = bytes.split_at(bytes.len() - 32);
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts the configured cursor key");
    mac.update(payload);
    mac.verify_slice(signature)
        .map_err(|_| ToolFailure::new("invalid_cursor", "Cursor signature is invalid"))?;
    let cursor: PageCursor = serde_json::from_slice(payload)
        .map_err(|_| ToolFailure::new("invalid_cursor", "Cursor is not valid"))?;
    if cursor.version != CURSOR_VERSION {
        return Err(ToolFailure::new(
            "invalid_cursor",
            "Cursor version is not supported",
        ));
    }
    if cursor.expires_at_ms <= store::now_ms() {
        return Err(ToolFailure::new(
            "cursor_expired",
            "Cursor expired; start a new query",
        ));
    }
    Ok(cursor)
}

fn success_result(value: Value) -> CallToolResult {
    let text = value.to_string();
    let result = CallToolResult::success(vec![ContentBlock::text(text)]);
    match serde_json::to_vec(&result) {
        Ok(encoded) if encoded.len() <= MAX_RESULT_BYTES - RESULT_HEADROOM => result,
        _ => failure_result(
            ToolFailure::new(
                "response_too_large",
                "The result exceeds the MCP 2 MiB response limit",
            )
            .with_guidance("Retry with a smaller limit or a larger metric step."),
        ),
    }
}

fn failure_result(error: ToolFailure) -> CallToolResult {
    let mut value = json!({ "error": error.code, "message": error.message });
    if let Some(guidance) = error.guidance {
        value["guidance"] = json!(guidance);
    }
    CallToolResult::error(vec![ContentBlock::text(value.to_string())])
}

fn query_failure(error: CoreQueryError) -> ToolFailure {
    match error {
        CoreQueryError::Invalid => ToolFailure::new("invalid_argument", "Core rejected the query"),
        CoreQueryError::Gone => ToolFailure::new(
            "cursor_expired",
            "Core query snapshot expired; start a new query",
        ),
        CoreQueryError::Unavailable | CoreQueryError::NotReady => {
            ToolFailure::new("unavailable", "Core query service is unavailable")
        }
        CoreQueryError::Timeout => ToolFailure::new("timeout", "Core query timed out"),
        CoreQueryError::Busy => {
            ToolFailure::new("busy", "Core query capacity is exhausted; retry later")
        }
        CoreQueryError::Unexpected => {
            ToolFailure::new("unavailable", "Core returned an unexpected response")
        }
        CoreQueryError::NotFound => {
            ToolFailure::new("not_found", "Requested log data was not found")
        }
    }
}

fn core_failure(error: CoreError) -> ToolFailure {
    match error {
        CoreError::Request(error) if error.is_timeout() => {
            ToolFailure::new("timeout", "Core query timed out")
        }
        CoreError::Request(_) => {
            ToolFailure::new("unavailable", "Core query service is unavailable")
        }
        CoreError::Status { status, .. } if status == StatusCode::GATEWAY_TIMEOUT => {
            ToolFailure::new("timeout", "Core query timed out")
        }
        CoreError::Status { status, detail } if status == StatusCode::SERVICE_UNAVAILABLE => {
            if detail
                .as_deref()
                .is_some_and(|detail| detail.contains("capacity is exhausted"))
            {
                ToolFailure::new("busy", "Core query capacity is exhausted; retry later")
            } else {
                ToolFailure::new("unavailable", "Core query service is unavailable")
            }
        }
        CoreError::Status { status, .. } if status == StatusCode::BAD_REQUEST => {
            ToolFailure::new("invalid_argument", "Core rejected the metric query")
        }
        CoreError::Status { status, .. } if status == StatusCode::GONE => ToolFailure::new(
            "cursor_expired",
            "Core query snapshot expired; start a new query",
        ),
        CoreError::Status { .. }
        | CoreError::Format
        | CoreError::Rejected(_, _)
        | CoreError::RenameUnsupported => {
            ToolFailure::new("unavailable", "Core returned an unexpected response")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_alert_message_settings_are_optional_and_forwarded() {
        let mut input = serde_json::json!({
            "name": "errors",
            "enabled": true,
            "query": "level:error",
            "threshold": 1,
            "cooldown_seconds": 300,
            "group_ids": []
        });
        let legacy: McpLogAlertInput = serde_json::from_value(input.clone()).unwrap();
        let legacy: crate::log_alerts::LogAlertInput = legacy.into();
        assert_eq!(legacy.message_template, None);
        assert_eq!(legacy.message_fields, None);

        input["message_template"] = serde_json::json!("<b>%name%</b>");
        input["message_fields"] = serde_json::json!(["name", "count"]);
        let customized: McpLogAlertInput = serde_json::from_value(input).unwrap();
        let customized: crate::log_alerts::LogAlertInput = customized.into();
        assert_eq!(
            customized.message_template.as_deref(),
            Some("<b>%name%</b>")
        );
        assert_eq!(
            customized.message_fields,
            Some(vec!["name".into(), "count".into()])
        );
    }

    #[test]
    fn registered_tools_match_the_fixed_allowlist_and_their_hints() {
        let mut names: Vec<String> = McpServer::tool_router()
            .list_all()
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect();
        names.sort();
        let mut expected: Vec<String> = ["get_versions"]
            .into_iter()
            .chain(LOG_TOOLS.iter().copied())
            .chain(METRIC_TOOLS.iter().copied())
            .chain(ALERT_GROUP_TOOLS.iter().copied())
            .chain(METRIC_ALERT_TOOLS.iter().copied())
            .chain(LOG_ALERT_TOOLS.iter().copied())
            .map(str::to_owned)
            .collect();
        expected.sort();
        assert_eq!(names, expected);
        assert_eq!(expected.len(), 18);
        for tool in McpServer::tool_router().list_all() {
            let annotations = tool.annotations.unwrap();
            let write = ALERT_WRITE_TOOLS.contains(&&*tool.name);
            assert_eq!(annotations.read_only_hint, Some(!write), "{}", tool.name);
            assert_eq!(
                annotations.destructive_hint,
                Some(matches!(&*tool.name, "delete_alert" | "delete_log_alert")),
                "{}",
                tool.name
            );
            assert_eq!(annotations.open_world_hint, Some(false));
        }
    }

    #[test]
    fn tool_rights_gate_the_allowlist_and_always_keep_get_versions() {
        let read_logs = TokenAccess {
            username: "alice".into(),
            rights: Rights {
                logs: true,
                metrics: false,
                alerts_write: false,
            },
            node_scope: Scope::All,
        };
        assert!(tool_allowed("search_logs", &read_logs));
        assert!(tool_allowed("get_versions", &read_logs));
        assert!(!tool_allowed("query_metrics", &read_logs));
        assert!(!tool_allowed("create_alert", &read_logs));
        assert!(!tool_allowed("create_log_alert", &read_logs));
        let writer = TokenAccess {
            rights: Rights {
                logs: true,
                metrics: false,
                alerts_write: true,
            },
            ..read_logs.clone()
        };
        assert!(tool_allowed("create_log_alert", &writer));
        assert!(tool_allowed("list_log_alerts", &writer));
        assert!(tool_allowed("update_log_alert", &writer));
        assert!(tool_allowed("delete_log_alert", &writer));
        assert!(tool_allowed("list_alert_groups", &writer));
        assert!(tool_allowed("search_logs", &writer));
        assert!(!tool_allowed("list_alerts", &writer));
        assert!(!tool_allowed("create_alert", &writer));
        assert!(!tool_allowed("delete_alert", &writer));

        let metric_writer = TokenAccess {
            rights: Rights {
                logs: false,
                metrics: true,
                alerts_write: true,
            },
            ..read_logs
        };
        assert!(tool_allowed("create_alert", &metric_writer));
        assert!(tool_allowed("list_alerts", &metric_writer));
        assert!(tool_allowed("update_alert", &metric_writer));
        assert!(tool_allowed("delete_alert", &metric_writer));
        assert!(!tool_allowed("list_log_alerts", &metric_writer));
        assert!(!tool_allowed("create_log_alert", &metric_writer));
        assert!(!tool_allowed("update_log_alert", &metric_writer));
        assert!(!tool_allowed("delete_log_alert", &metric_writer));
    }

    #[test]
    fn scope_filters_are_added_and_user_node_keys_are_rejected() {
        let filters = scoped_filters(Filters::new(), Scope::Relay { id: 7 }).unwrap();
        assert_eq!(filters.get("relay_id"), Some(&vec!["7".to_owned()]));
        assert!(!filters.contains_key("agent_id"));
        let user = Filters::from([("agent_id".to_owned(), vec!["3".to_owned()])]);
        assert!(scoped_filters(user.clone(), Scope::Relay { id: 7 }).is_ok());
        let conflict = Filters::from([("relay_id".to_owned(), vec!["8".to_owned()])]);
        assert_eq!(
            scoped_filters(conflict, Scope::Relay { id: 7 })
                .unwrap_err()
                .code,
            "invalid_argument"
        );
        assert!(
            scoped_filters(user, Scope::All)
                .unwrap()
                .contains_key("agent_id")
        );
        assert_eq!(
            source_scope(Scope::Agent { id: 4 }).agent_id.as_deref(),
            Some("4")
        );
    }

    #[test]
    fn search_periods_accept_offsets_and_freeze_relative_time() {
        let (from, to) = resolve_period(
            Some("2026-10-29T03:00:00+03:00".into()),
            Some("2026-10-29T01:00:00Z".into()),
            None,
            false,
        )
        .unwrap();
        assert_eq!(from, "2026-10-29T00:00:00Z");
        assert_eq!(to, "2026-10-29T01:00:00Z");

        let (from, to) = resolve_period(None, None, Some(3600), false).unwrap();
        let from = parse_rfc3339(&from).unwrap();
        let to = parse_rfc3339(&to).unwrap();
        assert_eq!((to - from).whole_seconds(), 3600);
        let (default_from, default_to) = resolve_period(None, None, None, true).unwrap();
        assert_eq!(
            (parse_rfc3339(&default_to).unwrap() - parse_rfc3339(&default_from).unwrap())
                .whole_seconds(),
            24 * 60 * 60
        );
        assert!(
            resolve_period(
                Some("bad".into()),
                Some("2026-10-29T01:00:00Z".into()),
                None,
                false
            )
            .is_err()
        );
        assert!(resolve_period(None, None, Some(0), false).is_err());
        assert!(
            resolve_period(
                Some("2026-10-29T01:00:00Z".into()),
                Some("2026-10-29T01:00:00Z".into()),
                None,
                false,
            )
            .is_err()
        );
    }

    #[test]
    fn signed_search_cursors_keep_resolved_bounds_and_expire_after_ten_minutes() {
        let key = [7; 32];
        let cursor = PageCursor {
            version: CURSOR_VERSION,
            from: "2026-10-29T00:00:00Z".into(),
            to: "2026-10-29T01:00:00Z".into(),
            limit: 100,
            scope: Scope::All,
            query: CursorQuery::Search {
                filters: Filters::default(),
                text: None,
            },
            core_cursor: "core-snapshot".into(),
            expires_at_ms: store::now_ms() + PAGE_TTL_MS,
        };
        let encoded = encode_page_cursor(&key, &cursor).unwrap();
        assert_eq!(
            decode_page_cursor(&key, &encoded).unwrap().from,
            cursor.from
        );
        let mut tampered = encoded.into_bytes();
        tampered[0] = if tampered[0] == b'A' { b'B' } else { b'A' };
        assert!(decode_page_cursor(&key, std::str::from_utf8(&tampered).unwrap()).is_err());

        let expired = PageCursor {
            expires_at_ms: store::now_ms() - 1,
            ..cursor
        };
        let encoded = encode_page_cursor(&key, &expired).unwrap();
        assert_eq!(
            decode_page_cursor(&key, &encoded).unwrap_err().code,
            "cursor_expired"
        );
    }

    #[test]
    fn response_size_failure_is_explicit() {
        let result = success_result(json!({ "message": "x".repeat(MAX_RESULT_BYTES) }));
        assert_eq!(result.is_error, Some(true));
        let text = match &result.content[0] {
            ContentBlock::Text(text) => text.text.as_str(),
            _ => panic!("expected text error"),
        };
        assert!(text.contains("response_too_large"));
        assert!(text.contains("larger metric step"));
    }

    #[tokio::test]
    async fn response_limit_includes_large_caller_controlled_id() {
        let id = json!("i".repeat(700_000));
        let result = success_result(json!({"message": "x".repeat(1_500_000)}));
        assert_ne!(result.is_error, Some(true));
        let response = Json(json!({"jsonrpc":"2.0", "id":id, "result":result})).into_response();
        let response = bound_response(response, id.clone()).await;
        let bytes = to_bytes(response.into_body(), MAX_RESULT_BYTES)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["id"], id);
        assert_eq!(body["result"]["isError"], true);
        assert!(
            body["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("response_too_large")
        );
    }

    #[test]
    fn limiter_enforces_per_user_inflight_rate_and_global_caps() {
        let limiter = McpLimiter::default();
        let first = limiter.try_enter("alice").unwrap();
        let second = limiter.try_enter("alice").unwrap();
        assert!(matches!(limiter.try_enter("alice"), Err(LimitError::Busy)));
        drop((first, second));
        let held = (0..8)
            .map(|index| limiter.try_enter(&format!("user-{index}")).unwrap())
            .collect::<Vec<_>>();
        assert!(matches!(limiter.try_enter("ninth"), Err(LimitError::Busy)));
        drop(held);
        for _ in 0..60 {
            let permit = limiter.try_enter("rate-test").unwrap();
            drop(permit);
        }
        assert!(matches!(
            limiter.try_enter("rate-test"),
            Err(LimitError::RateLimited)
        ));
    }
}
