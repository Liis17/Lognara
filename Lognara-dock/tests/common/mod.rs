#![allow(dead_code)]

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::{Body, to_bytes};
use axum::extract::{self, State};
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use http_body_util::BodyExt;
use lognara_dock::api::{self, App, AppState};
use lognara_dock::config::Config;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;
use uuid::Uuid;

pub const ORIGIN: &str = "http://localhost:7403";

pub struct TestApp {
    pub app: AppState,
    pub router: Router,
    pub dir: TempDir,
}

impl TestApp {
    pub fn htpasswd_path(&self) -> std::path::PathBuf {
        self.dir.path().join("auth/htpasswd")
    }
}

pub fn config(dir: &Path, extra: &[(&str, &str)]) -> Config {
    let auth = dir.join("auth/htpasswd").display().to_string();
    let data = dir.join("data").display().to_string();
    let web = dir.join("web").display().to_string();
    let mut vars: Vec<(String, String)> = [
        ("LOGNARA_DOCK_PUBLIC_URL", ORIGIN),
        ("LOGNARA_DOCK_CORE_URL", "http://127.0.0.1:9"),
        ("LOGNARA_DOCK_CORE_TOKEN", "query-secret"),
        ("LOGNARA_DOCK_HTPASSWD_FILE", &auth),
        ("LOGNARA_DOCK_DATA_DIR", &data),
        ("LOGNARA_DOCK_WEB_DIR", &web),
        ("LOGNARA_DOCK_BCRYPT_COST", "10"),
        ("LOGNARA_DOCK_PASSWORD_MIN_LENGTH", "8"),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    vars.extend(extra.iter().map(|(k, v)| (k.to_string(), v.to_string())));
    Config::from_lookup(|name| {
        vars.iter()
            .rev()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    })
    .unwrap()
}

/// Строки htpasswd с быстрым bcrypt cost 4.
pub fn htpasswd_lines(users: &[(&str, &str)]) -> String {
    users
        .iter()
        .map(|(user, password)| format!("{user}:{}\n", bcrypt::hash(password, 4).unwrap()))
        .collect()
}

pub fn start(users: &[(&str, &str)], extra: &[(&str, &str)]) -> TestApp {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("auth")).unwrap();
    std::fs::write(dir.path().join("auth/htpasswd"), htpasswd_lines(users)).unwrap();
    let app = App::open(config(dir.path(), extra)).unwrap();
    TestApp {
        router: api::router(app.clone()),
        app,
        dir,
    }
}

pub struct Reply {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Value,
}

impl Reply {
    /// `name=value` из Set-Cookie сессии.
    pub fn session(&self) -> String {
        self.cookie("lognara_dock_session")
    }

    pub fn cookie(&self, name: &str) -> String {
        self.headers
            .get_all(header::SET_COOKIE)
            .iter()
            .map(|v| v.to_str().unwrap())
            .find(|v| v.starts_with(&format!("{name}=")))
            .unwrap_or_else(|| panic!("no {name} cookie"))
            .split(';')
            .next()
            .unwrap()
            .to_owned()
    }
}

pub async fn send(router: &Router, request: Request<Body>) -> Reply {
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    Reply {
        status,
        headers,
        body,
    }
}

pub fn request(
    method: Method,
    path: &str,
    cookie: Option<&str>,
    body: Option<Value>,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header(header::ORIGIN, ORIGIN);
    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, cookie);
    }
    match body {
        Some(body) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    }
}

pub async fn login(t: &TestApp, username: &str, password: &str) -> Reply {
    send(
        &t.router,
        request(
            Method::POST,
            "/api/auth/login",
            None,
            Some(serde_json::json!({ "username": username, "password": password })),
        ),
    )
    .await
}

pub async fn me(t: &TestApp, cookie: &str) -> Reply {
    send(
        &t.router,
        request(Method::GET, "/api/me", Some(cookie), None),
    )
    .await
}

/// Запрос, который получил поддельный core.
#[derive(Clone, Debug)]
pub struct Call {
    pub method: Method,
    pub path: String,
    pub authorization: Option<String>,
    pub body: Value,
}

/// Поддельный core: поиск логов, метрики и определения.
#[derive(Default)]
pub struct FakeCore {
    /// События по убыванию времени, как отдаёт core.
    pub events: Mutex<Vec<Value>>,
    /// Поиск логов отвечает 503.
    pub failing: AtomicBool,
    pub requests: AtomicUsize,
    /// Все полученные запросы по порядку.
    pub calls: Mutex<Vec<Call>>,
    /// Если задано, на любой запрос отвечает этим статусом и телом.
    pub reply: Mutex<Option<(StatusCode, Value)>>,
    /// Определения, которые хранит fake core.
    pub definitions: Mutex<Vec<Value>>,
}

/// Записывает запрос, проверяет токен (admin для `/v1/admin/`, иначе query) и отвечает `reply`.
async fn observe(
    State(core): State<Arc<FakeCore>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    core.requests.fetch_add(1, Ordering::SeqCst);
    let (parts, body) = request.into_parts();
    let bytes = to_bytes(body, usize::MAX).await.unwrap();
    let path = parts.uri.path().to_owned();
    let authorization = parts
        .headers
        .get(header::AUTHORIZATION)
        .map(|value| value.to_str().unwrap().to_owned());
    core.calls.lock().unwrap().push(Call {
        method: parts.method.clone(),
        path: path.clone(),
        authorization: authorization.clone(),
        body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    });
    let expected = if path.starts_with("/v1/admin/") {
        "Bearer admin-secret"
    } else {
        "Bearer query-secret"
    };
    if authorization.as_deref() != Some(expected) {
        let body = json!({ "error": "unauthorized" });
        return (StatusCode::UNAUTHORIZED, Json(body)).into_response();
    }
    let reply = core.reply.lock().unwrap().clone();
    if let Some((status, body)) = reply {
        return (status, Json(body)).into_response();
    }
    next.run(Request::from_parts(parts, Body::from(bytes)))
        .await
}

async fn search(
    State(core): State<Arc<FakeCore>>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, StatusCode> {
    assert_eq!(body["limit"], 1000);
    assert!(body["from"].as_str().unwrap().ends_with('Z'));
    if core.failing.load(Ordering::SeqCst) {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    Ok(Json(
        json!({ "events": *core.events.lock().unwrap(), "next_cursor": null, "as_of": "x" }),
    ))
}

/// Ответ core на `GET /v1/series`.
pub fn series_metrics() -> Value {
    json!({ "metrics": [{
        "service": "api",
        "name": "requests",
        "origin": "counter",
        "first_seen": "2026-10-13T10:00:00Z",
        "last_seen": "2026-10-13T10:05:00Z",
        "definition": null,
        "environments": ["production", "staging"],
    }] })
}

/// Ответ core на `POST /v1/series/query`.
pub fn series_result() -> Value {
    json!({
        "kind": "delta",
        "aggregation": "sum",
        "unit": "requests",
        "buckets": ["2026-10-13T10:00:00Z", "2026-10-13T10:01:00Z"],
        "series": [{ "label": "", "values": [3, null] }],
    })
}

async fn series_list() -> Json<Value> {
    Json(series_metrics())
}

async fn series_query() -> Json<Value> {
    Json(series_result())
}

async fn list_definitions(State(core): State<Arc<FakeCore>>) -> Json<Value> {
    let definitions = core.definitions.lock().unwrap().clone();
    Json(json!({ "definitions": definitions }))
}

async fn create_definition(
    State(core): State<Arc<FakeCore>>,
    Json(mut definition): Json<Value>,
) -> (StatusCode, Json<Value>) {
    definition["id"] = json!(Uuid::new_v4().to_string());
    core.definitions.lock().unwrap().push(definition.clone());
    (StatusCode::CREATED, Json(definition))
}

async fn update_definition(
    State(core): State<Arc<FakeCore>>,
    extract::Path(id): extract::Path<String>,
    Json(mut definition): Json<Value>,
) -> Response {
    let mut definitions = core.definitions.lock().unwrap();
    let Some(slot) = definitions
        .iter_mut()
        .find(|d| d["id"].as_str() == Some(id.as_str()))
    else {
        return not_found();
    };
    definition["id"] = json!(id);
    *slot = definition.clone();
    (StatusCode::OK, Json(definition)).into_response()
}

async fn delete_definition(
    State(core): State<Arc<FakeCore>>,
    extract::Path(id): extract::Path<String>,
) -> Response {
    let mut definitions = core.definitions.lock().unwrap();
    let before = definitions.len();
    definitions.retain(|d| d["id"].as_str() != Some(id.as_str()));
    if definitions.len() == before {
        return not_found();
    }
    StatusCode::NO_CONTENT.into_response()
}

fn not_found() -> Response {
    let body = json!({ "error": "definition not found" });
    (StatusCode::NOT_FOUND, Json(body)).into_response()
}

async fn preview() -> Json<Value> {
    Json(json!({ "scanned": 10, "matched": 2, "samples": [] }))
}

/// Поддельный core на свободном порту; возвращает его состояние и базовый URL.
pub async fn fake_core() -> (Arc<FakeCore>, String) {
    let core = Arc::new(FakeCore::default());
    let router = Router::new()
        .route("/v1/logs/search", post(search))
        .route("/v1/series", get(series_list))
        .route("/v1/series/query", post(series_query))
        .route(
            "/v1/admin/series/definitions",
            get(list_definitions).post(create_definition),
        )
        .route(
            "/v1/admin/series/definitions/{id}",
            put(update_definition).delete(delete_definition),
        )
        .route("/v1/admin/series/preview", post(preview))
        .layer(middleware::from_fn_with_state(core.clone(), observe))
        .with_state(core.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (core, url)
}
