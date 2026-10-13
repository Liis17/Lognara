//! История, trace и аналитика против поддельного query API core.

mod common;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, Method, Request, StatusCode, Uri, header};
use common::*;
use serde_json::{Value, json};

const FROM: &str = "2026-10-14T10:00:00.000000001Z";
const TO: &str = "2026-10-14T11:00:00Z";
const TRACE_ID: &str = "0123456789ABCDEF0123456789ABCDEF";

#[derive(Debug)]
struct Recorded {
    method: Method,
    uri: Uri,
    authorization: String,
    body: Value,
}

struct FakeCore {
    requests: Mutex<Vec<Recorded>>,
    reply: Mutex<(StatusCode, String)>,
    delay: Mutex<Duration>,
}

impl FakeCore {
    fn reply(&self, status: StatusCode, body: Value) {
        *self.reply.lock().unwrap() = (status, body.to_string());
    }

    fn taken(&self) -> Vec<Recorded> {
        std::mem::take(&mut *self.requests.lock().unwrap())
    }
}

async fn record(
    State(core): State<Arc<FakeCore>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> (StatusCode, String) {
    core.requests.lock().unwrap().push(Recorded {
        method,
        uri,
        authorization: headers[header::AUTHORIZATION].to_str().unwrap().to_owned(),
        body: serde_json::from_slice(&body).unwrap_or(Value::Null),
    });
    let reply = core.reply.lock().unwrap().clone();
    let delay = *core.delay.lock().unwrap();
    tokio::time::sleep(delay).await;
    reply
}

async fn setup() -> (TestApp, String, Arc<FakeCore>) {
    let core = Arc::new(FakeCore {
        requests: Mutex::default(),
        reply: Mutex::new((StatusCode::OK, page().to_string())),
        delay: Mutex::new(Duration::ZERO),
    });
    let router = Router::new().fallback(record).with_state(core.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let t = start(&[("liis", "password")], &[("LOGNARA_DOCK_CORE_URL", &url)]);
    let session = login(&t, "liis", "password").await.session();
    (t, session, core)
}

fn range() -> Value {
    json!({"from": FROM, "to": TO})
}

fn page() -> Value {
    json!({
        "events": [{
            "id": "0199e342-c780-7000-8000-000000000001",
            "timestamp": "2026-10-14T10:00:01.000000002Z",
            "ingested_at": "2026-10-14T10:00:02.000000003Z",
            "environment": "prod", "server": "host-1", "backend": "api", "service": "billing",
            "service_instance": null, "action": "pay", "level": "error", "message": "Ошибка платежа",
            "trace_id": TRACE_ID.to_ascii_lowercase(), "span_id": null, "parent_span_id": null,
            "request_id": "req-1", "attributes": {"nested": {"values": [true, null, 1]}, "duration_ms": 2.5}
        }],
        "next_cursor": "opaque+/=&?", "as_of": "2026-10-14T11:00:00.000000004Z"
    })
}

fn endpoints() -> Vec<(Method, String, Option<Value>)> {
    vec![
        (Method::POST, "/api/logs/search".into(), Some(range())),
        (
            Method::GET,
            format!("/api/traces/{TRACE_ID}?from={FROM}&to={TO}"),
            None,
        ),
        (Method::POST, "/api/stats/histogram".into(), Some(range())),
        (
            Method::POST,
            "/api/stats/group-by".into(),
            Some(json!({"from": FROM, "to": TO, "group_by": ["service"]})),
        ),
    ]
}

#[tokio::test]
async fn search_preserves_filters_text_cursor_and_full_page() {
    let (t, session, core) = setup().await;
    let body = json!({
        "from": FROM, "to": TO,
        "filters": {"service": ["billing", "checkout"], "level": ["error", "warn"]},
        "text": {"query": "payment failed", "mode": "phrase"},
        "limit": 50, "cursor": "opaque+/=&?"
    });
    let reply = send(
        &t.router,
        request(
            Method::POST,
            "/api/logs/search",
            Some(&session),
            Some(body.clone()),
        ),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body, page());
    assert_eq!(reply.headers[header::CACHE_CONTROL], "no-store");
    let recorded = core.taken().pop().unwrap();
    assert_eq!(recorded.method, Method::POST);
    assert_eq!(recorded.uri.path(), "/v1/logs/search");
    assert_eq!(recorded.authorization, "Bearer query-secret");
    assert_eq!(recorded.body, body);

    let reply = send(
        &t.router,
        request(
            Method::POST,
            "/api/logs/search",
            Some(&session),
            Some(json!({"from": FROM, "to": TO, "text": {"query": "payment"}})),
        ),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(
        core.taken()[0].body,
        json!({"from": FROM, "to": TO, "filters": {}, "text": {"query": "payment", "mode": "all"}, "limit": 100, "cursor": null})
    );
}

#[tokio::test]
async fn trace_encodes_query_parameters_and_keeps_server_order() {
    let (t, session, core) = setup().await;
    let mut expected = page();
    let mut earlier = expected["events"][0].clone();
    earlier["id"] = json!("0199e342-c780-7000-8000-000000000002");
    earlier["timestamp"] = json!("2026-10-14T10:00:01.000000001Z");
    expected["events"]
        .as_array_mut()
        .unwrap()
        .insert(0, earlier);
    core.reply(StatusCode::OK, expected.clone());
    let params = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("from", FROM)
        .append_pair("to", TO)
        .append_pair("limit", "50")
        .append_pair("cursor", "opaque+/=&?")
        .finish();
    let path = format!("/api/traces/{TRACE_ID}?{params}");
    // Обычный same-origin GET браузера не содержит Origin.
    let mut req = request(Method::GET, &path, Some(&session), None);
    req.headers_mut().remove(header::ORIGIN);
    let reply = send(&t.router, req).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body, expected);
    assert_eq!(reply.headers[header::CACHE_CONTROL], "no-store");
    let recorded = core.taken().pop().unwrap();
    assert_eq!(recorded.method, Method::GET);
    assert_eq!(recorded.uri.path(), format!("/v1/traces/{TRACE_ID}"));
    assert_eq!(recorded.authorization, "Bearer query-secret");
    assert_eq!(recorded.body, Value::Null);
    let actual: BTreeMap<_, _> =
        url::form_urlencoded::parse(recorded.uri.query().unwrap().as_bytes())
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
    assert_eq!(
        actual,
        BTreeMap::from([
            ("from".into(), FROM.into()),
            ("to".into(), TO.into()),
            ("limit".into(), "50".into()),
            ("cursor".into(), "opaque+/=&?".into()),
        ])
    );
}

#[tokio::test]
async fn analytics_preserves_counts_null_groups_and_defaults() {
    let (t, session, core) = setup().await;
    let histogram = json!({"buckets": [{"timestamp": FROM, "count": 0}, {"timestamp": "2026-10-14T10:01:00Z", "count": 13}], "as_of": TO});
    core.reply(StatusCode::OK, histogram.clone());
    let reply = send(
        &t.router,
        request(
            Method::POST,
            "/api/stats/histogram",
            Some(&session),
            Some(range()),
        ),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body, histogram);
    assert_eq!(reply.headers[header::CACHE_CONTROL], "no-store");
    let recorded = core.taken().pop().unwrap();
    assert_eq!(recorded.uri.path(), "/v1/stats/histogram");
    assert_eq!(recorded.authorization, "Bearer query-secret");
    assert_eq!(
        recorded.body,
        json!({"from": FROM, "to": TO, "filters": {}, "interval_seconds": 60})
    );

    let groups = json!({"groups": [{"values": {"service_instance": null, "level": "error"}, "count": 13}], "as_of": TO});
    core.reply(StatusCode::OK, groups.clone());
    let body = json!({"from": FROM, "to": TO, "filters": {"service": ["billing"]}, "group_by": ["service_instance", "level"]});
    let reply = send(
        &t.router,
        request(
            Method::POST,
            "/api/stats/group-by",
            Some(&session),
            Some(body),
        ),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body, groups);
    assert_eq!(reply.headers[header::CACHE_CONTROL], "no-store");
    let recorded = core.taken().pop().unwrap();
    assert_eq!(recorded.uri.path(), "/v1/stats/group-by");
    assert_eq!(recorded.authorization, "Bearer query-secret");
    assert_eq!(recorded.body["limit"], 100);
    assert_eq!(recorded.body["filters"], json!({"service": ["billing"]}));
    assert_eq!(
        recorded.body["group_by"],
        json!(["service_instance", "level"])
    );
}

#[tokio::test]
async fn all_queries_require_a_current_session_and_posts_require_origin() {
    let (t, session, core) = setup().await;
    for (method, path, body) in endpoints() {
        let reply = send(
            &t.router,
            request(method.clone(), &path, None, body.clone()),
        )
        .await;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{path}");
        assert_eq!(reply.headers[header::CACHE_CONTROL], "no-store");
        if method == Method::POST {
            for origin in [None, Some("https://foreign.invalid")] {
                let mut req = request(method.clone(), &path, Some(&session), body.clone());
                req.headers_mut().remove(header::ORIGIN);
                if let Some(origin) = origin {
                    req.headers_mut()
                        .insert(header::ORIGIN, origin.parse().unwrap());
                }
                let reply = send(&t.router, req).await;
                assert_eq!(reply.status, StatusCode::FORBIDDEN, "{path}");
                assert_eq!(reply.headers[header::CACHE_CONTROL], "no-store");
            }
        }
    }
    std::fs::write(t.htpasswd_path(), htpasswd_lines(&[("other", "password")])).unwrap();
    let reply = send(
        &t.router,
        request(
            Method::POST,
            "/api/logs/search",
            Some(&session),
            Some(range()),
        ),
    )
    .await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    assert!(core.taken().is_empty());
}

#[tokio::test]
async fn invalid_request_shapes_never_reach_core() {
    let (t, session, core) = setup().await;
    for (path, body) in [
        (
            "/api/logs/search",
            json!({"from": FROM, "to": TO, "sql": "select secret"}),
        ),
        (
            "/api/logs/search",
            json!({"from": FROM, "to": TO, "text": {"query": "x", "mode": "regex"}}),
        ),
        (
            "/api/logs/search",
            json!({"from": FROM, "to": TO, "filters": {"service": "api"}}),
        ),
        (
            "/api/logs/search",
            json!({"from": FROM, "to": TO, "limit": -1}),
        ),
        (
            "/api/stats/histogram",
            json!({"from": FROM, "to": TO, "text": {"query": "x"}}),
        ),
        (
            "/api/stats/group-by",
            json!({"from": FROM, "to": TO, "group_by": "service"}),
        ),
        (
            "/api/stats/group-by",
            json!({"from": FROM, "to": TO, "group_by": ["service"], "cursor": "x"}),
        ),
    ] {
        let reply = send(
            &t.router,
            request(Method::POST, path, Some(&session), Some(body)),
        )
        .await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{path}");
        assert_eq!(reply.body, json!({"error": "invalid_request"}));
    }
    for params in [
        format!("to={TO}"),
        format!("from={FROM}&to={TO}&limit=x"),
        format!("from={FROM}&to={TO}&page=2"),
    ] {
        let path = format!("/api/traces/{TRACE_ID}?{params}");
        let reply = send(&t.router, request(Method::GET, &path, Some(&session), None)).await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST);
        assert_eq!(reply.body, json!({"error": "invalid_request"}));
    }
    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/logs/search")
        .header(header::ORIGIN, ORIGIN)
        .header(header::COOKIE, &session)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from("{"))
        .unwrap();
    assert_eq!(send(&t.router, req).await.status, StatusCode::BAD_REQUEST);
    assert!(core.taken().is_empty());
}

#[tokio::test]
async fn core_error_statuses_use_safe_codes_without_ending_browser_session() {
    let (t, session, core) = setup().await;
    for (method, path, body) in endpoints() {
        for (upstream, expected, code) in [
            (400, 400, "invalid_query"),
            (410, 410, "cursor_expired"),
            (503, 503, "query_unavailable"),
            (504, 504, "query_timeout"),
            (401, 502, "core_unavailable"),
            (404, 502, "core_unavailable"),
            (500, 502, "core_unavailable"),
            (201, 502, "core_unavailable"),
        ] {
            core.reply(
                StatusCode::from_u16(upstream).unwrap(),
                json!({"error": "query-secret admin-secret secret upstream detail"}),
            );
            let reply = send(
                &t.router,
                request(method.clone(), &path, Some(&session), body.clone()),
            )
            .await;
            assert_eq!(reply.status.as_u16(), expected, "{path}: {upstream}");
            assert_eq!(reply.body, json!({"error": code}));
            assert_eq!(reply.headers[header::CACHE_CONTROL], "no-store");
        }
    }
    assert_eq!(me(&t, &session).await.status, StatusCode::OK);
}

#[tokio::test]
async fn unexpected_response_shapes_are_bad_gateway() {
    let (t, session, core) = setup().await;
    let mut extra_event = page();
    extra_event["events"][0]["token"] = json!("query-secret");
    let mut extra_envelope = page();
    extra_envelope["admin_token"] = json!("admin-secret");
    let mut unknown_level = page();
    unknown_level["events"][0]["level"] = json!("critical");
    let mut missing_cursor = page();
    missing_cursor
        .as_object_mut()
        .unwrap()
        .remove("next_cursor");
    for (path, request_body, response_body) in [
        ("/api/logs/search", range(), json!({"events": []})),
        ("/api/logs/search", range(), extra_event),
        ("/api/logs/search", range(), extra_envelope),
        ("/api/logs/search", range(), unknown_level),
        ("/api/logs/search", range(), missing_cursor),
        (
            "/api/logs/search",
            range(),
            json!({"events": [], "next_cursor": 5, "as_of": TO}),
        ),
        (
            "/api/stats/histogram",
            range(),
            json!({"buckets": [{"timestamp": FROM, "count": "secret"}], "as_of": TO}),
        ),
        (
            "/api/stats/histogram",
            range(),
            json!({"buckets": [], "as_of": TO, "token": "secret"}),
        ),
        (
            "/api/stats/group-by",
            json!({"from": FROM, "to": TO, "group_by": ["service"]}),
            json!({"groups": [{"values": {"token": "secret"}, "count": 1}], "as_of": TO}),
        ),
    ] {
        core.reply(StatusCode::OK, response_body);
        let reply = send(
            &t.router,
            request(Method::POST, path, Some(&session), Some(request_body)),
        )
        .await;
        assert_eq!(reply.status, StatusCode::BAD_GATEWAY, "{path}");
        assert_eq!(reply.body, json!({"error": "core_unavailable"}));
    }
    *core.reply.lock().unwrap() = (StatusCode::OK, "not JSON secret".into());
    let reply = send(
        &t.router,
        request(
            Method::POST,
            "/api/logs/search",
            Some(&session),
            Some(range()),
        ),
    )
    .await;
    assert_eq!(reply.status, StatusCode::BAD_GATEWAY);
    assert_eq!(reply.body, json!({"error": "core_unavailable"}));
}

#[tokio::test]
async fn history_uses_a_longer_timeout_than_live() {
    let (t, session, core) = setup().await;
    *core.delay.lock().unwrap() = Duration::from_secs(11);
    let now = time::OffsetDateTime::now_utc();
    let history = send(
        &t.router,
        request(
            Method::POST,
            "/api/logs/search",
            Some(&session),
            Some(range()),
        ),
    );
    let live = t
        .app
        .core
        .search(now - time::Duration::minutes(5), now, 1000);
    let (history, live) = tokio::join!(history, live);
    assert_eq!(history.status, StatusCode::OK);
    assert_eq!(history.body, page());
    assert!(
        matches!(live, Err(lognara_dock::core_client::CoreError::Request(error)) if error.is_timeout())
    );
}

#[tokio::test]
async fn connection_failure_is_bad_gateway_without_exposing_connection_details() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let t = start(&[("liis", "password")], &[("LOGNARA_DOCK_CORE_URL", &url)]);
    let session = login(&t, "liis", "password").await.session();
    let reply = send(
        &t.router,
        request(
            Method::POST,
            "/api/logs/search",
            Some(&session),
            Some(range()),
        ),
    )
    .await;
    assert_eq!(reply.status, StatusCode::BAD_GATEWAY);
    assert_eq!(reply.body, json!({"error": "core_unavailable"}));
}
