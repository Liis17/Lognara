//! Паттерны: прокси к core, персональное скрытие и безопасные ошибки.

mod common;

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri, header};
use common::*;
use serde_json::{Value, json};

const FROM: &str = "2026-10-14T10:00:00Z";
const TO: &str = "2026-10-14T11:00:00Z";

fn id(n: u32) -> String {
    format!("p1_{n:064x}")
}

struct FakeCore {
    requests: Mutex<Vec<(Uri, String, Value)>>,
    reply: Mutex<(StatusCode, String)>,
}

async fn record(
    State(core): State<Arc<FakeCore>>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> (StatusCode, String) {
    core.requests.lock().unwrap().push((
        uri,
        headers[header::AUTHORIZATION].to_str().unwrap().to_owned(),
        serde_json::from_slice(&body).unwrap_or(Value::Null),
    ));
    core.reply.lock().unwrap().clone()
}

async fn setup() -> (TestApp, String, String, Arc<FakeCore>) {
    let core = Arc::new(FakeCore {
        requests: Mutex::default(),
        reply: Mutex::new((StatusCode::OK, response(&[]).to_string())),
    });
    let router = Router::new().fallback(record).with_state(core.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let t = start(
        &[("liis", "password"), ("other", "password")],
        &[("LOGNARA_DOCK_CORE_URL", &url)],
    );
    let liis = login(&t, "liis", "password").await.session();
    let other = login(&t, "other", "password").await.session();
    (t, liis, other, core)
}

fn pattern(pattern_id: &str) -> Value {
    json!({
        "pattern_id": pattern_id, "environment": null, "service": "billing", "level": "error",
        "template": "payment id=<id_1> failed",
        "parts": [{"kind": "literal", "text": "payment id="}, {"kind": "variable", "name": "id_1", "variable_type": "id"}, {"kind": "literal", "text": " failed"}],
        "exact": false, "count": 3, "share": 0.75, "previous_count": 0, "growth_percent": null,
        "is_new": null, "first_seen": FROM, "last_seen": FROM,
        "buckets": [{"timestamp": FROM, "count": 3}]
    })
}

fn response(ids: &[&str]) -> Value {
    json!({
        "algorithm_version": 1, "as_of": TO, "watermark": 4, "core_watermark": 5,
        "history_from": null, "history_complete": false, "total_events": 4,
        "total_patterns": ids.len() + 1, "visible_patterns": ids.len(), "new_patterns": null,
        "patterns": ids.iter().map(|id| pattern(id)).collect::<Vec<_>>()
    })
}

async fn call(
    t: &TestApp,
    method: Method,
    path: &str,
    session: &str,
    body: Option<Value>,
) -> Reply {
    send(&t.router, request(method, path, Some(session), body)).await
}

#[tokio::test]
async fn hidden_patterns_are_personal_idempotent_and_limited() {
    let (t, liis, other, core) = setup().await;
    let hide = format!("/api/patterns/{}/hide", id(1));
    for _ in 0..2 {
        let reply = call(&t, Method::POST, &hide, &liis, None).await;
        assert_eq!(reply.status, StatusCode::NO_CONTENT);
    }
    let hidden = call(&t, Method::GET, "/api/patterns/hidden", &liis, None).await;
    assert_eq!(hidden.body, json!({"pattern_ids": [id(1)]}));
    assert_eq!(hidden.headers[header::CACHE_CONTROL], "no-store");
    let foreign = call(&t, Method::GET, "/api/patterns/hidden", &other, None).await;
    assert_eq!(foreign.body, json!({"pattern_ids": []}));

    let mut forged = request(Method::DELETE, &hide, Some(&liis), None);
    forged.headers_mut().remove(header::ORIGIN);
    assert_eq!(send(&t.router, forged).await.status, StatusCode::FORBIDDEN);
    for path in [
        "/api/patterns/p1_XYZ/hide",
        "/api/patterns/'%20OR%201=1/hide",
    ] {
        let reply = call(&t, Method::POST, path, &liis, None).await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{path}");
    }
    let anonymous = send(
        &t.router,
        request(Method::GET, "/api/patterns/hidden", None, None),
    )
    .await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);

    for n in 2..=1000 {
        let path = format!("/api/patterns/{}/hide", id(n));
        assert_eq!(
            call(&t, Method::POST, &path, &liis, None).await.status,
            StatusCode::NO_CONTENT
        );
    }
    let full = call(
        &t,
        Method::POST,
        &format!("/api/patterns/{}/hide", id(1001)),
        &liis,
        None,
    )
    .await;
    assert_eq!(
        (full.status, full.body),
        (StatusCode::CONFLICT, json!({"error": "pattern_limit"}))
    );
    assert_eq!(
        call(&t, Method::POST, &hide, &liis, None).await.status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(&t, Method::DELETE, &hide, &liis, None).await.status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(&t, Method::DELETE, &hide, &liis, None).await.status,
        StatusCode::NO_CONTENT
    );
    let reply = call(&t, Method::GET, "/api/patterns/hidden", &liis, None).await;
    assert_eq!(reply.body["pattern_ids"].as_array().unwrap().len(), 999);
    assert!(core.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn search_excludes_hidden_patterns_unless_requested() {
    let (t, liis, _, core) = setup().await;
    call(
        &t,
        Method::POST,
        &format!("/api/patterns/{}/hide", id(2)),
        &liis,
        None,
    )
    .await;
    let body = json!({"from": FROM, "to": TO, "filters": {"service": ["billing"]},
        "text": {"query": "payment"}, "interval_seconds": 60, "limit": 50, "sort": "growth",
        "exclude_patterns": [id(3), id(2)]});
    *core.reply.lock().unwrap() = (StatusCode::OK, response(&[&id(1)]).to_string());
    let reply = call(
        &t,
        Method::POST,
        "/api/patterns/search",
        &liis,
        Some(body.clone()),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body, response(&[&id(1)]));
    let reply = call(
        &t,
        Method::POST,
        "/api/patterns/search?show_hidden=true",
        &liis,
        Some(body),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    let requests = std::mem::take(&mut *core.requests.lock().unwrap());
    assert_eq!(requests[0].0.path(), "/v1/patterns/search");
    assert_eq!(requests[0].1, "Bearer query-secret");
    assert_eq!(requests[0].2["exclude_patterns"], json!([id(2), id(3)]));
    assert_eq!(requests[0].2["sort"], "growth");
    assert_eq!(requests[1].2["exclude_patterns"], json!([id(3), id(2)]));

    // Скрытый ID в ответе core или нарушенные суммы не доходят до браузера.
    *core.reply.lock().unwrap() = (StatusCode::OK, response(&[&id(2)]).to_string());
    let reply = call(
        &t,
        Method::POST,
        "/api/patterns/search",
        &liis,
        Some(json!({"from": FROM, "to": TO})),
    )
    .await;
    assert_eq!(reply.body, json!({"error": "core_unavailable"}));
    let mut broken = response(&[&id(1)]);
    broken["patterns"][0]["buckets"][0]["count"] = json!(2);
    *core.reply.lock().unwrap() = (StatusCode::OK, broken.to_string());
    let reply = call(
        &t,
        Method::POST,
        "/api/patterns/search",
        &liis,
        Some(json!({"from": FROM, "to": TO})),
    )
    .await;
    assert_eq!(reply.status, StatusCode::BAD_GATEWAY);
    for body in [
        json!({"from": FROM, "to": TO, "limit": 101}),
        json!({"from": FROM, "to": TO, "exclude_patterns": ["x"]}),
        json!({"from": FROM, "to": TO, "unknown": 1}),
    ] {
        let reply = call(&t, Method::POST, "/api/patterns/search", &liis, Some(body)).await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    }
    assert_eq!(core.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn details_proxy_and_pattern_specific_errors() {
    let (t, liis, _, core) = setup().await;
    let path = format!("/api/patterns/{}/details", id(7));
    let details = json!({"as_of": TO, "pattern": pattern(&id(7)),
        "variables": [{"name": "id_1", "variable_type": "id", "total": 3, "values": [{"value": "42", "count": 2}], "other_count": 1}],
        "examples": []});
    *core.reply.lock().unwrap() = (StatusCode::OK, details.to_string());
    let reply = call(
        &t,
        Method::POST,
        &path,
        &liis,
        Some(json!({"from": FROM, "to": TO})),
    )
    .await;
    assert_eq!((reply.status, reply.body), (StatusCode::OK, details));
    assert_eq!(
        core.requests.lock().unwrap()[0].0.path(),
        format!("/v1/patterns/{}/details", id(7))
    );
    for (status, body, expected, code) in [
        (
            404,
            json!({"error": "pattern not found in range"}),
            404,
            "pattern_not_found",
        ),
        (
            503,
            json!({"error": "pattern index is not ready"}),
            503,
            "patterns_not_ready",
        ),
        (
            503,
            json!({"error": "query capacity is exhausted"}),
            503,
            "query_unavailable",
        ),
        (400, json!({"error": "secret detail"}), 400, "invalid_query"),
    ] {
        *core.reply.lock().unwrap() = (StatusCode::from_u16(status).unwrap(), body.to_string());
        let reply = call(
            &t,
            Method::POST,
            &path,
            &liis,
            Some(json!({"from": FROM, "to": TO})),
        )
        .await;
        assert_eq!(
            (reply.status.as_u16(), reply.body),
            (expected, json!({"error": code}))
        );
    }
    *core.reply.lock().unwrap() = (
        StatusCode::OK,
        json!({"as_of": TO, "pattern": pattern(&id(8)), "variables": [], "examples": []})
            .to_string(),
    );
    let reply = call(
        &t,
        Method::POST,
        &path,
        &liis,
        Some(json!({"from": FROM, "to": TO})),
    )
    .await;
    assert_eq!(reply.status, StatusCode::BAD_GATEWAY);
}
