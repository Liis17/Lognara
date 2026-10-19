//! Метрики core через dock: проксирование, admin-токен, ошибки и защита запросов.

mod common;

use std::sync::atomic::Ordering;

use axum::http::{HeaderValue, Method, StatusCode, header};
use common::*;
use serde_json::{Value, json};

const ID: &str = "00000000-0000-0000-0000-000000000001";

/// Dock против поддельного core; админский токен задан, если `admin` равно true.
fn dock(core_url: &str) -> TestApp {
    start(
        &[("liis", "password")],
        &[("LOGNARA_DOCK_CORE_URL", core_url)],
    )
}

/// Запрос к dock от имени вошедшего пользователя.
async fn call(
    t: &TestApp,
    method: Method,
    path: &str,
    session: &str,
    body: Option<Value>,
) -> Reply {
    send(&t.router, request(method, path, Some(session), body)).await
}

fn definition() -> Value {
    json!({ "service": "api", "name": "requests", "origin": "counter" })
}

fn query() -> Value {
    json!({
        "service": "api",
        "name": "requests",
        "from": "2026-10-13T10:00:00Z",
        "to": "2026-10-13T11:00:00Z",
        "step_seconds": 60,
        "split": "none",
    })
}

/// Маршруты, которые требуют только query-токен.
fn query_routes() -> Vec<(Method, String, Option<Value>)> {
    vec![
        (Method::GET, "/api/series".into(), None),
        (Method::GET, "/api/series?group_by=server".into(), None),
        (Method::GET, "/api/series/sources".into(), None),
        (Method::POST, "/api/series/query".into(), Some(query())),
        (
            Method::POST,
            "/api/series/lookup".into(),
            Some(json!({"keys":[]})),
        ),
    ]
}

/// Маршруты, которые требуют admin-токен.
fn admin_routes() -> Vec<(Method, String, Option<Value>)> {
    vec![
        (Method::GET, "/api/series/definitions".into(), None),
        (
            Method::POST,
            "/api/series/import/check".into(),
            Some(json!({})),
        ),
        (
            Method::POST,
            "/api/series/import/apply".into(),
            Some(json!({})),
        ),
        (
            Method::POST,
            "/api/series/definitions".into(),
            Some(definition()),
        ),
        (
            Method::PUT,
            format!("/api/series/definitions/{ID}"),
            Some(definition()),
        ),
        (
            Method::DELETE,
            format!("/api/series/definitions/{ID}"),
            None,
        ),
        (
            Method::POST,
            "/api/series/preview".into(),
            Some(json!({ "service": "api", "name": "requests" })),
        ),
    ]
}

#[tokio::test]
async fn lists_and_queries_series_with_query_token() {
    let (core, url) = fake_core().await;
    let t = dock(&url);
    let session = login(&t, "liis", "password").await.session();

    let list = call(&t, Method::GET, "/api/series", &session, None).await;
    assert_eq!(list.status, StatusCode::OK);
    assert_eq!(list.body, series_metrics());

    let reply = call(
        &t,
        Method::POST,
        "/api/series/query",
        &session,
        Some(query()),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body, series_result());

    let calls = core.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].method, Method::GET);
    assert_eq!(calls[0].path, "/v1/series");
    assert_eq!(calls[1].path, "/v1/series/query");
    assert_eq!(calls[1].body, query());
    for recorded in &calls {
        assert_eq!(
            recorded.authorization.as_deref(),
            Some("Bearer query-secret")
        );
    }
}

#[tokio::test]
async fn proxies_definition_crud_with_admin_token() {
    let (core, url) = fake_core().await;
    let t = dock(&url);
    let session = login(&t, "liis", "password").await.session();

    let created = call(
        &t,
        Method::POST,
        "/api/series/definitions",
        &session,
        Some(definition()),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED);
    let id = created.body["id"].as_str().unwrap().to_owned();
    assert_eq!(created.body["name"], "requests");

    let listed = call(&t, Method::GET, "/api/series/definitions", &session, None).await;
    assert_eq!(listed.status, StatusCode::OK);
    assert_eq!(listed.body["definitions"][0]["id"], id);

    let renamed = json!({ "service": "api", "name": "requests_total", "origin": "counter" });
    let path = format!("/api/series/definitions/{id}");
    let updated = call(&t, Method::PUT, &path, &session, Some(renamed.clone())).await;
    assert_eq!(updated.status, StatusCode::OK);
    assert_eq!(updated.body["name"], "requests_total");

    let deleted = call(&t, Method::DELETE, &path, &session, None).await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT);
    assert_eq!(deleted.body, Value::Null);

    // Ответ 404 от core доходит до браузера как not_found.
    let missing = call(&t, Method::DELETE, &path, &session, None).await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    assert_eq!(missing.body["error"], "not_found");

    let preview = call(
        &t,
        Method::POST,
        "/api/series/preview",
        &session,
        Some(json!({ "service": "api", "name": "requests" })),
    )
    .await;
    assert_eq!(preview.status, StatusCode::OK);
    assert_eq!(preview.body["scanned"], 10);

    // Id не UUID отбрасывается до запроса к core.
    let invalid = call(
        &t,
        Method::PUT,
        "/api/series/definitions/not-a-uuid",
        &session,
        Some(definition()),
    )
    .await;
    assert_eq!(invalid.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        invalid.body,
        json!({ "error": "invalid", "detail": "id must be a UUID" })
    );

    let calls = core.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 6);
    assert!(
        calls
            .iter()
            .all(|recorded| recorded.authorization.as_deref() == Some("Bearer admin-secret"))
    );
    assert!(
        calls
            .iter()
            .any(|recorded| recorded.method == Method::PUT && recorded.body == renamed)
    );
}

#[tokio::test]
async fn core_errors_reach_the_user_as_invalid_or_internal() {
    let (core, url) = fake_core().await;
    let t = dock(&url);
    let session = login(&t, "liis", "password").await.session();

    *core.reply.lock().unwrap() = Some((StatusCode::BAD_REQUEST, json!({ "error": "bad regex" })));
    let reply = call(
        &t,
        Method::POST,
        "/api/series/query",
        &session,
        Some(query()),
    )
    .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        reply.body,
        json!({ "error": "invalid", "detail": "bad regex" })
    );

    let reply = call(
        &t,
        Method::POST,
        "/api/series/definitions",
        &session,
        Some(definition()),
    )
    .await;
    assert_eq!(
        reply.body,
        json!({ "error": "invalid", "detail": "bad regex" })
    );

    // Длинный текст ошибки обрезается до 1000 символов.
    let long = "x".repeat(5000);
    *core.reply.lock().unwrap() = Some((StatusCode::BAD_REQUEST, json!({ "error": long })));
    let reply = call(
        &t,
        Method::POST,
        "/api/series/preview",
        &session,
        Some(json!({})),
    )
    .await;
    assert_eq!(reply.body["detail"].as_str().unwrap().chars().count(), 1000);

    // Отказ core по токену браузеру не раскрывается: обычный 500.
    *core.reply.lock().unwrap() =
        Some((StatusCode::UNAUTHORIZED, json!({ "error": "unauthorized" })));
    let reply = call(&t, Method::GET, "/api/series", &session, None).await;
    assert_eq!(reply.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(reply.body, json!({ "error": "internal" }));
}

#[tokio::test]
async fn core_outage_is_502_core_unavailable() {
    let (core, url) = fake_core().await;
    let t = dock(&url);
    let session = login(&t, "liis", "password").await.session();

    for status in [StatusCode::SERVICE_UNAVAILABLE, StatusCode::GATEWAY_TIMEOUT] {
        *core.reply.lock().unwrap() = Some((status, json!({ "error": "store down" })));
        let reply = call(&t, Method::GET, "/api/series", &session, None).await;
        assert_eq!(reply.status, StatusCode::BAD_GATEWAY, "{status}");
        assert_eq!(reply.body, json!({ "error": "core_unavailable" }));
    }

    // Порт без слушателя: соединение отказано.
    let closed = {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        format!("http://{}", listener.local_addr().unwrap())
    };
    let t = dock(&closed);
    let session = login(&t, "liis", "password").await.session();
    let list = call(&t, Method::GET, "/api/series", &session, None).await;
    assert_eq!(list.status, StatusCode::BAD_GATEWAY);
    assert_eq!(list.body, json!({ "error": "core_unavailable" }));
    let create = call(
        &t,
        Method::POST,
        "/api/series/definitions",
        &session,
        Some(definition()),
    )
    .await;
    assert_eq!(create.status, StatusCode::BAD_GATEWAY);
    assert_eq!(create.body, json!({ "error": "core_unavailable" }));
}

#[tokio::test]
async fn series_routes_require_session_and_own_origin() {
    let (core, url) = fake_core().await;
    let t = dock(&url);
    let session = login(&t, "liis", "password").await.session();

    for (method, path, body) in query_routes().into_iter().chain(admin_routes()) {
        let anonymous = send(
            &t.router,
            request(method.clone(), &path, None, body.clone()),
        )
        .await;
        assert_eq!(
            anonymous.status,
            StatusCode::UNAUTHORIZED,
            "{method} {path}"
        );

        if method == Method::GET {
            continue;
        }
        for origin in [None, Some("https://evil.example")] {
            let mut req = request(method.clone(), &path, Some(&session), body.clone());
            match origin {
                Some(origin) => {
                    req.headers_mut()
                        .insert(header::ORIGIN, HeaderValue::from_static(origin));
                }
                None => {
                    req.headers_mut().remove(header::ORIGIN);
                }
            }
            let reply = send(&t.router, req).await;
            assert_eq!(
                reply.status,
                StatusCode::FORBIDDEN,
                "{method} {path} {origin:?}"
            );
        }
    }
    assert_eq!(
        core.requests.load(Ordering::SeqCst),
        0,
        "rejected requests must not reach core"
    );
}

#[tokio::test]
async fn responses_never_expose_tokens() {
    let (core, url) = fake_core().await;
    let t = dock(&url);
    let session = login(&t, "liis", "password").await.session();

    let mut replies = vec![
        (
            "list",
            call(&t, Method::GET, "/api/series", &session, None).await,
        ),
        (
            "create",
            call(
                &t,
                Method::POST,
                "/api/series/definitions",
                &session,
                Some(definition()),
            )
            .await,
        ),
        (
            "invalid id",
            call(
                &t,
                Method::PUT,
                "/api/series/definitions/nope",
                &session,
                Some(definition()),
            )
            .await,
        ),
        (
            "anonymous",
            send(&t.router, request(Method::GET, "/api/series", None, None)).await,
        ),
    ];

    for (name, status, body) in [
        (
            "core 400",
            StatusCode::BAD_REQUEST,
            json!({ "error": "bad regex" }),
        ),
        (
            "core 401",
            StatusCode::UNAUTHORIZED,
            json!({ "error": "unauthorized" }),
        ),
        (
            "core 503",
            StatusCode::SERVICE_UNAVAILABLE,
            json!({ "error": "down" }),
        ),
    ] {
        *core.reply.lock().unwrap() = Some((status, body));
        let reply = call(
            &t,
            Method::POST,
            "/api/series/query",
            &session,
            Some(query()),
        )
        .await;
        replies.push((name, reply));
    }

    for (name, reply) in replies {
        let text = format!("{} {:?}", reply.body, reply.headers);
        for secret in ["query-secret", "admin-secret"] {
            assert!(!text.contains(secret), "{name}: {text}");
        }
    }
}

#[tokio::test]
async fn proxies_scoped_sources_and_import_with_safe_conflicts() {
    let (core, url) = fake_core().await;
    let t = dock(&url);
    let session = login(&t, "liis", "password").await.session();
    for path in ["/api/series?group_by=server", "/api/series/sources"] {
        assert_eq!(
            call(&t, Method::GET, path, &session, None).await.status,
            StatusCode::OK
        );
    }
    let body = json!({"server":"a","service":"api","bundle":{"format":"lognara-metrics","version":1,"metrics":[]}});
    for action in ["check", "apply"] {
        assert_eq!(
            call(
                &t,
                Method::POST,
                &format!("/api/series/import/{action}"),
                &session,
                Some(body.clone())
            )
            .await
            .status,
            StatusCode::OK
        );
    }
    let calls = core.calls.lock().unwrap().clone();
    assert_eq!(calls[1].path, "/v1/series/sources");
    for call in &calls[2..] {
        assert_eq!(call.authorization.as_deref(), Some("Bearer admin-secret"));
        assert_eq!(call.body, body);
    }
    *core.reply.lock().unwrap() = Some((StatusCode::CONFLICT, json!({"error":"import_conflict"})));
    let reply = call(
        &t,
        Method::POST,
        "/api/series/import/apply",
        &session,
        Some(body),
    )
    .await;
    assert_eq!(reply.status, StatusCode::CONFLICT);
    assert_eq!(reply.body, json!({"error":"import_conflict"}));
}

#[tokio::test]
async fn favorite_metrics_are_shared_idempotent_and_scoped() {
    let t = start(&[("alice", "password"), ("bob", "password")], &[]);
    let alice = login(&t, "alice", "password").await.session();
    let bob = login(&t, "bob", "password").await.session();
    let path = "/api/series/favorites";
    let key = json!({"server":"a","service":"api","name":"requests","origin":"counter"});
    for _ in 0..2 {
        assert_eq!(
            call(&t, Method::PUT, path, &alice, Some(key.clone()))
                .await
                .status,
            StatusCode::NO_CONTENT
        );
    }
    let mut other = key.clone();
    other["server"] = json!("b");
    assert_eq!(
        call(&t, Method::PUT, path, &bob, Some(other.clone()))
            .await
            .status,
        StatusCode::NO_CONTENT
    );
    let mut gauge = key.clone();
    gauge["origin"] = json!("gauge");
    let mut worker = key.clone();
    worker["service"] = json!("worker");
    for variant in [&gauge, &worker] {
        assert_eq!(
            call(&t, Method::PUT, path, &alice, Some(variant.clone()))
                .await
                .status,
            StatusCode::NO_CONTENT
        );
    }
    let reply = call(&t, Method::GET, path, &bob, None).await;
    assert_eq!(reply.body["keys"], json!([key, gauge, worker, other]));
    for _ in 0..2 {
        assert_eq!(
            call(&t, Method::DELETE, path, &bob, Some(key.clone()))
                .await
                .status,
            StatusCode::NO_CONTENT
        );
    }
    assert_eq!(
        call(&t, Method::GET, path, &alice, None).await.body["keys"],
        json!([gauge, worker, other])
    );
    let mut invalid = key.clone();
    invalid["origin"] = json!("unknown");
    assert_eq!(
        call(&t, Method::PUT, path, &alice, Some(invalid))
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        send(&t.router, request(Method::GET, path, None, None))
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    let mut req = request(Method::PUT, path, Some(&alice), Some(key));
    req.headers_mut().remove(header::ORIGIN);
    assert_eq!(send(&t.router, req).await.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn scoped_catalog_filters_are_forwarded_without_losing_encoding() {
    let (core, url) = fake_core().await;
    let t = dock(&url);
    let session = login(&t, "liis", "password").await.session();
    let reply = call(
        &t,
        Method::GET,
        "/api/series?group_by=server&server=a%26b&service=api%2Fworker",
        &session,
        None,
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    let requests = core.calls.lock().unwrap();
    assert!(
        requests
            .last()
            .unwrap()
            .query
            .as_ref()
            .unwrap()
            .contains("server=a%26b&service=api%2Fworker")
    );
}
