use axum::{Router, body::Body, body::to_bytes, http::Request, routing::get};
use lognara_dock::{api, api::App, config::Config, store};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use tempfile::tempdir;
use tower::ServiceExt;

const USER: &str = "alice";
const TOKEN: &str = "lnm_protocol_fixture_secret";
const PROTOCOL: &str = "2025-03-26";

async fn post_mcp(
    router: &Router,
    body: Value,
    token: Option<&str>,
    cookie: bool,
) -> (u16, Value, Option<String>) {
    let mut request = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("host", "dock.example.com")
        .header("origin", "https://chatgpt.com")
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("mcp-protocol-version", PROTOCOL)
        .header("mcp-method", body["method"].as_str().unwrap_or(""));
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    if cookie {
        request = request.header("cookie", "lognara_dock_session=browser-session");
    }
    let response = router
        .clone()
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status().as_u16();
    let challenge = response
        .headers()
        .get("www-authenticate")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let bytes = to_bytes(response.into_body(), 2 * 1024 * 1024)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        challenge,
    )
}

fn config(data_dir: &std::path::Path, htpasswd: &std::path::Path, core_url: String) -> Config {
    let values = HashMap::from([
        (
            "LOGNARA_DOCK_PUBLIC_URL",
            "https://dock.example.com".to_owned(),
        ),
        ("LOGNARA_DOCK_CORE_URL", core_url),
        ("LOGNARA_DOCK_CORE_TOKEN", "query-secret".to_owned()),
        ("LOGNARA_DOCK_CORE_ADMIN_TOKEN", "admin-secret".to_owned()),
        ("LOGNARA_DOCK_MCP_ENABLED", "true".to_owned()),
        (
            "LOGNARA_DOCK_HTPASSWD_FILE",
            htpasswd.to_string_lossy().into_owned(),
        ),
        (
            "LOGNARA_DOCK_DATA_DIR",
            data_dir.to_string_lossy().into_owned(),
        ),
    ]);
    Config::from_lookup(|key| values.get(key).cloned()).unwrap()
}

fn core_status() -> Value {
    json!({
        "version": "0.7.0",
        "release_version": "0.7.4",
        "git_commit": "0123456789abcdef",
        "ready": true,
        "visibility_lag_ms": 0,
        "wal_bytes": 0,
        "wal_limit_bytes": 1024,
        "storage": {
            "segments": 0,
            "parquet_bytes": 0,
            "index_bytes": 0,
            "oldest_event_at": null,
            "retention_seconds": 0,
            "disk_total_bytes": 1024,
            "disk_free_bytes": 1024,
        }
    })
}

async fn add_token(app: &lognara_dock::api::AppState, id: &str, expires_at: i64) -> [u8; 32] {
    let hash: [u8; 32] = Sha256::digest(id.as_bytes()).into();
    app.store
        .create_personal_token(
            id.to_owned(),
            USER.to_owned(),
            "protocol test".to_owned(),
            hash,
            expires_at,
        )
        .await
        .unwrap();
    hash
}

#[tokio::test]
async fn stateless_mcp_initializes_lists_tools_and_calls_get_versions_with_bearer_only_auth() {
    let dir = tempdir().unwrap();
    let htpasswd = dir.path().join("htpasswd");
    std::fs::write(&htpasswd, format!("{USER}:$2y$04$fixture\n")).unwrap();

    let core_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let core_addr = core_listener.local_addr().unwrap();
    let core_router =
        Router::new().route("/v1/status", get(|| async { axum::Json(core_status()) }));
    let core_server = tokio::spawn(async move {
        axum::serve(core_listener, core_router).await.unwrap();
    });

    let config = config(
        &dir.path().join("dock-data"),
        &htpasswd,
        format!("http://{core_addr}/"),
    );
    let app = tokio::task::spawn_blocking(move || App::open(config))
        .await
        .unwrap()
        .unwrap();
    let valid_hash = add_token(&app, TOKEN, store::now_ms() + 30 * 24 * 60 * 60 * 1000).await;
    let expired_token = "lnm_expired_fixture";
    add_token(&app, expired_token, store::now_ms() - 1).await;
    let revoked_token = "lnm_revoked_fixture";
    add_token(&app, revoked_token, store::now_ms() + 60_000).await;
    assert!(
        app.store
            .revoke_personal_token(USER.to_owned(), revoked_token.to_owned())
            .await
            .unwrap()
    );

    let router = api::router(app.clone());
    let (status, response, _) = post_mcp(
        &router,
        json!({
            "jsonrpc":"2.0",
            "id":1,
            "method":"initialize",
            "params":{
                "protocolVersion":PROTOCOL,
                "capabilities":{},
                "clientInfo":{"name":"protocol-test","version":"1"}
            }
        }),
        Some(TOKEN),
        false,
    )
    .await;
    assert_eq!(status, 200, "{response}");
    assert_eq!(response["result"]["protocolVersion"], PROTOCOL);
    assert!(response["result"]["capabilities"]["tools"].is_object());

    let (status, tools_response, _) = post_mcp(
        &router,
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
        Some(TOKEN),
        false,
    )
    .await;
    assert_eq!(status, 200, "{tools_response}");
    let tools = tools_response["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 9);
    assert!(tools.iter().any(|tool| tool["name"] == "get_versions"));
    for tool in tools {
        assert_eq!(tool["annotations"]["readOnlyHint"], true);
        assert_eq!(tool["annotations"]["destructiveHint"], false);
        assert_eq!(tool["securitySchemes"][0]["type"], "oauth2");
    }

    let (status, versions, _) = post_mcp(
        &router,
        json!({
            "jsonrpc":"2.0",
            "id":3,
            "method":"tools/call",
            "params":{"name":"get_versions","arguments":{}}
        }),
        Some(TOKEN),
        false,
    )
    .await;
    assert_eq!(status, 200, "{versions}");
    assert_eq!(versions["result"]["isError"].as_bool(), Some(false));
    let text = versions["result"]["content"][0]["text"].as_str().unwrap();
    let versions: Value = serde_json::from_str(text).unwrap();
    assert_eq!(versions["core"]["version"], "0.7.0");
    assert_eq!(versions["core"]["release_version"], "0.7.4");
    assert_eq!(versions["core"]["git_commit"], "0123456789abcdef");

    let (status, _, challenge) = post_mcp(
        &router,
        json!({"jsonrpc":"2.0","id":4,"method":"tools/list","params":{}}),
        None,
        true,
    )
    .await;
    assert_eq!(
        status, 401,
        "browser session cookies must not authorize MCP"
    );
    let challenge = challenge.unwrap();
    assert!(challenge.contains("oauth-protected-resource/mcp"));
    assert!(challenge.contains("scope=\"lognara.read\""));
    let (status, _, _) = post_mcp(
        &router,
        json!({"jsonrpc":"2.0","id":5,"method":"tools/list","params":{}}),
        Some(expired_token),
        false,
    )
    .await;
    assert_eq!(status, 401);
    let (status, _, _) = post_mcp(
        &router,
        json!({"jsonrpc":"2.0","id":6,"method":"tools/list","params":{}}),
        Some(revoked_token),
        false,
    )
    .await;
    assert_eq!(status, 401);

    std::fs::write(&htpasswd, "").unwrap();
    let (status, _, _) = post_mcp(
        &router,
        json!({"jsonrpc":"2.0","id":7,"method":"tools/list","params":{}}),
        Some(TOKEN),
        false,
    )
    .await;
    assert_eq!(status, 401, "users removed from htpasswd lose MCP access");
    assert!(
        app.store
            .authenticate_mcp_token(valid_hash, "https://dock.example.com/mcp".to_owned())
            .await
            .unwrap()
            .is_none()
    );

    app.shutdown.cancel();
    core_server.abort();
}
