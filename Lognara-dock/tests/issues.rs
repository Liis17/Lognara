//! Проблемы: общий статус, прокси к core, история и наблюдатель возвратов.

mod common;

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{Method, StatusCode, Uri};
use common::*;
use lognara_dock::issues;
use serde_json::{Value, json};

const FROM: &str = "2026-10-14T10:00:00Z";
const TO: &str = "2026-10-14T11:00:00Z";

fn id(n: u32) -> String {
    format!("f1_{n:064x}")
}

struct FakeCore {
    requests: Mutex<Vec<(String, Value)>>,
    reply: Mutex<(StatusCode, String)>,
}

impl FakeCore {
    fn reply(&self, status: StatusCode, body: Value) {
        *self.reply.lock().unwrap() = (status, body.to_string());
    }

    fn taken(&self) -> Vec<(String, Value)> {
        std::mem::take(&mut *self.requests.lock().unwrap())
    }
}

async fn record(State(core): State<Arc<FakeCore>>, uri: Uri, body: Bytes) -> (StatusCode, String) {
    core.requests.lock().unwrap().push((
        uri.path().to_owned(),
        serde_json::from_slice(&body).unwrap_or(Value::Null),
    ));
    core.reply.lock().unwrap().clone()
}

async fn setup() -> (TestApp, String, String, Arc<FakeCore>) {
    let core = Arc::new(FakeCore {
        requests: Mutex::default(),
        reply: Mutex::new((StatusCode::OK, "{}".into())),
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

fn issue(fingerprint: &str, count: u64) -> Value {
    json!({
        "fingerprint": fingerprint, "environment": null, "service": "billing", "action": "charge",
        "exception_type": "TimeoutException", "frames": ["Pay.charge(Pay.java:)"],
        "pattern_id": format!("p1_{}", "a".repeat(64)), "template": "Timeout <id_1>",
        "parts": [{"kind": "literal", "text": "Timeout "}, {"kind": "variable", "name": "id_1", "variable_type": "id"}],
        "exact": false, "count": count, "servers": count.min(1), "is_new": true,
        "first_seen": FROM, "last_seen": FROM,
        "buckets": [{"timestamp": FROM, "count": count}]
    })
}

fn response(issues: &[Value]) -> Value {
    json!({
        "algorithm_version": 1, "as_of": TO, "watermark": 4, "core_watermark": 5,
        "history_from": FROM, "history_complete": true,
        "counts": {"open": 2, "new": 1, "regressed": 1, "resolved": 1},
        "status_events": 10, "issues": issues
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
async fn status_is_shared_and_passed_to_core() {
    let (t, liis, other, core) = setup().await;
    let resolve = format!("/api/issues/{}/resolve", id(1));
    for _ in 0..2 {
        assert_eq!(
            call(&t, Method::POST, &resolve, &liis, None).await.status,
            StatusCode::NO_CONTENT
        );
    }
    let mut forged = request(
        Method::POST,
        &format!("/api/issues/{}/resolve", id(2)),
        Some(&liis),
        None,
    );
    forged.headers_mut().remove(axum::http::header::ORIGIN);
    assert_eq!(send(&t.router, forged).await.status, StatusCode::FORBIDDEN);
    for path in ["/api/issues/f1_XYZ/resolve", "/api/issues/p1_1/reopen"] {
        assert_eq!(
            call(&t, Method::POST, path, &liis, None).await.status,
            StatusCode::BAD_REQUEST,
            "{path}"
        );
    }
    let anonymous = send(
        &t.router,
        request(
            Method::POST,
            "/api/issues/search",
            None,
            Some(json!({"from": FROM, "to": TO})),
        ),
    )
    .await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
    assert!(core.taken().is_empty());

    // Статус общий: другой пользователь видит решённую проблему.
    core.reply(StatusCode::OK, response(&[issue(&id(1), 3)]));
    let body = json!({"from": FROM, "to": TO, "filters": {"service": ["billing"]}, "status": "resolved", "limit": 20});
    let reply = call(&t, Method::POST, "/api/issues/search", &other, Some(body)).await;
    assert_eq!(reply.status, StatusCode::OK);
    let state = &reply.body["issues"][0]["state"];
    assert_eq!(
        (state["status"].as_str(), state["resolved_by"].as_str()),
        (Some("resolved"), Some("liis"))
    );
    assert!(
        state["resolved_at"]
            .as_str()
            .is_some_and(|at| at.ends_with('Z'))
    );
    assert_eq!(reply.body["issues"][0]["count"], 3);
    let requests = core.taken();
    assert_eq!(requests[0].0, "/v1/issues/search");
    assert_eq!(requests[0].1["resolved"], json!([id(1)]));
    assert_eq!(requests[0].1["regressed"], json!([]));
    assert_eq!(requests[0].1["status"], "resolved");
    assert_eq!(requests[0].1["filters"], json!({"service": ["billing"]}));

    // Решённая проблема в открытой вкладке — нарушение контракта core.
    let reply = call(
        &t,
        Method::POST,
        "/api/issues/search",
        &other,
        Some(json!({"from": FROM, "to": TO})),
    )
    .await;
    assert_eq!(
        (reply.status, reply.body),
        (
            StatusCode::BAD_GATEWAY,
            json!({"error": "core_unavailable"})
        )
    );
    core.reply(StatusCode::OK, response(&[issue(&id(2), 4)]));
    let reply = call(
        &t,
        Method::POST,
        "/api/issues/search",
        &other,
        Some(json!({"from": FROM, "to": TO})),
    )
    .await;
    assert_eq!(
        reply.body["issues"][0]["state"],
        json!({"status": "open", "resolved_at": null, "resolved_by": null})
    );
    for body in [
        json!({"from": FROM, "to": TO, "limit": 101}),
        json!({"from": FROM, "to": TO, "status": "closed"}),
        json!({"from": FROM, "to": TO, "resolved": []}),
    ] {
        let reply = call(&t, Method::POST, "/api/issues/search", &liis, Some(body)).await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    }
    assert_eq!(core.taken().len(), 2);

    // Повторное открытие пишет историю один раз и снимает решение.
    let reopen = format!("/api/issues/{}/reopen", id(1));
    for _ in 0..2 {
        assert_eq!(
            call(&t, Method::POST, &reopen, &other, None).await.status,
            StatusCode::NO_CONTENT
        );
    }
    let details = json!({"as_of": TO, "issue": issue(&id(1), 3),
        "servers": [{"value": "s1", "count": 2}], "other_servers": 1, "examples": []});
    core.reply(StatusCode::OK, details.clone());
    let reply = call(
        &t,
        Method::POST,
        &format!("/api/issues/{}/details", id(1)),
        &liis,
        Some(json!({"from": FROM, "to": TO})),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body["state"]["status"], "open");
    assert_eq!(reply.body["servers"], details["servers"]);
    let history = reply.body["history"].as_array().unwrap();
    assert_eq!(
        history
            .iter()
            .map(|item| (
                item["action"].as_str().unwrap(),
                item["actor"].as_str().unwrap()
            ))
            .collect::<Vec<_>>(),
        [("reopened", "other"), ("resolved", "liis")]
    );
    assert_eq!(core.taken()[0].0, format!("/v1/issues/{}/details", id(1)));
}

#[tokio::test]
async fn details_errors_are_issue_specific() {
    let (t, liis, _, core) = setup().await;
    let path = format!("/api/issues/{}/details", id(7));
    for (status, body, expected, code) in [
        (
            404,
            json!({"error": "not found in range"}),
            404,
            "issue_not_found",
        ),
        (
            503,
            json!({"error": "pattern index is not ready"}),
            503,
            "issues_not_ready",
        ),
        (
            503,
            json!({"error": "query capacity is exhausted"}),
            503,
            "query_unavailable",
        ),
        (400, json!({"error": "secret detail"}), 400, "invalid_query"),
    ] {
        core.reply(StatusCode::from_u16(status).unwrap(), body);
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
    // Сумма серверов не сходится со счётчиком — ответ отклонён.
    core.reply(
        StatusCode::OK,
        json!({"as_of": TO, "issue": issue(&id(7), 3), "servers": [{"value": "s1", "count": 1}], "other_servers": 0, "examples": []}),
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

fn recurrences(watermark: u64, core_watermark: u64, found: &[(&str, &str)]) -> Value {
    json!({"watermark": watermark, "core_watermark": core_watermark,
        "recurrences": found.iter().map(|(id, first)| json!({"fingerprint": id, "first_seen": first, "count": 1})).collect::<Vec<_>>()})
}

async fn status(t: &TestApp, session: &str, core: &FakeCore, fingerprint: &str) -> Value {
    core.reply(
        StatusCode::OK,
        json!({"as_of": TO, "issue": issue(fingerprint, 0), "servers": [], "other_servers": 0, "examples": []}),
    );
    let reply = call(
        t,
        Method::POST,
        &format!("/api/issues/{fingerprint}/details"),
        session,
        Some(json!({"from": FROM, "to": TO})),
    )
    .await;
    core.taken();
    reply.body
}

#[tokio::test]
async fn watcher_marks_only_new_cases_after_resolution() {
    let (t, liis, _, core) = setup().await;
    // Первый шаг без решённых проблем только запоминает watermark core.
    core.reply(StatusCode::OK, recurrences(10, 10, &[]));
    assert!(!issues::sync(&t.app).await.unwrap());
    assert_eq!(core.taken()[0].1, json!({"after": 0, "issues": []}));

    let (a, b) = (id(1), id(2));
    for fingerprint in [&a, &b] {
        call(
            &t,
            Method::POST,
            &format!("/api/issues/{fingerprint}/resolve"),
            &liis,
            None,
        )
        .await;
    }
    let future = "2999-01-01T00:00:00Z";
    // b: случай раньше решения (core так не отвечает, Dock всё равно проверяет время).
    core.reply(
        StatusCode::OK,
        recurrences(12, 20, &[(&a, future), (&b, "2000-01-01T00:00:00Z")]),
    );
    assert!(issues::sync(&t.app).await.unwrap());
    let request = &core.taken()[0].1;
    assert_eq!(request["after"], 10);
    let watched = request["issues"].as_array().unwrap();
    assert_eq!(watched.len(), 2);
    assert!(
        watched
            .iter()
            .all(|watch| watch["since"].as_str().unwrap().ends_with('Z'))
    );

    let a_status = status(&t, &liis, &core, &a).await;
    assert_eq!(a_status["state"]["status"], "regressed");
    assert_eq!(a_status["history"][0]["action"], "regressed");
    assert_eq!(a_status["history"][0]["actor"], Value::Null);
    assert_eq!(a_status["history"][0]["event_at"], future);
    assert_eq!(
        status(&t, &liis, &core, &b).await["state"]["status"],
        "resolved"
    );

    // Следующий шаг продолжает с нового checkpoint и наблюдает только решённые.
    core.reply(StatusCode::OK, recurrences(20, 20, &[]));
    assert!(!issues::sync(&t.app).await.unwrap());
    let request = &core.taken()[0].1;
    assert_eq!(request["after"], 12);
    assert_eq!(request["issues"].as_array().unwrap().len(), 1);
    assert_eq!(request["issues"][0]["fingerprint"], b);

    // Повторное решение вернувшейся проблемы снова наблюдает её.
    call(
        &t,
        Method::POST,
        &format!("/api/issues/{a}/resolve"),
        &liis,
        None,
    )
    .await;
    assert_eq!(
        status(&t, &liis, &core, &a).await["state"]["status"],
        "resolved"
    );

    // Хранилище core начато заново: checkpoint опускается, решения остаются сравнимыми.
    core.reply(StatusCode::OK, recurrences(3, 3, &[]));
    assert!(!issues::sync(&t.app).await.unwrap());
    core.taken();
    core.reply(StatusCode::OK, recurrences(5, 5, &[(&a, future)]));
    assert!(!issues::sync(&t.app).await.unwrap());
    assert_eq!(core.taken()[0].1["after"], 3);
    assert_eq!(
        status(&t, &liis, &core, &a).await["state"]["status"],
        "regressed"
    );

    // Ответ с ненаблюдаемым fingerprint отклоняется без изменения checkpoint.
    core.reply(StatusCode::OK, recurrences(9, 9, &[(&id(9), future)]));
    assert!(issues::sync(&t.app).await.is_err());
    core.taken();
    core.reply(StatusCode::OK, recurrences(9, 9, &[]));
    issues::sync(&t.app).await.unwrap();
    assert_eq!(core.taken()[0].1["after"], 5);
}
