use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use lognara_core::{
    access::{Access, AccessError},
    api,
    config::Config,
    storage::Core,
};
use sha2::{Digest, Sha256};
use tower::ServiceExt;

async fn call(
    router: &Router,
    method: &str,
    path: &str,
    token: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    (
        status,
        if bytes.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        },
    )
}

#[tokio::test]
async fn admin_crud_isolated_agents_and_revoked_relay() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::from_lookup(|key| match key {
        "LOGNARA_ADMIN_TOKEN" => Some("admin".into()),
        "LOGNARA_QUERY_TOKEN" => Some("query".into()),
        _ => None,
    })
    .unwrap();
    config.data_dir = dir.path().into();
    let core = Core::open(config).unwrap();
    let router = api::router(core.clone());
    let nil = serde_json::Value::Null;
    for token in ["", "query", "unknown"] {
        assert_eq!(
            call(&router, "GET", "/v1/admin/relays", token, nil.clone())
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
    }
    for name in [" ".to_string(), "x".repeat(65), "x\ny".into()] {
        let (status, error) = call(
            &router,
            "POST",
            "/v1/admin/relays",
            "admin",
            serde_json::json!({"name":name}),
        )
        .await;
        assert_eq!(status, 400);
        assert_eq!(error["error"], "invalid_name");
    }
    let (status, first) = call(
        &router,
        "POST",
        "/v1/admin/relays",
        "admin",
        serde_json::json!({"name":" relay "}),
    )
    .await;
    assert_eq!(status, 201);
    assert_eq!(first["relay"]["name"], "relay");
    let id = first["relay"]["id"].as_i64().unwrap();
    let token = first["token"].as_str().unwrap();
    assert!(token.starts_with("lnr_"));
    assert_eq!(token.len(), 68);
    let (_, second) = call(
        &router,
        "POST",
        "/v1/admin/relays",
        "admin",
        serde_json::json!({"name":"other"}),
    )
    .await;
    assert_ne!(first["token"], second["token"]);
    let second_id = second["relay"]["id"].as_i64().unwrap();
    let (status, error) = call(
        &router,
        "POST",
        "/v1/admin/relays",
        "admin",
        serde_json::json!({"name":"relay"}),
    )
    .await;
    assert_eq!(status, 409);
    assert_eq!(error["error"], "name_taken");
    let (status, error) = call(
        &router,
        "POST",
        "/v1/admin/agents",
        "admin",
        serde_json::json!({"name":"agent","relay_id":999}),
    )
    .await;
    assert_eq!(status, 404);
    assert_eq!(error["error"], "relay_not_found");
    let (status, agent) = call(
        &router,
        "POST",
        "/v1/admin/agents",
        "admin",
        serde_json::json!({"name":"agent","relay_id":id}),
    )
    .await;
    assert_eq!(status, 201);
    assert!(agent["token"].as_str().unwrap().starts_with("lna_"));
    assert_eq!(
        call(
            &router,
            "POST",
            "/v1/admin/agents",
            "admin",
            serde_json::json!({"name":"agent","relay_id":id})
        )
        .await
        .0,
        409
    );
    assert_eq!(
        call(
            &router,
            "POST",
            "/v1/admin/agents",
            "admin",
            serde_json::json!({"name":"agent","relay_id":second_id})
        )
        .await
        .0,
        201
    );
    let (_, relays) = call(&router, "GET", "/v1/admin/relays", "admin", nil.clone()).await;
    assert_eq!(relays["relays"].as_array().unwrap().len(), 2);
    assert!(relays.to_string().find("token").is_none());
    let (_, agents) = call(&router, "GET", "/v1/admin/agents", "admin", nil.clone()).await;
    assert_eq!(agents["agents"].as_array().unwrap().len(), 2);
    assert!(agents.to_string().find("token").is_none());
    let (_, hashes) = call(&router, "GET", "/v1/agents", token, nil.clone()).await;
    assert_eq!(
        hashes["agents"],
        serde_json::json!([hex::encode(Sha256::digest(
            agent["token"].as_str().unwrap().as_bytes()
        ))])
    );
    for bad in ["admin", "query", agent["token"].as_str().unwrap()] {
        assert_eq!(
            call(&router, "GET", "/v1/agents", bad, nil.clone()).await.0,
            401
        );
        assert_eq!(
            call(&router, "POST", "/v1/batches", bad, nil.clone())
                .await
                .0,
            401
        );
    }
    let path = format!("/v1/admin/relays/{id}");
    let (status, error) = call(&router, "DELETE", &path, "admin", nil.clone()).await;
    assert_eq!(status, 409);
    assert_eq!(error["error"], "relay_has_agents");
    let agent_path = format!("/v1/admin/agents/{}", agent["agent"]["id"]);
    assert_eq!(
        call(&router, "DELETE", &agent_path, "admin", nil.clone())
            .await
            .0,
        204
    );
    assert_eq!(
        call(&router, "DELETE", &agent_path, "admin", nil.clone())
            .await
            .0,
        404
    );
    assert_eq!(
        call(&router, "GET", "/v1/agents", token, nil.clone())
            .await
            .1["agents"],
        serde_json::json!([])
    );
    assert_eq!(
        call(&router, "DELETE", &path, "admin", nil.clone()).await.0,
        204
    );
    assert_eq!(
        call(&router, "DELETE", &path, "admin", nil.clone()).await.0,
        404
    );
    assert_eq!(
        call(&router, "GET", "/v1/agents", token, nil.clone())
            .await
            .0,
        401
    );
    assert_eq!(
        call(&router, "POST", "/v1/batches", token, nil.clone())
            .await
            .0,
        401
    );
    assert_eq!(
        call(
            &router,
            "POST",
            "/v1/admin/relays",
            "admin",
            serde_json::json!({"invalid":true})
        )
        .await
        .0,
        400
    );
    core.shutdown().unwrap();
}

#[test]
fn registry_survives_restart_and_stores_only_hashes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("access.sqlite3");
    let access = Access::open(&path).unwrap();
    let relay = access.create_relay(&"я".repeat(64)).unwrap();
    let hash: [u8; 32] = Sha256::digest(relay.token.as_bytes()).into();
    let agent = access.create_agent("agent", relay.relay.id).unwrap();
    assert!(matches!(
        access.create_relay("bad\0name"),
        Err(AccessError::InvalidName)
    ));
    drop(access);
    let access = Access::open(&path).unwrap();
    assert_eq!(access.relay_for_token(&hash), Some(relay.relay.id));
    assert_eq!(
        access.agent_hashes(relay.relay.id).unwrap(),
        vec![hex::encode(Sha256::digest(agent.token.as_bytes()))]
    );
    let connection = rusqlite::Connection::open(&path).unwrap();
    let stored: Vec<u8> = connection
        .query_row("SELECT token_hash FROM relays", [], |row| row.get(0))
        .unwrap();
    assert_eq!(stored, hash);
    assert!(
        !std::fs::read(path)
            .unwrap()
            .windows(relay.token.len())
            .any(|bytes| bytes == relay.token.as_bytes())
    );
    access.delete_agent(agent.agent.id).unwrap();
    access.delete_relay(relay.relay.id).unwrap();
    assert_eq!(access.relay_for_token(&hash), None);
    let replacement = access.create_relay("replacement").unwrap();
    assert_ne!(replacement.relay.id, relay.relay.id);
}

#[test]
fn admin_and_query_tokens_must_differ() {
    assert!(
        Config::from_lookup(|key| match key {
            "LOGNARA_ADMIN_TOKEN" | "LOGNARA_QUERY_TOKEN" => Some("same".into()),
            _ => None,
        })
        .is_err()
    );
    assert!(
        Config::from_lookup(|key| match key {
            "LOGNARA_INGEST_TOKEN" => Some("legacy".into()),
            "LOGNARA_QUERY_TOKEN" => Some("query".into()),
            _ => None,
        })
        .is_err()
    );
}
