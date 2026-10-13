//! Relay и агенты: прокси admin API core.

mod common;

use axum::http::{Method, StatusCode, header};
use common::*;
use serde_json::{Value, json};

const RELAY_TOKEN: &str = "lnr_0000000000000000000000000000000000000000000000000000000000000001";
const AGENT_TOKEN: &str = "lna_0000000000000000000000000000000000000000000000000000000000000002";

/// Dock, вошедший пользователь и поддельный core.
async fn setup() -> (TestApp, String, std::sync::Arc<FakeAdminCore>) {
    let (core, url) = fake_admin_core().await;
    let t = start(&[("liis", "password")], &[("LOGNARA_DOCK_CORE_URL", &url)]);
    let session = login(&t, "liis", "password").await.session();
    (t, session, core)
}

async fn call(
    t: &TestApp,
    session: &str,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> Reply {
    send(&t.router, request(method, path, Some(session), body)).await
}

#[tokio::test]
async fn proxies_relays_with_admin_token() {
    let (t, session, core) = setup().await;

    let list =
        json!({ "relays": [{ "id": 1, "name": "main", "created_at": "2026-10-14T10:00:00Z" }] });
    core.reply_json(StatusCode::OK, list.clone());
    let reply = call(&t, &session, Method::GET, "/api/relays", None).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body, list);

    let created = json!({
        "relay": { "id": 2, "name": "edge", "created_at": "2026-10-14T10:01:00Z" },
        "token": RELAY_TOKEN,
    });
    core.reply_json(StatusCode::CREATED, created.clone());
    let reply = call(
        &t,
        &session,
        Method::POST,
        "/api/relays",
        Some(json!({ "name": "edge" })),
    )
    .await;
    assert_eq!(reply.status, StatusCode::CREATED);
    assert_eq!(reply.body, created);
    assert_eq!(reply.headers[header::CACHE_CONTROL], "no-store");

    core.reply_raw(StatusCode::NO_CONTENT, "");
    let reply = call(&t, &session, Method::DELETE, "/api/relays/2", None).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);

    let requests = core.taken();
    let seen: Vec<_> = requests
        .iter()
        .map(|r| (r.method.as_str(), r.path.as_str()))
        .collect();
    assert_eq!(
        seen,
        [
            ("GET", "/v1/admin/relays"),
            ("POST", "/v1/admin/relays"),
            ("DELETE", "/v1/admin/relays/2"),
        ]
    );
    assert_eq!(requests[1].body, json!({ "name": "edge" }));
    for request in &requests {
        assert_eq!(
            request.authorization.as_deref(),
            Some("Bearer admin-secret")
        );
    }
}

#[tokio::test]
async fn proxies_agents_with_admin_token() {
    let (t, session, core) = setup().await;

    let list = json!({ "agents": [
        { "id": 3, "name": "api", "relay_id": 1, "created_at": "2026-10-14T10:00:00Z" },
    ] });
    core.reply_json(StatusCode::OK, list.clone());
    let reply = call(&t, &session, Method::GET, "/api/agents", None).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body, list);

    let created = json!({
        "agent": { "id": 4, "name": "worker", "relay_id": 1, "created_at": "2026-10-14T10:01:00Z" },
        "token": AGENT_TOKEN,
    });
    core.reply_json(StatusCode::CREATED, created.clone());
    let reply = call(
        &t,
        &session,
        Method::POST,
        "/api/agents",
        Some(json!({ "name": "worker", "relay_id": 1 })),
    )
    .await;
    assert_eq!(reply.status, StatusCode::CREATED);
    assert_eq!(reply.body, created);
    assert_eq!(reply.headers[header::CACHE_CONTROL], "no-store");

    core.reply_raw(StatusCode::NO_CONTENT, "");
    let reply = call(&t, &session, Method::DELETE, "/api/agents/4", None).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);

    let requests = core.taken();
    let seen: Vec<_> = requests
        .iter()
        .map(|r| (r.method.as_str(), r.path.as_str()))
        .collect();
    assert_eq!(
        seen,
        [
            ("GET", "/v1/admin/agents"),
            ("POST", "/v1/admin/agents"),
            ("DELETE", "/v1/admin/agents/4"),
        ]
    );
    assert_eq!(requests[1].body, json!({ "name": "worker", "relay_id": 1 }));
    for request in &requests {
        assert_eq!(
            request.authorization.as_deref(),
            Some("Bearer admin-secret")
        );
    }
}

#[tokio::test]
async fn requires_session() {
    let (t, _session, core) = setup().await;
    for (method, path, body) in [
        (Method::GET, "/api/relays", None),
        (Method::POST, "/api/relays", Some(json!({ "name": "a" }))),
        (Method::DELETE, "/api/relays/1", None),
        (Method::GET, "/api/agents", None),
        (
            Method::POST,
            "/api/agents",
            Some(json!({ "name": "a", "relay_id": 1 })),
        ),
        (Method::DELETE, "/api/agents/1", None),
    ] {
        let reply = send(&t.router, request(method, path, None, body)).await;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{path}");
    }
    assert!(core.taken().is_empty());
}

#[tokio::test]
async fn changes_require_origin() {
    let (t, session, core) = setup().await;
    for (method, path, body) in [
        (Method::POST, "/api/relays", json!({ "name": "a" })),
        (Method::DELETE, "/api/relays/1", Value::Null),
        (
            Method::POST,
            "/api/agents",
            json!({ "name": "a", "relay_id": 1 }),
        ),
        (Method::DELETE, "/api/agents/1", Value::Null),
    ] {
        let builder = axum::http::Request::builder()
            .method(method)
            .uri(path)
            .header(header::COOKIE, &session)
            .header(header::CONTENT_TYPE, "application/json");
        let body = if body.is_null() {
            axum::body::Body::empty()
        } else {
            axum::body::Body::from(body.to_string())
        };
        let reply = send(&t.router, builder.body(body).unwrap()).await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{path}");
    }
    assert!(core.taken().is_empty());
}

#[tokio::test]
async fn passes_known_core_errors_as_safe_codes() {
    let (t, session, core) = setup().await;
    for (core_status, core_code, status, code) in [
        (400, "invalid_name", 400, "invalid_name"),
        (400, "invalid_request", 400, "invalid_request"),
        (404, "relay_not_found", 404, "relay_not_found"),
        (404, "not_found", 404, "not_found"),
        (409, "name_taken", 409, "name_taken"),
        (409, "relay_has_agents", 409, "relay_has_agents"),
    ] {
        core.reply_json(
            StatusCode::from_u16(core_status).unwrap(),
            json!({ "error": core_code }),
        );
        let reply = call(
            &t,
            &session,
            Method::POST,
            "/api/agents",
            Some(json!({ "name": "a", "relay_id": 1 })),
        )
        .await;
        assert_eq!(reply.status.as_u16(), status, "{core_code}");
        assert_eq!(reply.body, json!({ "error": code }), "{core_code}");
    }
}

#[tokio::test]
async fn hides_unexpected_core_answers() {
    let (t, session, core) = setup().await;
    for (status, body) in [
        (401, r#"{"error":"unauthorized"}"#),
        (503, r#"{"error":"storage_unavailable"}"#),
        (500, "internal stack trace: /srv/core/secret.rs"),
        (409, r#"{"error":"something_new"}"#),
        (200, "not json"),
    ] {
        core.reply_raw(StatusCode::from_u16(status).unwrap(), body);
        let reply = call(&t, &session, Method::GET, "/api/relays", None).await;
        assert_eq!(reply.status, StatusCode::BAD_GATEWAY, "{status}");
        assert_eq!(
            reply.body,
            json!({ "error": "core_unavailable" }),
            "{status}"
        );
    }
}

#[tokio::test]
async fn reports_unreachable_core() {
    // Адрес по умолчанию в тестах (127.0.0.1:9) ничего не слушает.
    let t = start(&[("liis", "password")], &[]);
    let session = login(&t, "liis", "password").await.session();
    let reply = call(&t, &session, Method::GET, "/api/agents", None).await;
    assert_eq!(reply.status, StatusCode::BAD_GATEWAY);
    assert_eq!(reply.body, json!({ "error": "core_unavailable" }));
}

#[tokio::test]
async fn rejects_invalid_requests_without_calling_core() {
    let (t, session, core) = setup().await;
    for (method, path, body) in [
        (Method::POST, "/api/relays", json!({})),
        (
            Method::POST,
            "/api/relays",
            json!({ "name": "a", "extra": 1 }),
        ),
        (Method::POST, "/api/relays", json!({ "name": 5 })),
        (Method::POST, "/api/agents", json!({ "name": "a" })),
        (
            Method::POST,
            "/api/agents",
            json!({ "name": "a", "relay_id": "1" }),
        ),
    ] {
        let reply = call(&t, &session, method, path, Some(body)).await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{path}");
        assert_eq!(reply.body, json!({ "error": "invalid_request" }), "{path}");
    }
    for path in ["/api/relays/abc", "/api/agents/abc"] {
        let reply = call(&t, &session, Method::DELETE, path, None).await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{path}");
        assert_eq!(reply.body, json!({ "error": "invalid_request" }), "{path}");
    }
    assert!(core.taken().is_empty());
}
