mod common;

use std::time::Duration;

use axum::http::{Method, StatusCode, header};
use common::*;
use lognara_dock::api::App;
use serde_json::json;

#[tokio::test]
async fn password_login_sets_strict_cookie_and_opens_session() {
    let t = start(&[("liis", "correct horse")], &[]);
    let reply = login(&t, "liis", "correct horse").await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body["username"], "liis");
    let set_cookie = reply.headers[header::SET_COOKIE].to_str().unwrap();
    for flag in [
        "HttpOnly",
        "Secure",
        "SameSite=Strict",
        "Path=/",
        "Max-Age=43200",
    ] {
        assert!(set_cookie.contains(flag), "{set_cookie}");
    }

    let me = me(&t, &reply.session()).await;
    assert_eq!(me.status, StatusCode::OK);
    assert_eq!(me.body["username"], "liis");
}

#[tokio::test]
async fn wrong_password_and_unknown_user_are_rejected() {
    let t = start(&[("liis", "correct horse")], &[]);
    assert_eq!(
        login(&t, "liis", "wrong").await.status,
        StatusCode::UNAUTHORIZED
    );
    let unknown = login(&t, "ghost", "correct horse").await;
    assert_eq!(unknown.status, StatusCode::UNAUTHORIZED);
    assert_eq!(unknown.body["error"], "unauthorized");
    assert!(unknown.headers.get(header::SET_COOKIE).is_none());
    assert_eq!(
        me(&t, "lognara_dock_session=forged").await.status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn non_bcrypt_htpasswd_stops_startup_with_line_number() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("auth")).unwrap();
    let text = format!(
        "{}md5:$apr1$x$secrethash\n",
        htpasswd_lines(&[("liis", "pw")])
    );
    std::fs::write(dir.path().join("auth/htpasswd"), text).unwrap();
    let err = App::open(config(dir.path(), &[]))
        .err()
        .unwrap()
        .to_string();
    assert!(err.contains("htpasswd:2"), "{err}");
    assert!(err.contains("bcrypt"), "{err}");
    assert!(!err.contains("secrethash"));

    let missing = tempfile::tempdir().unwrap();
    assert!(App::open(config(missing.path(), &[])).is_err());
}

#[tokio::test]
async fn htpasswd_changes_apply_without_restart() {
    let t = start(&[("liis", "first-pass")], &[]);
    let session = login(&t, "liis", "first-pass").await.session();

    std::fs::write(
        t.htpasswd_path(),
        htpasswd_lines(&[("liis", "first-pass"), ("anna", "second-pass")]),
    )
    .unwrap();
    assert_eq!(
        login(&t, "anna", "second-pass").await.status,
        StatusCode::OK
    );

    // Удалённый пользователь теряет доступ сразу, даже с действующей сессией.
    std::fs::write(
        t.htpasswd_path(),
        htpasswd_lines(&[("anna", "second-pass")]),
    )
    .unwrap();
    assert_eq!(me(&t, &session).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        login(&t, "liis", "first-pass").await.status,
        StatusCode::UNAUTHORIZED
    );

    // Сломанный файл запрещает вход и снимает readiness.
    std::fs::write(t.htpasswd_path(), "anna:plaintext\n").unwrap();
    assert_eq!(
        login(&t, "anna", "second-pass").await.status,
        StatusCode::UNAUTHORIZED
    );
    let ready = send(&t.router, request(Method::GET, "/health/ready", None, None)).await;
    assert_eq!(ready.status, StatusCode::SERVICE_UNAVAILABLE);

    std::fs::write(
        t.htpasswd_path(),
        htpasswd_lines(&[("anna", "second-pass")]),
    )
    .unwrap();
    let ready = send(&t.router, request(Method::GET, "/health/ready", None, None)).await;
    assert_eq!(ready.status, StatusCode::OK);
}

#[tokio::test]
async fn sessions_expire_by_absolute_ttl_and_idle_timeout() {
    let ttl = start(
        &[("liis", "password")],
        &[("LOGNARA_DOCK_SESSION_TTL_SECONDS", "1")],
    );
    let session = login(&ttl, "liis", "password").await.session();
    assert_eq!(me(&ttl, &session).await.status, StatusCode::OK);
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert_eq!(me(&ttl, &session).await.status, StatusCode::UNAUTHORIZED);

    let idle = start(
        &[("liis", "password")],
        &[("LOGNARA_DOCK_SESSION_IDLE_SECONDS", "1")],
    );
    let session = login(&idle, "liis", "password").await.session();
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert_eq!(me(&idle, &session).await.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn changing_requests_require_own_origin() {
    let t = start(&[("liis", "password")], &[]);
    let body = json!({ "username": "liis", "password": "password" }).to_string();
    for origin in [Some("https://evil.example"), Some("null"), None] {
        let mut builder = axum::http::Request::builder()
            .method(Method::POST)
            .uri("/api/auth/login")
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(origin) = origin {
            builder = builder.header(header::ORIGIN, origin);
        }
        let reply = send(&t.router, builder.body(body.clone().into()).unwrap()).await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{origin:?}");
    }
}

#[tokio::test]
async fn five_failures_lock_password_login() {
    let t = start(&[("liis", "password"), ("anna", "password")], &[]);
    for _ in 0..5 {
        assert_eq!(
            login(&t, "liis", "wrong").await.status,
            StatusCode::UNAUTHORIZED
        );
    }
    let locked = login(&t, "liis", "password").await;
    assert_eq!(locked.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(locked.headers.contains_key(header::RETRY_AFTER));
    // Блокировка касается только этого логина.
    assert_eq!(login(&t, "anna", "password").await.status, StatusCode::OK);
}

#[tokio::test]
async fn logout_revokes_session() {
    let t = start(&[("liis", "password")], &[]);
    let session = login(&t, "liis", "password").await.session();
    let reply = send(
        &t.router,
        request(Method::POST, "/api/auth/logout", Some(&session), None),
    )
    .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    assert!(
        reply.headers[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .contains("Max-Age=0")
    );
    assert_eq!(me(&t, &session).await.status, StatusCode::UNAUTHORIZED);
}
