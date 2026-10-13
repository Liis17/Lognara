#![allow(dead_code)]

use std::path::Path;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use lognara_dock::api::{self, App, AppState};
use lognara_dock::config::Config;
use serde_json::Value;
use tempfile::TempDir;
use tower::ServiceExt;

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
    let mut vars: Vec<(String, String)> = [
        ("LOGNARA_DOCK_PUBLIC_URL", ORIGIN),
        ("LOGNARA_DOCK_CORE_URL", "http://127.0.0.1:9"),
        ("LOGNARA_DOCK_CORE_TOKEN", "query-secret"),
        ("LOGNARA_DOCK_HTPASSWD_FILE", &auth),
        ("LOGNARA_DOCK_DATA_DIR", &data),
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
