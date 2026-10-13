mod common;

use axum::http::{Method, StatusCode};
use common::*;
use serde_json::json;

async fn change(t: &TestApp, cookie: &str, current: &str, new: &str) -> Reply {
    send(
        &t.router,
        request(
            Method::POST,
            "/api/account/password",
            Some(cookie),
            Some(json!({ "current": current, "new": new })),
        ),
    )
    .await
}

#[tokio::test]
async fn password_change_rewrites_only_own_line_and_revokes_other_sessions() {
    let t = start(&[], &[]);
    let original = format!(
        "# users\n{}\r\n{}",
        htpasswd_lines(&[("anna", "anna-pass")]).trim_end(),
        htpasswd_lines(&[("liis", "old-password"), ("bob", "bob-pass")])
    );
    std::fs::write(t.htpasswd_path(), &original).unwrap();

    let current = login(&t, "liis", "old-password").await.session();
    let other = login(&t, "liis", "old-password").await.session();
    let reply = change(&t, &current, "old-password", "new-password").await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{:?}", reply.body);

    let updated = std::fs::read_to_string(t.htpasswd_path()).unwrap();
    let before: Vec<&str> = original.split_inclusive('\n').collect();
    let after: Vec<&str> = updated.split_inclusive('\n').collect();
    assert_eq!(before.len(), after.len());
    for (old, new) in before.iter().zip(&after) {
        if old.starts_with("liis:") {
            assert!(new.starts_with("liis:$2y$10$"), "{new}");
            assert_ne!(old, new);
        } else {
            assert_eq!(old, new);
        }
    }
    // Временный файл не остаётся в смонтированной папке.
    let names: Vec<_> = std::fs::read_dir(t.dir.path().join("auth"))
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, ["htpasswd"]);

    assert_eq!(me(&t, &current).await.status, StatusCode::OK);
    assert_eq!(me(&t, &other).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        login(&t, "liis", "old-password").await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        login(&t, "liis", "new-password").await.status,
        StatusCode::OK
    );
    assert_eq!(login(&t, "anna", "anna-pass").await.status, StatusCode::OK);
}

#[tokio::test]
async fn password_change_validates_input() {
    let t = start(&[("liis", "old-password")], &[]);
    let session = login(&t, "liis", "old-password").await.session();
    let before = std::fs::read_to_string(t.htpasswd_path()).unwrap();

    let short = change(&t, &session, "old-password", "short").await;
    assert_eq!(short.status, StatusCode::BAD_REQUEST);
    assert_eq!(short.body["error"], "password_too_short");
    let long = change(&t, &session, "old-password", &"x".repeat(72)).await;
    assert_eq!(long.body["error"], "password_too_long");
    let wrong = change(&t, &session, "wrong", "new-password").await;
    assert_eq!(wrong.body["error"], "invalid_current_password");
    assert_eq!(std::fs::read_to_string(t.htpasswd_path()).unwrap(), before);

    let anonymous = send(
        &t.router,
        request(
            Method::POST,
            "/api/account/password",
            None,
            Some(json!({ "current": "old-password", "new": "new-password" })),
        ),
    )
    .await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
}
