//! Полная церемония passkey с программным аутентификатором.
//!
//! SoftPasskey не хранит resident key, поэтому тест делает то, что делает браузер при
//! выборе сохранённого ключа: подставляет его в allowCredentials и возвращает user handle.
//! Эти поля не входят в подпись, проверка сервера остаётся полной. Требование resident key
//! сервер только передаёт браузеру, поэтому для SoftPasskey оно снимается.

mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use common::*;
use serde_json::{Value, json};
use url::Url;
use webauthn_authenticator_rs::WebauthnAuthenticator;
use webauthn_authenticator_rs::softpasskey::SoftPasskey;
use webauthn_rs_proto::{
    AllowCredentials, CreationChallengeResponse, PublicKeyCredential, RequestChallengeResponse,
};

/// SoftPasskey отказывается от resident key; сервер его не проверяет.
fn soft_options(mut options: CreationChallengeResponse) -> CreationChallengeResponse {
    if let Some(selection) = options.public_key.authenticator_selection.as_mut() {
        selection.resident_key = None;
        selection.require_resident_key = false;
    }
    options
}

fn with_cookies(method: Method, path: &str, cookies: &[&str], body: Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(path)
        .header(header::ORIGIN, ORIGIN)
        .header(header::COOKIE, cookies.join("; "))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

struct Registered {
    credential_id: Vec<u8>,
    user_handle: Vec<u8>,
}

async fn register(
    t: &TestApp,
    session: &str,
    authenticator: &mut WebauthnAuthenticator<SoftPasskey>,
    name: &str,
) -> (Reply, Option<Registered>) {
    let start = send(
        &t.router,
        with_cookies(
            Method::POST,
            "/api/account/passkeys/start",
            &[session],
            json!({ "name": name, "password": "password" }),
        ),
    )
    .await;
    assert_eq!(start.status, StatusCode::OK, "{:?}", start.body);
    let options: CreationChallengeResponse = serde_json::from_value(start.body.clone()).unwrap();
    let user_handle = options.public_key.user.id.to_vec();
    let credential = authenticator
        .do_registration(Url::parse(ORIGIN).unwrap(), soft_options(options))
        .unwrap();
    let credential_id = credential.raw_id.to_vec();
    let finish = send(
        &t.router,
        with_cookies(
            Method::POST,
            "/api/account/passkeys/finish",
            &[session, &start.cookie("lognara_dock_challenge")],
            serde_json::to_value(&credential).unwrap(),
        ),
    )
    .await;
    let registered = (finish.status == StatusCode::OK).then_some(Registered {
        credential_id,
        user_handle,
    });
    (finish, registered)
}

async fn passkey_login(
    t: &TestApp,
    authenticator: &mut WebauthnAuthenticator<SoftPasskey>,
    key: &Registered,
) -> (Reply, Value, String) {
    let start = send(
        &t.router,
        request(Method::POST, "/api/auth/passkey/start", None, None),
    )
    .await;
    assert_eq!(start.status, StatusCode::OK, "{:?}", start.body);
    assert_eq!(start.body["publicKey"]["rpId"], "localhost");
    let mut options: RequestChallengeResponse = serde_json::from_value(start.body.clone()).unwrap();
    assert!(options.public_key.allow_credentials.is_empty());
    options.public_key.allow_credentials = vec![AllowCredentials {
        type_: "public-key".into(),
        id: key.credential_id.clone().into(),
        transports: None,
    }];
    let mut credential: PublicKeyCredential = authenticator
        .do_authentication(Url::parse(ORIGIN).unwrap(), options)
        .unwrap();
    credential.response.user_handle = Some(key.user_handle.clone().into());
    let body = serde_json::to_value(&credential).unwrap();
    let challenge = start.cookie("lognara_dock_challenge");
    let finish = send(
        &t.router,
        with_cookies(
            Method::POST,
            "/api/auth/passkey/finish",
            &[&challenge],
            body.clone(),
        ),
    )
    .await;
    (finish, body, challenge)
}

#[tokio::test]
async fn registers_passkey_and_logs_in_without_username() {
    let t = start(&[("liis", "password")], &[]);
    let session = login(&t, "liis", "password").await.session();
    let mut authenticator = WebauthnAuthenticator::new(SoftPasskey::new(true));

    let (finish, key) = register(&t, &session, &mut authenticator, "Laptop").await;
    assert_eq!(finish.status, StatusCode::OK, "{:?}", finish.body);
    assert_eq!(finish.body["name"], "Laptop");
    let key = key.unwrap();
    assert_eq!(me(&t, &session).await.body["passkeys"], 1);

    let (reply, body, challenge) = passkey_login(&t, &mut authenticator, &key).await;
    assert_eq!(reply.status, StatusCode::OK, "{:?}", reply.body);
    assert_eq!(reply.body["username"], "liis");
    let passkey_session = reply.session();
    assert_eq!(me(&t, &passkey_session).await.body["username"], "liis");

    // Challenge одноразовый.
    let replay = send(
        &t.router,
        with_cookies(
            Method::POST,
            "/api/auth/passkey/finish",
            &[&challenge],
            body,
        ),
    )
    .await;
    assert_eq!(replay.body["error"], "challenge_expired");

    let list = send(
        &t.router,
        request(Method::GET, "/api/account/passkeys", Some(&session), None),
    )
    .await;
    let passkeys = list.body["passkeys"].as_array().unwrap();
    assert_eq!(passkeys.len(), 1);
    assert!(passkeys[0]["last_used_at"].is_i64());
}

#[tokio::test]
async fn registration_requests_resident_key_and_requires_password() {
    let t = start(&[("liis", "password")], &[]);
    let session = login(&t, "liis", "password").await.session();
    let start = send(
        &t.router,
        request(
            Method::POST,
            "/api/account/passkeys/start",
            Some(&session),
            Some(json!({ "name": "Phone", "password": "password" })),
        ),
    )
    .await;
    let options = &start.body["publicKey"];
    assert_eq!(options["rp"]["id"], "localhost");
    assert_eq!(options["user"]["name"], "liis");
    assert_eq!(options["authenticatorSelection"]["residentKey"], "required");
    assert_eq!(
        options["authenticatorSelection"]["userVerification"],
        "required"
    );

    for (body, error) in [
        (
            json!({ "name": "Phone", "password": "wrong" }),
            "invalid_current_password",
        ),
        (
            json!({ "name": "  ", "password": "password" }),
            "invalid_name",
        ),
        (
            json!({ "name": "x".repeat(65), "password": "password" }),
            "invalid_name",
        ),
    ] {
        let reply = send(
            &t.router,
            request(
                Method::POST,
                "/api/account/passkeys/start",
                Some(&session),
                Some(body),
            ),
        )
        .await;
        assert_eq!(reply.body["error"], error);
    }
}

#[tokio::test]
async fn finish_rejects_missing_foreign_or_forged_challenge() {
    let t = start(&[("liis", "password"), ("anna", "password")], &[]);
    let liis = login(&t, "liis", "password").await.session();
    let anna = login(&t, "anna", "password").await.session();

    let start = send(
        &t.router,
        request(
            Method::POST,
            "/api/account/passkeys/start",
            Some(&liis),
            Some(json!({ "name": "Laptop", "password": "password" })),
        ),
    )
    .await;
    let options: CreationChallengeResponse = serde_json::from_value(start.body.clone()).unwrap();
    let mut authenticator = WebauthnAuthenticator::new(SoftPasskey::new(true));
    let credential = authenticator
        .do_registration(Url::parse(ORIGIN).unwrap(), soft_options(options))
        .unwrap();
    let body = serde_json::to_value(&credential).unwrap();
    let challenge = start.cookie("lognara_dock_challenge");

    for cookies in [
        vec![liis.as_str()],
        vec![liis.as_str(), "lognara_dock_challenge=forged"],
    ] {
        let reply = send(
            &t.router,
            with_cookies(
                Method::POST,
                "/api/account/passkeys/finish",
                &cookies,
                body.clone(),
            ),
        )
        .await;
        assert_eq!(reply.body["error"], "challenge_expired");
    }
    // Чужая сессия не может завершить регистрацию; challenge при этом сгорает.
    let foreign = send(
        &t.router,
        with_cookies(
            Method::POST,
            "/api/account/passkeys/finish",
            &[&anna, &challenge],
            body.clone(),
        ),
    )
    .await;
    assert_eq!(foreign.body["error"], "challenge_expired");

    let login_without_challenge = send(
        &t.router,
        with_cookies(Method::POST, "/api/auth/passkey/finish", &[], json!({})),
    )
    .await;
    assert_ne!(login_without_challenge.status, StatusCode::OK);
}

#[tokio::test]
async fn removed_user_and_deleted_passkey_cannot_log_in() {
    let t = start(&[("liis", "password"), ("anna", "password")], &[]);
    let session = login(&t, "liis", "password").await.session();
    let mut authenticator = WebauthnAuthenticator::new(SoftPasskey::new(true));
    let key = register(&t, &session, &mut authenticator, "Laptop")
        .await
        .1
        .unwrap();

    // Удаление требует пароль.
    let list = send(
        &t.router,
        request(Method::GET, "/api/account/passkeys", Some(&session), None),
    )
    .await;
    let id = list.body["passkeys"][0]["id"].as_i64().unwrap();
    let path = format!("/api/account/passkeys/{id}");
    let wrong = send(
        &t.router,
        request(
            Method::DELETE,
            &path,
            Some(&session),
            Some(json!({ "password": "wrong" })),
        ),
    )
    .await;
    assert_eq!(wrong.body["error"], "invalid_current_password");

    // Пользователь, удалённый из htpasswd, не входит по своему ключу.
    std::fs::write(t.htpasswd_path(), htpasswd_lines(&[("anna", "password")])).unwrap();
    let (reply, _, _) = passkey_login(&t, &mut authenticator, &key).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert!(
        reply
            .headers
            .get_all(header::SET_COOKIE)
            .iter()
            .all(|c| { !c.to_str().unwrap().starts_with("lognara_dock_session=") })
    );

    std::fs::write(
        t.htpasswd_path(),
        htpasswd_lines(&[("liis", "password"), ("anna", "password")]),
    )
    .unwrap();
    let session = login(&t, "liis", "password").await.session();
    let deleted = send(
        &t.router,
        request(
            Method::DELETE,
            &path,
            Some(&session),
            Some(json!({ "password": "password" })),
        ),
    )
    .await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT);
    let (reply, _, _) = passkey_login(&t, &mut authenticator, &key).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
}
