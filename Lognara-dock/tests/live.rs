//! Лента в реальном времени против поддельного core.

mod common;

use std::sync::atomic::Ordering;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, StatusCode};
use common::*;
use http_body_util::BodyExt;
use lognara_dock::api::{self, App};
use serde_json::{Value, json};
use tower::ServiceExt;

fn event(id: &str, timestamp: &str) -> Value {
    json!({ "id": id, "timestamp": timestamp, "level": "info", "message": id, "attributes": {} })
}

/// Читает SSE-события из тела ответа.
struct SseReader {
    body: Body,
    buffer: String,
}

impl SseReader {
    async fn next(&mut self) -> (String, Value) {
        loop {
            if let Some(end) = self.buffer.find("\n\n") {
                let block: String = self.buffer.drain(..end + 2).collect();
                let mut name = String::new();
                let mut data = String::new();
                for line in block.lines() {
                    if let Some(value) = line.strip_prefix("event: ") {
                        name = value.to_owned();
                    } else if let Some(value) = line.strip_prefix("data: ") {
                        data.push_str(value);
                    }
                }
                if name.is_empty() {
                    continue; // keepalive
                }
                return (name, serde_json::from_str(&data).unwrap());
            }
            let frame = tokio::time::timeout(Duration::from_secs(5), self.body.frame())
                .await
                .expect("SSE event in time")
                .expect("stream open")
                .unwrap();
            if let Ok(data) = frame.into_data() {
                self.buffer.push_str(std::str::from_utf8(&data).unwrap());
            }
        }
    }

    async fn next_logs(&mut self) -> Vec<String> {
        let (name, data) = self.next().await;
        assert_eq!(name, "logs", "{data}");
        data.as_array()
            .unwrap()
            .iter()
            .map(|e| e["id"].as_str().unwrap().to_owned())
            .collect()
    }
}

async fn open_stream(t: &TestApp, cookie: &str) -> SseReader {
    let response = t
        .router
        .clone()
        .oneshot(request(Method::GET, "/api/logs/stream", Some(cookie), None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    SseReader {
        body: response.into_body(),
        buffer: String::new(),
    }
}

async fn start_with_core(url: &str) -> TestApp {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("auth")).unwrap();
    std::fs::write(
        dir.path().join("auth/htpasswd"),
        htpasswd_lines(&[("liis", "password")]),
    )
    .unwrap();
    let app = App::open(config(
        dir.path(),
        &[
            ("LOGNARA_DOCK_CORE_URL", url),
            ("LOGNARA_DOCK_LIVE_POLL_MS", "50"),
        ],
    ))
    .unwrap();
    TestApp {
        router: api::router(app.clone()),
        app,
        dir,
    }
}

#[tokio::test]
async fn streams_new_and_late_events_once() {
    let (core, url) = fake_core().await;
    *core.events.lock().unwrap() = vec![
        event("b", "2026-10-13T10:00:02Z"),
        event("a", "2026-10-13T10:00:01Z"),
    ];
    let t = start_with_core(&url).await;
    let session = login(&t, "liis", "password").await.session();
    let mut stream = open_stream(&t, &session).await;

    // Снимок пуст, затем первый опрос приносит события окна по возрастанию времени.
    assert!(stream.next_logs().await.is_empty());
    assert_eq!(stream.next_logs().await, ["a", "b"]);

    // Новое событие и опоздавшее с более ранней меткой приходят по одному разу.
    core.events
        .lock()
        .unwrap()
        .splice(0..0, [event("c", "2026-10-13T10:00:03Z")]);
    core.events
        .lock()
        .unwrap()
        .push(event("late", "2026-10-13T10:00:00Z"));
    assert_eq!(stream.next_logs().await, ["late", "c"]);

    // Второй подписчик получает накопленное одним снимком.
    let mut second = open_stream(&t, &session).await;
    assert_eq!(second.next_logs().await, ["a", "b", "late", "c"]);

    core.events
        .lock()
        .unwrap()
        .insert(0, event("d", "2026-10-13T10:00:04Z"));
    assert_eq!(stream.next_logs().await, ["d"]);
    assert_eq!(second.next_logs().await, ["d"]);
}

#[tokio::test]
async fn reports_core_errors_and_recovery() {
    let (core, url) = fake_core().await;
    core.failing.store(true, Ordering::SeqCst);
    let t = start_with_core(&url).await;
    let session = login(&t, "liis", "password").await.session();
    let mut stream = open_stream(&t, &session).await;
    assert!(stream.next_logs().await.is_empty());

    let (name, data) = stream.next().await;
    assert_eq!(name, "core_error");
    assert!(data["message"].as_str().unwrap().contains("503"), "{data}");
    assert!(!data.to_string().contains("query-secret"));

    core.events
        .lock()
        .unwrap()
        .push(event("a", "2026-10-13T10:00:01Z"));
    core.failing.store(false, Ordering::SeqCst);
    assert_eq!(stream.next().await.0, "core_ok");
    assert_eq!(stream.next_logs().await, ["a"]);
}

#[tokio::test]
async fn polling_stops_without_subscribers() {
    let (core, url) = fake_core().await;
    let t = start_with_core(&url).await;
    let session = login(&t, "liis", "password").await.session();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        core.requests.load(Ordering::SeqCst),
        0,
        "no polling before subscribers"
    );

    let mut stream = open_stream(&t, &session).await;
    stream.next_logs().await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(t.app.live.is_running());
    assert!(core.requests.load(Ordering::SeqCst) > 0);

    drop(stream);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!t.app.live.is_running());
    let stopped = core.requests.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(core.requests.load(Ordering::SeqCst), stopped);
}

#[tokio::test]
async fn stream_requires_session_and_closes_on_shutdown() {
    let (_core, url) = fake_core().await;
    let t = start_with_core(&url).await;
    let anonymous = send(
        &t.router,
        request(Method::GET, "/api/logs/stream", None, None),
    )
    .await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);

    let session = login(&t, "liis", "password").await.session();
    let mut stream = open_stream(&t, &session).await;
    stream.next_logs().await;
    t.app.shutdown.cancel();
    let end = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(frame) = stream.body.frame().await {
            frame.unwrap();
        }
    })
    .await;
    assert!(end.is_ok(), "stream must end on shutdown");
}
