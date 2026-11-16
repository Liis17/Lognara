//! Права и скоуп MCP-токенов: инструменты, запросы к core, курсоры, алерты и согласие OAuth.
mod common;

use std::sync::{Arc, Mutex};

use axum::body::{Body, to_bytes};
use axum::extract::{Request as AxumRequest, State};
use axum::http::{Method, Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use lognara_dock::api::AppState;
use lognara_dock::store::{self, PendingAuthorization};
use lognara_dock::token_access::{Permissions, Rights, Scope};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use tower::ServiceExt;

use common::{login, request, send, start};

const PROTOCOL: &str = "2025-03-26";
const RESOURCE: &str = "http://localhost:7403/mcp";
const CLIENT_ID: &str = "https://chatgpt.com/oauth/client.json";
const REDIRECT: &str = "https://chatgpt.com/connector_platform_oauth_redirect";
const VERIFIER: &str = "verifier-with-more-than-forty-three-characters-0123456789";

/// Запрос, который получил поддельный core.
#[derive(Clone, Debug)]
struct Seen {
    path: String,
    query: Option<String>,
    body: Value,
}

/// Поддельный core: записывает запросы и отвечает минимальными ответами по пути.
#[derive(Default)]
struct FakeCore {
    seen: Mutex<Vec<Seen>>,
}

impl FakeCore {
    fn count(&self) -> usize {
        self.seen.lock().unwrap().len()
    }

    fn last(&self, path: &str) -> Seen {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|seen| seen.path == path)
            .cloned()
            .unwrap_or_else(|| panic!("core did not receive {path}"))
    }
}

async fn core_handler(State(core): State<Arc<FakeCore>>, request: AxumRequest) -> Response {
    let (parts, body) = request.into_parts();
    let bytes = to_bytes(body, usize::MAX).await.unwrap();
    let path = parts.uri.path().to_owned();
    core.seen.lock().unwrap().push(Seen {
        path: path.clone(),
        query: parts.uri.query().map(str::to_owned),
        body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    });
    let reply = match (parts.method.as_str(), path.as_str()) {
        ("POST", "/v1/logs/sources") => {
            json!({"values": ["checkout"], "next_cursor": null, "as_of": "t"})
        }
        ("POST", "/v1/logs/search") => json!({"events": [], "next_cursor": "snap-1", "as_of": "t"}),
        ("GET", p) if p.starts_with("/v1/traces/") => {
            json!({"events": [], "next_cursor": null, "as_of": "t"})
        }
        ("POST", "/v1/stats/group-by") => json!({"groups": [], "as_of": "t"}),
        ("GET", "/v1/series") => json!({"metrics": []}),
        ("POST", "/v1/series/query") => json!({"kind": "delta", "unit": "requests", "series": []}),
        ("GET", "/v1/admin/relays") => {
            json!({"relays": [{"id": 1, "name": "relay-a", "created_at": "t"}]})
        }
        ("GET", "/v1/admin/agents") => json!({"agents": [
            {"id": 3, "name": "agent-x", "relay_id": 1, "created_at": "t"},
            {"id": 4, "name": "agent-y", "relay_id": 2, "created_at": "t"},
        ]}),
        _ => json!({"error": "unexpected"}),
    };
    (StatusCode::OK, Json(reply)).into_response()
}

async fn spawn_core() -> (Arc<FakeCore>, String) {
    let core = Arc::new(FakeCore::default());
    let router = Router::new()
        .fallback(core_handler)
        .with_state(core.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (core, url)
}

/// Dock с MCP, пользователем alice и поддельным core.
fn dock(core_url: &str) -> common::TestApp {
    start(
        &[("alice", "password-123")],
        &[
            ("LOGNARA_DOCK_MCP_ENABLED", "true"),
            ("LOGNARA_DOCK_CORE_URL", core_url),
        ],
    )
}

/// Создаёт токен пользователя alice с заданными правами; возвращает секрет.
async fn token(app: &AppState, id: &str, permissions: Permissions) -> String {
    let secret = format!("lnm_scope_{id}");
    let hash: [u8; 32] = Sha256::digest(secret.as_bytes()).into();
    app.store
        .create_personal_token(
            id.to_owned(),
            "alice".to_owned(),
            id.to_owned(),
            hash,
            store::now_ms() + 60 * 60 * 1000,
            permissions,
        )
        .await
        .unwrap();
    secret
}

fn all_rights() -> Permissions {
    Permissions {
        rights: Rights::default(),
        node_scope: Scope::All,
    }
}

fn node(scope: Scope) -> Permissions {
    Permissions {
        rights: Rights::default(),
        node_scope: scope,
    }
}

async fn mcp(router: &Router, token: &str, body: Value) -> Value {
    let request = Request::builder()
        .method(Method::POST)
        .uri("/mcp")
        .header("host", "localhost:7403")
        .header("origin", "https://chatgpt.com")
        .header(header::CONTENT_TYPE, "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("mcp-protocol-version", PROTOCOL)
        .header("mcp-method", body["method"].as_str().unwrap_or(""))
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = router.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), 2 * 1024 * 1024)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

async fn call(router: &Router, token: &str, name: &str, arguments: Value) -> Value {
    mcp(
        router,
        token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments},
        }),
    )
    .await
}

async fn tool_names(router: &Router, token: &str) -> Vec<String> {
    let reply = mcp(
        router,
        token,
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}}),
    )
    .await;
    let mut names: Vec<String> = reply["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_owned())
        .collect();
    names.sort();
    names
}

/// Текст результата инструмента как JSON; для ошибок — исходный текст.
fn payload(reply: &Value) -> Value {
    let text = reply["result"]["content"][0]["text"].as_str().unwrap();
    serde_json::from_str(text).unwrap_or(Value::String(text.to_owned()))
}

fn is_error(reply: &Value) -> bool {
    reply["result"]["isError"].as_bool() == Some(true)
}

const RANGE: &str = "2026-01-01T00:00:00Z";
const RANGE_END: &str = "2026-01-01T01:00:00Z";

#[tokio::test]
async fn token_rights_limit_tools_and_forbidden_calls_never_reach_core() {
    let (core, core_url) = spawn_core().await;
    let t = dock(&core_url);
    let logs_only = token(
        &t.app,
        "logs-only",
        Permissions {
            rights: Rights {
                logs: true,
                metrics: false,
                alerts_write: false,
            },
            node_scope: Scope::All,
        },
    )
    .await;
    let writer = token(
        &t.app,
        "writer",
        Permissions {
            rights: Rights {
                logs: false,
                metrics: true,
                alerts_write: true,
            },
            node_scope: Scope::All,
        },
    )
    .await;

    assert_eq!(
        tool_names(&t.router, &logs_only).await,
        [
            "get_trace",
            "get_versions",
            "list_applications",
            "list_environments",
            "list_servers",
            "search_logs",
            "summarize_logs",
        ]
    );
    assert_eq!(
        tool_names(&t.router, &writer).await,
        [
            "create_alert",
            "delete_alert",
            "get_versions",
            "list_alert_groups",
            "list_alerts",
            "list_metrics",
            "query_metrics",
            "update_alert",
        ]
    );

    let denied = call(
        &t.router,
        &logs_only,
        "query_metrics",
        json!({"service": "api", "name": "requests", "from": RANGE, "to": RANGE_END, "step_seconds": 60}),
    )
    .await;
    assert!(is_error(&denied), "{denied}");
    assert_eq!(payload(&denied)["error"], "forbidden");
    let denied_write = call(
        &t.router,
        &logs_only,
        "delete_alert",
        json!({"id": "00000000-0000-0000-0000-000000000000"}),
    )
    .await;
    assert!(is_error(&denied_write));
    assert_eq!(core.count(), 0, "forbidden calls must not reach core");
    t.app.shutdown.cancel();
}

#[tokio::test]
async fn node_scope_reaches_every_core_query_and_user_filters_cannot_override_it() {
    let (core, core_url) = spawn_core().await;
    let t = dock(&core_url);
    let relay = token(&t.app, "relay", node(Scope::Relay { id: 1 })).await;
    let agent = token(&t.app, "agent", node(Scope::Agent { id: 3 })).await;

    let sources = call(&t.router, &relay, "list_applications", json!({})).await;
    assert!(!is_error(&sources), "{sources}");
    assert_eq!(
        core.last("/v1/logs/sources").body["filters"]["relay_id"],
        "1"
    );

    let servers = call(&t.router, &agent, "list_servers", json!({})).await;
    assert!(!is_error(&servers), "{servers}");
    assert_eq!(
        core.last("/v1/logs/sources").body["filters"]["agent_id"],
        "3"
    );

    let search = call(
        &t.router,
        &relay,
        "search_logs",
        json!({"from": RANGE, "to": RANGE_END}),
    )
    .await;
    assert!(!is_error(&search), "{search}");
    assert_eq!(
        core.last("/v1/logs/search").body["filters"]["relay_id"],
        json!(["1"])
    );

    let trace = call(
        &t.router,
        &agent,
        "get_trace",
        json!({"trace_id": "abc", "from": RANGE, "to": RANGE_END}),
    )
    .await;
    assert!(!is_error(&trace), "{trace}");
    let trace_seen = core.last("/v1/traces/abc");
    assert!(
        trace_seen.query.as_deref().unwrap().contains("agent_id=3"),
        "{trace_seen:?}"
    );

    let summary = call(
        &t.router,
        &relay,
        "summarize_logs",
        json!({"operation": "group", "from": RANGE, "to": RANGE_END, "group_by": ["service"]}),
    )
    .await;
    assert!(!is_error(&summary), "{summary}");
    assert_eq!(
        core.last("/v1/stats/group-by").body["filters"]["relay_id"],
        json!(["1"])
    );

    let metrics = call(&t.router, &agent, "list_metrics", json!({})).await;
    assert!(!is_error(&metrics), "{metrics}");
    assert!(
        core.last("/v1/series")
            .query
            .as_deref()
            .unwrap()
            .contains("agent_id=3"),
    );

    let query = call(
        &t.router,
        &relay,
        "query_metrics",
        json!({"service": "api", "name": "requests", "from": RANGE, "to": RANGE_END, "step_seconds": 60}),
    )
    .await;
    assert!(!is_error(&query), "{query}");
    assert_eq!(core.last("/v1/series/query").body["relay_id"], 1);

    let before = core.count();
    let override_attempt = call(
        &t.router,
        &relay,
        "search_logs",
        json!({"from": RANGE, "to": RANGE_END, "filters": {"relay_id": ["2"]}}),
    )
    .await;
    assert!(is_error(&override_attempt), "{override_attempt}");
    assert_eq!(core.count(), before, "rejected filters must not reach core");
    t.app.shutdown.cancel();
}

#[tokio::test]
async fn cursor_from_another_scope_is_rejected() {
    let (_core, core_url) = spawn_core().await;
    let t = dock(&core_url);
    let all = token(&t.app, "all", all_rights()).await;
    let relay = token(&t.app, "relay-cursor", node(Scope::Relay { id: 1 })).await;

    let first = call(
        &t.router,
        &all,
        "search_logs",
        json!({"from": RANGE, "to": RANGE_END, "limit": 10}),
    )
    .await;
    assert!(!is_error(&first), "{first}");
    let cursor = payload(&first)["next_cursor"].as_str().unwrap().to_owned();

    let stolen = call(&t.router, &relay, "search_logs", json!({"cursor": cursor})).await;
    assert!(is_error(&stolen), "{stolen}");
    assert_eq!(payload(&stolen)["error"], "invalid_cursor");

    let own = call(&t.router, &all, "search_logs", json!({"cursor": cursor})).await;
    assert!(!is_error(&own), "{own}");
    t.app.shutdown.cancel();
}

#[tokio::test]
async fn alerts_over_mcp_are_created_within_scope_and_hidden_outside_it() {
    let (_core, core_url) = spawn_core().await;
    let t = dock(&core_url);
    // Инструменты алертов доступны только с правом записи.
    let writer = |node_scope| Permissions {
        rights: Rights {
            logs: true,
            metrics: true,
            alerts_write: true,
        },
        node_scope,
    };
    let all = token(&t.app, "alerts-all", writer(Scope::All)).await;
    let relay = token(&t.app, "alerts-relay", writer(Scope::Relay { id: 1 })).await;
    let agent = token(&t.app, "alerts-agent", writer(Scope::Agent { id: 3 })).await;

    // Группы создаёт только человек: через браузерный API с сессией.
    let session = login(&t, "alice", "password-123").await.session();
    let group = send(
        &t.router,
        request(
            Method::POST,
            "/api/alerts/groups",
            Some(session.as_str()),
            Some(json!({"kind": "personal", "name": "Team", "recipient_ids": []})),
        ),
    )
    .await;
    assert_eq!(group.status, StatusCode::CREATED, "{}", group.body);
    let group_id = group.body["id"].as_str().unwrap().to_owned();

    let groups = payload(&call(&t.router, &all, "list_alert_groups", json!({})).await);
    assert_eq!(groups["groups"][0]["id"], group_id);
    assert!(groups["groups"][0].get("chat_id").is_none());

    let rule = |server: &str, name: &str| {
        json!({
            "server": server, "service": "web", "name": name, "origin": "counter",
            "environment": null, "enabled": false, "no_data": "normal", "combine": "and",
            "group_ids": [group_id],
            "triggers": [{"window_seconds": 60, "aggregate": "sum", "comparator": ">", "threshold": 10.0}],
        })
    };
    let created_all = payload(&call(&t.router, &all, "create_alert", rule("s1", "requests")).await);
    let all_id = created_all["alert"]["id"].as_str().unwrap().to_owned();
    assert_eq!(created_all["alert"]["node_scope"]["kind"], "all");
    assert_eq!(
        created_all["alert"]["message_fields"],
        json!([
            "name",
            "description",
            "service",
            "server",
            "origin",
            "node",
            "environment",
            "condition",
            "value",
            "time",
            "last_point",
            "status"
        ])
    );
    assert!(created_all["alert"]["message_template"].is_null());

    let mut custom_rule = rule("s1", "custom");
    custom_rule["message_template"] = json!("Значение: %value%");
    custom_rule["message_fields"] = json!(["name", "value"]);
    let custom = payload(&call(&t.router, &all, "create_alert", custom_rule).await);
    let custom_id = custom["alert"]["id"].as_str().unwrap().to_owned();
    let legacy_update = payload(
        &call(
            &t.router,
            &all,
            "update_alert",
            json!({"id": custom_id, "alert": rule("s1", "custom")}),
        )
        .await,
    );
    assert_eq!(
        legacy_update["alert"]["message_template"],
        "Значение: %value%"
    );
    assert_eq!(
        legacy_update["alert"]["message_fields"],
        json!(["name", "value"])
    );

    let created_relay =
        payload(&call(&t.router, &relay, "create_alert", rule("s1", "errors")).await);
    let relay_id = created_relay["alert"]["id"].as_str().unwrap().to_owned();
    assert_eq!(created_relay["alert"]["node_scope"]["id"], 1);

    let created_agent =
        payload(&call(&t.router, &agent, "create_alert", rule("s1", "latency")).await);
    let agent_id = created_agent["alert"]["id"].as_str().unwrap().to_owned();
    assert_eq!(created_agent["alert"]["node_scope"]["kind"], "agent");

    // Агент 3 принадлежит relay 1: relay видит своё правило и правило агента, но не глобальное.
    let relay_view = payload(&call(&t.router, &relay, "list_alerts", json!({})).await);
    let mut relay_ids: Vec<String> = relay_view["alerts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|alert| alert["id"].as_str().unwrap().to_owned())
        .collect();
    relay_ids.sort();
    let mut expected = vec![relay_id.clone(), agent_id.clone()];
    expected.sort();
    assert_eq!(relay_ids, expected);

    let agent_view = payload(&call(&t.router, &agent, "list_alerts", json!({})).await);
    assert_eq!(agent_view["alerts"].as_array().unwrap().len(), 1);
    assert_eq!(agent_view["alerts"][0]["id"], agent_id);

    // Чужие правила не видны и не меняются: отказ выглядит как «не найдено».
    let hidden_update = call(
        &t.router,
        &relay,
        "update_alert",
        json!({"id": all_id, "alert": rule("s1", "requests")}),
    )
    .await;
    assert!(is_error(&hidden_update));
    assert_eq!(payload(&hidden_update)["error"], "not_found");
    let hidden_delete = call(&t.router, &agent, "delete_alert", json!({"id": relay_id})).await;
    assert!(is_error(&hidden_delete));

    // Ключ и скоуп уникальны: глобальный алерт с тем же ключом не перезаписывается.
    let duplicate = call(&t.router, &all, "create_alert", rule("s1", "requests")).await;
    assert!(is_error(&duplicate));
    assert_eq!(payload(&duplicate)["error"], "conflict");

    // Группа должна существовать.
    let mut unknown_group = rule("s2", "requests");
    unknown_group["group_ids"] = json!(["missing-group"]);
    let missing = call(&t.router, &all, "create_alert", unknown_group).await;
    assert!(is_error(&missing));
    assert_eq!(payload(&missing)["error"], "group_not_found");

    // Обновление сохраняет скоуп правила: токен `all` меняет правило relay, но узел остаётся relay.
    let updated = payload(
        &call(
            &t.router,
            &all,
            "update_alert",
            json!({"id": relay_id, "alert": rule("s1", "errors-renamed")}),
        )
        .await,
    );
    assert_eq!(updated["alert"]["node_scope"]["kind"], "relay");
    assert_eq!(updated["alert"]["key"]["name"], "errors-renamed");

    // Глобальное правило в браузерном API по ключу остаётся отдельным от скоупленного.
    let browser_global = send(
        &t.router,
        request(
            Method::GET,
            "/api/alerts/definition?server=s1&service=web&name=errors-renamed&origin=counter",
            Some(session.as_str()),
            None,
        ),
    )
    .await;
    assert_eq!(browser_global.status, StatusCode::OK);
    assert!(
        browser_global.body["alert"].is_null(),
        "{}",
        browser_global.body
    );

    let deleted = call(&t.router, &relay, "delete_alert", json!({"id": relay_id})).await;
    assert!(!is_error(&deleted), "{deleted}");
    t.app.shutdown.cancel();
}

#[tokio::test]
async fn oauth_consent_chooses_rights_and_the_token_reports_the_granted_scope() {
    let (_core, core_url) = spawn_core().await;
    let t = dock(&core_url);
    let session = login(&t, "alice", "password-123").await.session();
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(VERIFIER.as_bytes()));
    let pending = |id: &str| PendingAuthorization {
        id: id.to_owned(),
        username: "alice".to_owned(),
        client_id: CLIENT_ID.to_owned(),
        client_name: "ChatGPT".to_owned(),
        redirect_uri: REDIRECT.to_owned(),
        state: "state-1".to_owned(),
        scope: "lognara.read".to_owned(),
        resource: RESOURCE.to_owned(),
        code_challenge: challenge.clone(),
        expires_at: store::now_ms() + 300_000,
    };

    // Право записи выбирает пользователь; без метрик запись запрещена.
    t.app
        .store
        .create_oauth_pending(pending("consent-bad"))
        .await
        .unwrap();
    let invalid = send(
        &t.router,
        request(
            Method::POST,
            "/api/oauth/consent/consent-bad",
            Some(session.as_str()),
            Some(json!({"allow": true, "logs": false, "metrics": false, "alerts_write": true})),
        ),
    )
    .await;
    assert_eq!(invalid.status, StatusCode::BAD_REQUEST);

    t.app
        .store
        .create_oauth_pending(pending("consent-ok"))
        .await
        .unwrap();
    let decision = send(
        &t.router,
        request(
            Method::POST,
            "/api/oauth/consent/consent-ok",
            Some(session.as_str()),
            Some(json!({"allow": true, "logs": true, "metrics": true, "alerts_write": true, "node_scope": {"kind": "all"}})),
        ),
    )
    .await;
    assert_eq!(decision.status, StatusCode::OK, "{}", decision.body);
    let redirect = url::Url::parse(decision.body["redirect_to"].as_str().unwrap()).unwrap();
    let code = redirect
        .query_pairs()
        .find(|(key, _)| key == "code")
        .map(|(_, value)| value.into_owned())
        .expect("consent returns a code");

    let form = format!(
        "grant_type=authorization_code&client_id={CLIENT_ID}&resource={RESOURCE}&code={code}&redirect_uri={REDIRECT}&code_verifier={VERIFIER}"
    );
    let token_request = Request::builder()
        .method(Method::POST)
        .uri("/oauth/token")
        .header("host", "localhost:7403")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(form))
        .unwrap();
    let response = t.router.clone().oneshot(token_request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
    let issued: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(issued["scope"], "lognara.read lognara.write");
    let access = issued["access_token"].as_str().unwrap().to_owned();

    let names = tool_names(&t.router, &access).await;
    assert!(names.contains(&"create_alert".to_owned()), "{names:?}");
    assert!(names.contains(&"search_logs".to_owned()), "{names:?}");

    // Отказ не выдаёт код.
    t.app
        .store
        .create_oauth_pending(pending("consent-deny"))
        .await
        .unwrap();
    let denied = send(
        &t.router,
        request(
            Method::POST,
            "/api/oauth/consent/consent-deny",
            Some(session.as_str()),
            Some(json!({"allow": false})),
        ),
    )
    .await;
    assert_eq!(denied.status, StatusCode::OK);
    let redirect = denied.body["redirect_to"].as_str().unwrap();
    assert!(redirect.contains("error=access_denied"), "{redirect}");
    assert!(!redirect.contains("code="));
    t.app.shutdown.cancel();
}
