mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use common::*;
use http_body_util::BodyExt;
use lognara_dock::api::{self, App};
use lognara_dock::store::{CURRENT_SCHEMA_VERSION, now_ms};
use serde_json::{Value, json};
use tower::ServiceExt;

fn request_with(
    method: Method,
    path: &str,
    cookie: Option<&str>,
    authorization: Option<&str>,
    origin: Option<&str>,
    ip: Option<&str>,
    body: Option<Value>,
) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(path);
    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, cookie);
    }
    if let Some(authorization) = authorization {
        builder = builder.header(header::AUTHORIZATION, authorization);
    }
    if let Some(origin) = origin {
        builder = builder.header(header::ORIGIN, origin);
    }
    if let Some(ip) = ip {
        builder = builder.header("x-real-ip", ip);
    }
    match body {
        Some(body) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    }
}

fn start_with_core(url: &str, recheck: Option<Duration>) -> TestApp {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("auth")).unwrap();
    std::fs::write(
        dir.path().join("auth/htpasswd"),
        htpasswd_lines(&[("liis", "password"), ("anna", "password")]),
    )
    .unwrap();
    let mut app = App::open(config(dir.path(), &[("LOGNARA_DOCK_CORE_URL", url)])).unwrap();
    if let Some(interval) = recheck {
        Arc::get_mut(&mut app)
            .unwrap()
            .set_credential_recheck_interval_for_tests(interval);
    }
    TestApp {
        router: api::router(app.clone()),
        app,
        dir,
    }
}

async fn create_pairing(t: &TestApp, cookie: &str) -> Reply {
    send(
        &t.router,
        request_with(
            Method::POST,
            "/api/devices/pairing",
            Some(cookie),
            None,
            Some(ORIGIN),
            None,
            None,
        ),
    )
    .await
}

fn pairing_code(reply: &Reply) -> (String, Value) {
    let decoded = URL_SAFE_NO_PAD
        .decode(reply.body["payload"].as_str().unwrap())
        .unwrap();
    let payload: Value = serde_json::from_slice(&decoded).unwrap();
    (payload["code"].as_str().unwrap().to_owned(), payload)
}

async fn pair(t: &TestApp, code: &str, name: &str) -> Reply {
    send(
        &t.router,
        request_with(
            Method::POST,
            "/api/devices/pair",
            None,
            None,
            None,
            Some("192.0.2.10"),
            Some(json!({ "code": code, "name": name })),
        ),
    )
    .await
}

async fn create_device(t: &TestApp, cookie: &str, name: &str) -> (String, String, String) {
    let pairing = create_pairing(t, cookie).await;
    assert_eq!(pairing.status, StatusCode::CREATED, "{}", pairing.body);
    let (code, _) = pairing_code(&pairing);
    let paired = pair(t, &code, name).await;
    assert_eq!(paired.status, StatusCode::CREATED, "{}", paired.body);
    (
        paired.body["token"].as_str().unwrap().to_owned(),
        paired.body["device_id"].as_str().unwrap().to_owned(),
        code,
    )
}

async fn bearer(
    t: &TestApp,
    method: Method,
    path: &str,
    token: &str,
    cookie: Option<&str>,
    origin: Option<&str>,
    body: Option<Value>,
) -> Reply {
    send(
        &t.router,
        request_with(
            method,
            path,
            cookie,
            Some(&format!("Bearer {token}")),
            origin,
            None,
            body,
        ),
    )
    .await
}

#[tokio::test]
async fn pairing_payload_status_and_bearer_cycle() {
    let (_core, url) = fake_core().await;
    let t = start_with_core(&url, None);
    let session = login(&t, "liis", "password").await.session();
    let pairing = create_pairing(&t, &session).await;
    assert_eq!(pairing.status, StatusCode::CREATED);
    assert_eq!(pairing.headers[header::CACHE_CONTROL], "no-store");
    let (code, payload) = pairing_code(&pairing);
    assert_eq!(payload["v"], 1);
    assert_eq!(payload["url"], ORIGIN);
    assert_eq!(code.len(), 43);
    let expires_at = pairing.body["expires_at"].as_i64().unwrap();
    assert!((expires_at - now_ms() - 120_000).abs() < 1_000);

    let pairing_id = pairing.body["pairing_id"].as_str().unwrap();
    let pending = send(
        &t.router,
        request_with(
            Method::GET,
            &format!("/api/devices/pairing/{pairing_id}"),
            Some(&session),
            None,
            None,
            None,
            None,
        ),
    )
    .await;
    assert_eq!(pending.status, StatusCode::OK);
    assert_eq!(pending.body, json!({ "status": "pending" }));

    let paired = pair(&t, &code, "  Pixel  ").await;
    assert_eq!(paired.status, StatusCode::CREATED);
    assert_eq!(paired.headers[header::CACHE_CONTROL], "no-store");
    assert_eq!(paired.body["username"], "liis");
    assert!(paired.body["token"].as_str().unwrap().starts_with("lgd_"));
    let token = paired.body["token"].as_str().unwrap();
    let used = send(
        &t.router,
        request_with(
            Method::GET,
            &format!("/api/devices/pairing/{pairing_id}"),
            Some(&session),
            None,
            None,
            None,
            None,
        ),
    )
    .await;
    assert_eq!(
        used.body,
        json!({ "status": "used", "device_id": paired.body["device_id"] })
    );

    assert_eq!(
        bearer(&t, Method::GET, "/api/me", token, None, None, None)
            .await
            .status,
        StatusCode::OK
    );
    let search = bearer(
        &t,
        Method::POST,
        "/api/logs/search",
        token,
        None,
        None,
        Some(json!({
            "from": "2026-10-13T10:00:00Z",
            "to": "2026-10-13T11:00:00Z",
            "limit": 1000
        })),
    )
    .await;
    assert_eq!(search.status, StatusCode::OK, "{}", search.body);
    assert_eq!(
        pair(&t, &code, "another phone").await.body,
        json!({ "error": "invalid_code" })
    );
}

#[tokio::test]
async fn parallel_pairing_exchange_consumes_code_once() {
    let t = start(&[("liis", "password")], &[]);
    let session = login(&t, "liis", "password").await.session();
    let pairing = create_pairing(&t, &session).await;
    let (code, _) = pairing_code(&pairing);

    let (first, second) = tokio::join!(pair(&t, &code, "phone one"), pair(&t, &code, "phone two"));
    let statuses = [first.status, second.status];
    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == StatusCode::CREATED)
            .count(),
        1
    );
    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == StatusCode::UNAUTHORIZED)
            .count(),
        1
    );
}

#[tokio::test]
async fn malformed_htpasswd_temporarily_blocks_devices_without_revoking_them_or_pairing() {
    let t = start(&[("liis", "password")], &[]);
    let session = login(&t, "liis", "password").await.session();
    let (token, _, _) = create_device(&t, &session, "existing phone").await;
    let pairing = create_pairing(&t, &session).await;
    let (code, _) = pairing_code(&pairing);
    let path = t.htpasswd_path();
    let valid_htpasswd = std::fs::read(&path).unwrap();

    std::fs::write(&path, "temporarily invalid htpasswd\n").unwrap();
    let rejected_pairing = pair(&t, &code, "new phone").await;
    assert_eq!(rejected_pairing.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        bearer(&t, Method::GET, "/api/me", &token, None, None, None)
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );

    std::fs::write(&path, valid_htpasswd).unwrap();
    let paired = pair(&t, &code, "new phone").await;
    assert_eq!(paired.status, StatusCode::CREATED, "{}", paired.body);
    assert_eq!(
        bearer(&t, Method::GET, "/api/me", &token, None, None, None)
            .await
            .status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn pairing_replacement_expiry_and_owner_checks() {
    let t = start(&[("liis", "password"), ("anna", "password")], &[]);
    let liis = login(&t, "liis", "password").await.session();
    let anna = login(&t, "anna", "password").await.session();
    let first = create_pairing(&t, &liis).await;
    let (old_code, _) = pairing_code(&first);
    let id = first.body["pairing_id"].as_str().unwrap().to_owned();

    let foreign = send(
        &t.router,
        request_with(
            Method::GET,
            &format!("/api/devices/pairing/{id}"),
            Some(&anna),
            None,
            None,
            None,
            None,
        ),
    )
    .await;
    assert_eq!(foreign.status, StatusCode::NOT_FOUND);
    assert_eq!(foreign.body["error"], "not_found");

    let second = create_pairing(&t, &liis).await;
    let replaced = send(
        &t.router,
        request_with(
            Method::GET,
            &format!("/api/devices/pairing/{id}"),
            Some(&liis),
            None,
            None,
            None,
            None,
        ),
    )
    .await;
    assert_eq!(replaced.body, json!({ "status": "replaced" }));
    let old_pair = pair(&t, &old_code, "old phone").await;
    assert_eq!(old_pair.status, StatusCode::UNAUTHORIZED);
    assert_eq!(old_pair.body["error"], "invalid_code");
    let (new_code, _) = pairing_code(&second);
    let expired_id = second.body["pairing_id"].as_str().unwrap().to_owned();
    let expired_id_for_db = expired_id.clone();
    t.app
        .store
        .call(move |conn| {
            conn.execute(
                "UPDATE device_pairings SET expires_at=?2 WHERE id=?1",
                rusqlite::params![expired_id_for_db, now_ms() - 1],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let expired = send(
        &t.router,
        request_with(
            Method::GET,
            &format!("/api/devices/pairing/{expired_id}"),
            Some(&liis),
            None,
            None,
            None,
            None,
        ),
    )
    .await;
    assert_eq!(expired.body, json!({ "status": "expired" }));
    let expired_pair = pair(&t, &new_code, "late phone").await;
    assert_eq!(expired_pair.status, StatusCode::UNAUTHORIZED);
    assert_eq!(expired_pair.body["error"], "invalid_code");
}

#[tokio::test]
async fn password_change_revokes_devices_and_pending_codes() {
    let t = start(&[("liis", "password")], &[]);
    let session = login(&t, "liis", "password").await.session();
    let (token, _, old_code) = create_device(&t, &session, "phone").await;
    let pending = create_pairing(&t, &session).await;
    let (pending_code, _) = pairing_code(&pending);
    let changed = send(
        &t.router,
        request(
            Method::POST,
            "/api/account/password",
            Some(&session),
            Some(json!({ "current": "password", "new": "new-password" })),
        ),
    )
    .await;
    assert_eq!(changed.status, StatusCode::NO_CONTENT);
    for code in [old_code, pending_code] {
        let reply = pair(&t, &code, "phone").await;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
        assert_eq!(reply.body["error"], "invalid_code");
    }
    let device = bearer(&t, Method::GET, "/api/me", &token, None, None, None).await;
    assert_eq!(device.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn device_bearer_is_rejected_outside_allowlist_and_never_falls_back_to_cookie() {
    let t = start(&[("liis", "password")], &[]);
    let session = login(&t, "liis", "password").await.session();
    let no_origin_pairing = send(
        &t.router,
        request_with(
            Method::POST,
            "/api/devices/pairing",
            Some(&session),
            None,
            None,
            None,
            None,
        ),
    )
    .await;
    assert_eq!(no_origin_pairing.status, StatusCode::FORBIDDEN);
    let (token, _, _) = create_device(&t, &session, "phone").await;
    for (method, path, body) in [
        (
            Method::POST,
            "/api/account/password",
            Some(json!({ "current": "password", "new": "new-password" })),
        ),
        (Method::GET, "/api/relays", None),
        (Method::GET, "/api/mcp/personal-tokens", None),
        (Method::POST, "/api/devices/pairing", None),
        (Method::GET, "/api/devices", None),
        (Method::HEAD, "/api/me", None),
        (Method::GET, "/api/me/", None),
        (
            Method::POST,
            "/api/devices/pair",
            Some(json!({ "code": "not-a-code", "name": "phone" })),
        ),
    ] {
        let reply = bearer(&t, method, path, &token, None, None, body).await;
        assert_eq!(
            reply.status,
            StatusCode::UNAUTHORIZED,
            "{path}: {}",
            reply.body
        );
    }

    let bad_bearer_with_cookie = send(
        &t.router,
        request_with(
            Method::POST,
            "/api/account/password",
            Some(&session),
            Some("Bearer garbage"),
            None,
            None,
            Some(json!({ "current": "password", "new": "new-password" })),
        ),
    )
    .await;
    assert!(matches!(
        bad_bearer_with_cookie.status,
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
    ));

    let valid_bearer_with_cookie = bearer(
        &t,
        Method::POST,
        "/api/account/password",
        &token,
        Some(&session),
        Some(ORIGIN),
        Some(json!({ "current": "password", "new": "new-password" })),
    )
    .await;
    assert_eq!(valid_bearer_with_cookie.status, StatusCode::UNAUTHORIZED);

    let cookie_without_origin = send(
        &t.router,
        request_with(
            Method::POST,
            "/api/account/password",
            Some(&session),
            None,
            None,
            None,
            Some(json!({ "current": "password", "new": "new-password" })),
        ),
    )
    .await;
    assert_eq!(cookie_without_origin.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn device_bearer_works_on_multiple_allowlisted_routes() {
    let (_core, url) = fake_core().await;
    let t = start_with_core(&url, None);
    let session = login(&t, "liis", "password").await.session();
    let (token, _, _) = create_device(&t, &session, "phone").await;

    let me = bearer(&t, Method::GET, "/api/me", &token, None, None, None).await;
    assert_eq!(me.status, StatusCode::OK);
    assert_eq!(me.body["username"], "liis");

    let series = bearer(&t, Method::GET, "/api/series", &token, None, None, None).await;
    assert_eq!(series.status, StatusCode::OK, "{}", series.body);

    let favorites = bearer(
        &t,
        Method::GET,
        "/api/series/favorites",
        &token,
        None,
        None,
        None,
    )
    .await;
    assert_eq!(favorites.status, StatusCode::OK, "{}", favorites.body);

    let series_query = bearer(
        &t,
        Method::POST,
        "/api/series/query",
        &token,
        None,
        None,
        Some(json!({
            "service": "api",
            "name": "requests",
            "from": "2026-10-13T10:00:00Z",
            "to": "2026-10-13T11:00:00Z",
            "step_seconds": 60,
            "split": "none"
        })),
    )
    .await;
    assert_eq!(series_query.status, StatusCode::OK, "{}", series_query.body);
}

#[tokio::test]
async fn device_revocation_and_current_route_only_revoke_the_presented_device() {
    let t = start(&[("liis", "password"), ("anna", "password")], &[]);
    let session = login(&t, "liis", "password").await.session();
    let anna = login(&t, "anna", "password").await.session();
    let (first_token, first_id, _) = create_device(&t, &session, "first").await;
    let (second_token, _, _) = create_device(&t, &session, "second").await;
    let listing = send(
        &t.router,
        request(Method::GET, "/api/devices", Some(&session), None),
    )
    .await;
    assert_eq!(listing.status, StatusCode::OK);
    assert_eq!(listing.body["devices"].as_array().unwrap().len(), 2);
    assert!(
        listing.body["devices"]
            .as_array()
            .unwrap()
            .iter()
            .any(|device| { device["name"] == "first" && device.get("token").is_none() })
    );

    let foreign_delete = send(
        &t.router,
        request(
            Method::DELETE,
            &format!("/api/devices/{first_id}"),
            Some(&anna),
            None,
        ),
    )
    .await;
    assert_eq!(foreign_delete.status, StatusCode::NOT_FOUND);

    let cookie_current = send(
        &t.router,
        request(Method::DELETE, "/api/devices/current", Some(&session), None),
    )
    .await;
    assert_eq!(cookie_current.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        bearer(&t, Method::GET, "/api/me", &first_token, None, None, None)
            .await
            .status,
        StatusCode::OK
    );

    let revoked = send(
        &t.router,
        request(
            Method::DELETE,
            &format!("/api/devices/{first_id}"),
            Some(&session),
            None,
        ),
    )
    .await;
    assert_eq!(revoked.status, StatusCode::NO_CONTENT);
    assert_eq!(
        bearer(&t, Method::GET, "/api/me", &first_token, None, None, None)
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        bearer(&t, Method::GET, "/api/me", &second_token, None, None, None)
            .await
            .status,
        StatusCode::OK
    );

    let self_revoked = bearer(
        &t,
        Method::DELETE,
        "/api/devices/current",
        &second_token,
        None,
        None,
        None,
    )
    .await;
    assert_eq!(self_revoked.status, StatusCode::NO_CONTENT);
    assert_eq!(
        bearer(&t, Method::GET, "/api/me", &second_token, None, None, None)
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn device_limit_is_ten_active_devices_per_user() {
    let t = start(&[("liis", "password")], &[]);
    let session = login(&t, "liis", "password").await.session();
    for index in 0..10 {
        create_device(&t, &session, &format!("phone {index}")).await;
    }
    let pairing = create_pairing(&t, &session).await;
    let (code, _) = pairing_code(&pairing);
    let limited = pair(&t, &code, "eleventh").await;
    assert_eq!(limited.status, StatusCode::CONFLICT);
    assert_eq!(limited.body["error"], "device_limit");
}

#[tokio::test]
async fn removed_htpasswd_user_loses_and_revokes_device() {
    let t = start(&[("liis", "password"), ("anna", "password")], &[]);
    let session = login(&t, "liis", "password").await.session();
    let (token, id, _) = create_device(&t, &session, "phone").await;
    std::fs::write(t.htpasswd_path(), htpasswd_lines(&[("anna", "password")])).unwrap();
    let rejected = bearer(&t, Method::GET, "/api/me", &token, None, None, None).await;
    assert_eq!(rejected.status, StatusCode::UNAUTHORIZED);
    let id_for_db = id.clone();
    let revoked_at: Option<i64> = t
        .app
        .store
        .call(move |conn| {
            conn.query_row(
                "SELECT revoked_at FROM devices WHERE id=?1",
                [id_for_db],
                |row| row.get(0),
            )
        })
        .await
        .unwrap();
    assert!(revoked_at.is_some());
}

#[tokio::test]
async fn live_bearer_stream_closes_after_device_revocation() {
    let (_core, url) = fake_core().await;
    let t = start_with_core(&url, Some(Duration::from_millis(20)));
    let session = login(&t, "liis", "password").await.session();
    let (token, id, _) = create_device(&t, &session, "phone").await;
    let response = t
        .router
        .clone()
        .oneshot(request_with(
            Method::GET,
            "/api/logs/stream",
            None,
            Some(&format!("Bearer {token}")),
            None,
            None,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body();
    tokio::time::timeout(Duration::from_secs(2), body.frame())
        .await
        .expect("initial SSE snapshot")
        .expect("stream stays open")
        .unwrap();

    let revoked = send(
        &t.router,
        request(
            Method::DELETE,
            &format!("/api/devices/{id}"),
            Some(&session),
            None,
        ),
    )
    .await;
    assert_eq!(revoked.status, StatusCode::NO_CONTENT);
    tokio::time::timeout(Duration::from_secs(2), async {
        while body.frame().await.is_some() {}
    })
    .await
    .expect("SSE ends after credential recheck");
    t.app.shutdown.cancel();
}

#[tokio::test]
async fn pairing_attempts_are_limited_by_reverse_proxy_ip() {
    let t = start(&[("liis", "password")], &[]);
    for _ in 0..5 {
        let reply = pair(&t, "wrong-code", "phone").await;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
        assert_eq!(reply.body["error"], "invalid_code");
    }
    let blocked = pair(&t, "wrong-code", "phone").await;
    assert_eq!(blocked.status, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn device_pairing_rejects_invalid_json_and_names() {
    let t = start(&[("liis", "password")], &[]);
    let malformed = Request::builder()
        .method(Method::POST)
        .uri("/api/devices/pair")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from("{"))
        .unwrap();
    let malformed = send(&t.router, malformed).await;
    assert_eq!(malformed.status, StatusCode::BAD_REQUEST);
    assert_eq!(malformed.body["error"], "invalid_request");

    let unknown = pair(&t, "irrelevant", "phone").await;
    assert_eq!(unknown.status, StatusCode::UNAUTHORIZED);
    let invalid_name = send(
        &t.router,
        request_with(
            Method::POST,
            "/api/devices/pair",
            None,
            None,
            None,
            Some("192.0.2.11"),
            Some(json!({ "code": "irrelevant", "name": "\u{0007}" })),
        ),
    )
    .await;
    assert_eq!(invalid_name.status, StatusCode::BAD_REQUEST);
    assert_eq!(invalid_name.body["error"], "invalid_request");

    let unknown_field = send(
        &t.router,
        request_with(
            Method::POST,
            "/api/devices/pair",
            None,
            None,
            None,
            Some("192.0.2.12"),
            Some(json!({ "code": "irrelevant", "name": "phone", "extra": true })),
        ),
    )
    .await;
    assert_eq!(unknown_field.status, StatusCode::BAD_REQUEST);
    assert_eq!(unknown_field.body["error"], "invalid_request");
}

#[tokio::test]
async fn device_migration_runs_from_previous_user_version() {
    let t = start(&[("liis", "password")], &[]);
    let (current_version, previous_version, old_tables) = t
        .app
        .store
        .call(|conn| {
            let current_version: i64 =
                conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
            conn.execute_batch("DROP TABLE devices; DROP TABLE device_pairings;")?;
            let tables: i64 = conn.query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name IN ('devices','device_pairings')",
                [],
                |row| row.get(0),
            )?;
            let previous_version = current_version - 1;
            conn.pragma_update(None, "user_version", previous_version)?;
            Ok((current_version, previous_version, tables))
        })
        .await
        .unwrap();
    assert_eq!(current_version, CURRENT_SCHEMA_VERSION);
    assert_eq!(old_tables, 0);
    let TestApp { app, router, dir } = t;
    drop(router);
    drop(app);
    let migrated = App::open(config(dir.path(), &[])).unwrap();
    let (version, tables) = migrated
        .store
        .call(|conn| {
            let version: i64 =
                conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
            let tables: i64 = conn.query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name IN ('devices','device_pairings')",
                [],
                |row| row.get(0),
            )?;
            Ok((version, tables))
        })
        .await
        .unwrap();
    assert_eq!(previous_version, current_version - 1);
    assert_eq!(version, previous_version + 1);
    assert_eq!(version, CURRENT_SCHEMA_VERSION);
    assert_eq!(tables, 2);
}
