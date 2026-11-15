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
    let agent_hash: [u8; 32] = Sha256::digest(agent.token.as_bytes()).into();
    assert_eq!(
        access.agent_node(&lognara_core::wire::AgentHash(agent_hash)),
        Some(lognara_core::model::Node {
            relay_id: relay.relay.id,
            agent_id: agent.agent.id,
        })
    );
    assert_eq!(access.relay_for_token(&hash), Some(1));
    assert!(matches!(
        access.create_relay("bad\0name"),
        Err(AccessError::InvalidName)
    ));
    drop(access);
    let access = Access::open(&path).unwrap();
    assert_eq!(access.relay_for_token(&hash), Some(relay.relay.id));
    assert_eq!(
        access.agent_node(&lognara_core::wire::AgentHash(agent_hash)),
        Some(lognara_core::model::Node {
            relay_id: relay.relay.id,
            agent_id: agent.agent.id,
        })
    );
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
fn migrates_access_schema_v1_and_retains_agent_identity_hashes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("access.sqlite3");
    let agent_hash = [2_u8; 32];
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "PRAGMA foreign_keys=ON;
             CREATE TABLE relays(id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, token_hash BLOB NOT NULL UNIQUE, created_at TEXT NOT NULL);
             CREATE TABLE agents(id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, relay_id INTEGER NOT NULL REFERENCES relays(id), token_hash BLOB NOT NULL UNIQUE, created_at TEXT NOT NULL, UNIQUE(relay_id,name));
             CREATE TABLE heartbeats(kind TEXT NOT NULL CHECK(kind IN ('relay','agent')), node_id INTEGER NOT NULL, received_at INTEGER NOT NULL, telemetry TEXT NOT NULL, PRIMARY KEY(kind,node_id));
             INSERT INTO relays(name,token_hash,created_at) VALUES('relay',x'0101010101010101010101010101010101010101010101010101010101010101','now');
             INSERT INTO agents(name,relay_id,token_hash,created_at) VALUES('agent',1,x'0202020202020202020202020202020202020202020202020202020202020202','now');
             PRAGMA user_version=1;",
        )
        .unwrap();
    drop(connection);

    let access = Access::open(&path).unwrap();
    let version: u32 = rusqlite::Connection::open(&path)
        .unwrap()
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 3);
    let relays = access.list_relays().unwrap();
    assert_eq!(relays[0].kind, "relay");
    assert_eq!(
        access.agent_node(&lognara_core::wire::AgentHash(agent_hash)),
        Some(lognara_core::model::Node {
            relay_id: 1,
            agent_id: 1,
        })
    );
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

fn agent_report(server: &str) -> serde_json::Value {
    serde_json::json!({
        "version": "0.1.0",
        "source": {"environment": "prod", "server": server, "backend": "api", "service": "billing", "service_instance": null},
        "spool": {"used_bytes": 10, "limit_bytes": 100, "records": 2},
        "last_record_ms_ago": 4000, "last_delivery_ms_ago": null, "last_failure_ms_ago": 1000,
        "delivery_failures": 3
    })
}

#[tokio::test]
async fn rotating_agent_token_preserves_identity_and_revokes_old_hash_durably() {
    use serde_json::json;
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::from_lookup(|key| match key {
        "LOGNARA_ADMIN_TOKEN" => Some("admin".into()),
        "LOGNARA_QUERY_TOKEN" => Some("query".into()),
        _ => None,
    })
    .unwrap();
    config.data_dir = dir.path().into();
    let core = Core::open(config.clone()).unwrap();
    let router = api::router(core.clone());
    let nil = serde_json::Value::Null;
    let (_, relay) = call(
        &router,
        "POST",
        "/v1/admin/relays",
        "admin",
        json!({"name":"main"}),
    )
    .await;
    let relay_id = relay["relay"]["id"].as_i64().unwrap();
    let relay_token = relay["token"].as_str().unwrap();
    let (_, agent) = call(
        &router,
        "POST",
        "/v1/admin/agents",
        "admin",
        json!({"name":"api", "relay_id":relay_id}),
    )
    .await;
    let (_, other) = call(
        &router,
        "POST",
        "/v1/admin/agents",
        "admin",
        json!({"name":"worker", "relay_id":relay_id}),
    )
    .await;
    let hash =
        |token: &serde_json::Value| hex::encode(Sha256::digest(token.as_str().unwrap().as_bytes()));
    let old_hash = hash(&agent["token"]);
    let old_hash_bytes: [u8; 32] =
        Sha256::digest(agent["token"].as_str().unwrap().as_bytes()).into();
    let agent_id = agent["agent"]["id"].as_i64().unwrap();
    let node = lognara_core::model::Node { relay_id, agent_id };
    assert_eq!(
        core.journal
            .access
            .agent_node(&lognara_core::wire::AgentHash(old_hash_bytes)),
        Some(node)
    );
    let body = json!({"relay": relay_report(), "agents": [{"token_sha256": old_hash, "heartbeat_ms_ago": 0, "telemetry": agent_report("srv-1")}]});
    assert_eq!(
        call(&router, "POST", "/v1/heartbeat", relay_token, body)
            .await
            .0,
        204
    );
    let (_, before) = call(&router, "GET", "/v1/admin/agents", "admin", nil.clone()).await;
    let path = format!("/v1/admin/agents/{}/rotate-token", agent["agent"]["id"]);
    for token in ["", "query", relay_token, agent["token"].as_str().unwrap()] {
        assert_eq!(
            call(&router, "POST", &path, token, nil.clone()).await.0,
            401
        );
    }
    assert_eq!(
        call(
            &router,
            "POST",
            "/v1/admin/agents/999/rotate-token",
            "admin",
            nil.clone()
        )
        .await,
        (StatusCode::NOT_FOUND, json!({"error":"not_found"}))
    );
    assert_eq!(
        call(
            &router,
            "POST",
            "/v1/admin/agents/abc/rotate-token",
            "admin",
            nil.clone()
        )
        .await
        .0,
        400
    );
    let (status, rotated) = call(&router, "POST", &path, "admin", nil.clone()).await;
    assert_eq!(status, 200);
    assert_ne!(rotated["token"], agent["token"]);
    assert_eq!(
        core.journal
            .access
            .agent_node(&lognara_core::wire::AgentHash(old_hash_bytes)),
        Some(node)
    );
    assert!(rotated["token"].as_str().unwrap().starts_with("lna_"));
    assert_eq!(rotated["agent"], before["agents"][0]);
    let expected_hashes = {
        let mut hashes = vec![hash(&rotated["token"]), hash(&other["token"])];
        hashes.sort();
        json!(hashes)
    };
    let (_, hashes) = call(&router, "GET", "/v1/agents", relay_token, nil.clone()).await;
    assert_eq!(hashes["agents"], expected_hashes);
    assert!(!hashes.to_string().contains(&old_hash));
    let new_hash: [u8; 32] = Sha256::digest(rotated["token"].as_str().unwrap().as_bytes()).into();
    assert_eq!(
        core.journal
            .access
            .agent_node(&lognara_core::wire::AgentHash(new_hash)),
        Some(node)
    );
    let (_, after) = call(&router, "GET", "/v1/admin/agents", "admin", nil.clone()).await;
    assert_eq!(before["agents"], after["agents"]);
    assert!(!after.to_string().contains("token"));
    core.shutdown().unwrap();
    drop(router);
    drop(core);
    let core = Core::open(config).unwrap();
    let router = api::router(core.clone());
    let (_, hashes) = call(&router, "GET", "/v1/agents", relay_token, nil.clone()).await;
    assert_eq!(hashes["agents"], expected_hashes);
    assert_eq!(
        core.journal
            .access
            .agent_node(&lognara_core::wire::AgentHash(old_hash_bytes)),
        Some(node)
    );
    let (_, persisted) = call(&router, "GET", "/v1/admin/agents", "admin", nil).await;
    assert_eq!(persisted["agents"], before["agents"]);
    core.shutdown().unwrap();
}

fn relay_report() -> serde_json::Value {
    serde_json::json!({
        "version": "0.1.0",
        "spool": {"used_bytes": 1, "limit_bytes": 1000, "records": 1},
        "last_ingest_ms_ago": 500, "last_delivery_ms_ago": 200, "last_failure_ms_ago": null,
        "delivery_failures": 0, "events_per_second": 12.5
    })
}

fn millis(value: &serde_json::Value) -> i64 {
    lognara_core::model::parse_timestamp(value.as_str().unwrap()).unwrap() / 1_000_000
}

#[tokio::test]
async fn heartbeats_follow_authenticated_relay_and_core_clock() {
    use serde_json::json;
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::from_lookup(|key| match key {
        "LOGNARA_ADMIN_TOKEN" => Some("admin".into()),
        "LOGNARA_QUERY_TOKEN" => Some("query".into()),
        _ => None,
    })
    .unwrap();
    config.data_dir = dir.path().into();
    let core = Core::open(config.clone()).unwrap();
    let router = api::router(core.clone());
    let nil = serde_json::Value::Null;
    let (_, first) = call(
        &router,
        "POST",
        "/v1/admin/relays",
        "admin",
        json!({"name":"eu-1"}),
    )
    .await;
    let (_, second) = call(
        &router,
        "POST",
        "/v1/admin/relays",
        "admin",
        json!({"name":"eu-2"}),
    )
    .await;
    let relay_token = first["token"].as_str().unwrap();
    let mut agents = vec![];
    for (name, relay) in [("own", &first), ("foreign", &second), ("silent", &first)] {
        let body = json!({"name":name,"relay_id":relay["relay"]["id"]});
        agents.push(
            call(&router, "POST", "/v1/admin/agents", "admin", body)
                .await
                .1,
        );
    }
    let hash = |agent: &serde_json::Value| {
        hex::encode(Sha256::digest(agent["token"].as_str().unwrap().as_bytes()))
    };
    let (_, before) = call(&router, "GET", "/v1/admin/agents", "admin", nil.clone()).await;
    assert!(
        before["agents"]
            .as_array()
            .unwrap()
            .iter()
            .all(|agent| agent.get("heartbeat").is_none())
    );

    let body = json!({"relay": relay_report(), "agents": [
        {"token_sha256": hash(&agents[0]), "heartbeat_ms_ago": 2000, "telemetry": agent_report("srv-1")},
        // Агент другого relay и неизвестный хеш не принимаются от этого relay.
        {"token_sha256": hash(&agents[1]), "heartbeat_ms_ago": 0, "telemetry": agent_report("spoofed")},
        {"token_sha256": "ab".repeat(32), "heartbeat_ms_ago": 0, "telemetry": agent_report("unknown")},
    ]});
    for token in ["admin", "query", agents[0]["token"].as_str().unwrap()] {
        assert_eq!(
            call(&router, "POST", "/v1/heartbeat", token, body.clone())
                .await
                .0,
            401
        );
    }
    assert_eq!(
        call(&router, "POST", "/v1/heartbeat", relay_token, body)
            .await
            .0,
        204
    );
    let mut negative = relay_report();
    negative["events_per_second"] = json!(-1.0);
    let mut old = relay_report();
    old["last_ingest_ms_ago"] = json!(u64::MAX);
    let mut unnamed = agent_report("srv-1");
    unnamed["version"] = json!("");
    for bad in [
        json!({"relay": {"version": "x"}}),
        json!({"relay": negative}),
        json!({"relay": old}),
        json!({"relay": relay_report(), "agents": [{"token_sha256": hash(&agents[0]), "heartbeat_ms_ago": 0, "telemetry": unnamed}]}),
    ] {
        assert_eq!(
            call(&router, "POST", "/v1/heartbeat", relay_token, bad)
                .await
                .0,
            400
        );
    }

    let (_, relays) = call(&router, "GET", "/v1/admin/relays", "admin", nil.clone()).await;
    let relay = &relays["relays"][0]["heartbeat"];
    assert_eq!(relay["version"], "0.1.0");
    assert_eq!(relay["events_per_second"], 12.5);
    assert!((millis(&relays["as_of"]) - millis(&relay["received_at"])).abs() < 5000);
    assert_eq!(
        millis(&relay["received_at"]) - millis(&relay["last_ingest_at"]),
        500
    );
    assert!(relay["last_failure_at"].is_null());
    assert!(relays["relays"][1].get("heartbeat").is_none());
    let (_, listed) = call(&router, "GET", "/v1/admin/agents", "admin", nil.clone()).await;
    let listed = listed["agents"].as_array().unwrap();
    let own = &listed[0]["heartbeat"];
    assert_eq!(own["source"]["server"], "srv-1");
    assert_eq!(
        own["spool"],
        json!({"used_bytes": 10, "limit_bytes": 100, "records": 2})
    );
    assert_eq!(
        millis(&relay["received_at"]) - millis(&own["received_at"]),
        2000
    );
    assert_eq!(
        millis(&own["received_at"]) - millis(&own["last_record_at"]),
        4000
    );
    assert_eq!(own["delivery_failures"], 3);
    assert!(listed[1].get("heartbeat").is_none() && listed[2].get("heartbeat").is_none());

    // Более старый повтор не заменяет свежий heartbeat.
    let stale = json!({"relay": relay_report(), "agents": [
        {"token_sha256": hash(&agents[0]), "heartbeat_ms_ago": 60_000, "telemetry": agent_report("old")}]});
    assert_eq!(
        call(&router, "POST", "/v1/heartbeat", relay_token, stale)
            .await
            .0,
        204
    );
    let (_, listed) = call(&router, "GET", "/v1/admin/agents", "admin", nil.clone()).await;
    assert_eq!(
        listed["agents"][0]["heartbeat"]["source"]["server"],
        "srv-1"
    );

    let (status, state) = call(&router, "GET", "/v1/status", "query", nil.clone()).await;
    assert_eq!(status, 200);
    assert_eq!(state["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(state["ready"], true);
    assert_eq!(state["storage"]["retention_seconds"], 7 * 86400);
    let disk = &state["storage"];
    assert!(
        disk["disk_total_bytes"].as_u64().unwrap() >= disk["disk_free_bytes"].as_u64().unwrap()
    );
    assert_eq!(
        call(&router, "GET", "/v1/status", "admin", nil.clone())
            .await
            .0,
        401
    );

    // Heartbeat переживает рестарт и удаляется вместе с агентом.
    core.shutdown().unwrap();
    drop(router);
    drop(core);
    let core = Core::open(config).unwrap();
    let router = api::router(core.clone());
    let (_, listed) = call(&router, "GET", "/v1/admin/agents", "admin", nil.clone()).await;
    assert_eq!(listed["agents"][0]["heartbeat"]["version"], "0.1.0");
    let path = format!("/v1/admin/agents/{}", agents[0]["agent"]["id"]);
    assert_eq!(
        call(&router, "DELETE", &path, "admin", nil.clone()).await.0,
        204
    );
    core.shutdown().unwrap();
    let rows: i64 = rusqlite::Connection::open(dir.path().join("access.sqlite3"))
        .unwrap()
        .query_row(
            "SELECT count(*) FROM heartbeats WHERE kind='agent'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(rows, 0);
}

fn host_batch(token: &str, message: &str) -> Vec<u8> {
    use lognara_core::wire::{AgentHash, CoreBatch, Event, Group, LogId, LogLevel, Source};
    let now = lognara_core::model::now_nanos();
    let batch = CoreBatch {
        dropped: 0,
        groups: vec![Group {
            source: Source {
                environment: None,
                server: "srv-host".into(),
                backend: "host".into(),
                service: "host".into(),
                service_instance: None,
            },
            dropped: 0,
            events: vec![Event {
                id: LogId(uuid::Uuid::now_v7()),
                timestamp: now,
                ingested_at: now,
                action: None,
                level: LogLevel::Info,
                message: message.into(),
                trace_id: None,
                span_id: None,
                parent_span_id: None,
                request_id: None,
                attributes: Default::default(),
            }],
            agent: Some(AgentHash(Sha256::digest(token.as_bytes()).into())),
        }],
    };
    zstd::encode_all(&rmp_serde::to_vec_named(&batch).unwrap()[..], 3).unwrap()
}

async fn post_batch(router: &Router, token: &str, body: Vec<u8>) -> StatusCode {
    router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/batches")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/msgpack")
                .header("content-encoding", "zstd")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn host_sends_directly_and_keeps_node_identity_after_token_rotation() {
    use serde_json::json;
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

    let (status, host) = call(
        &router,
        "POST",
        "/v1/admin/hosts",
        "admin",
        json!({"name":"mail"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(host["relay"]["kind"], "host");
    let host_id = host["relay"]["id"].as_i64().unwrap();
    let agent_id = host["agent"]["id"].as_i64().unwrap();
    assert_eq!(host["agent"]["relay_id"], host_id);
    let old_token = host["token"].as_str().unwrap().to_owned();
    assert!(old_token.starts_with("lnh_"));
    assert_eq!(
        call(
            &router,
            "POST",
            "/v1/admin/hosts",
            "admin",
            json!({"name":"mail"})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );

    // Узел хоста управляется только hosts API.
    let conflict = (StatusCode::CONFLICT, json!({"error":"host_node"}));
    assert_eq!(
        call(
            &router,
            "POST",
            "/v1/admin/agents",
            "admin",
            json!({"name":"x","relay_id":host_id})
        )
        .await,
        conflict
    );
    assert_eq!(
        call(
            &router,
            "DELETE",
            &format!("/v1/admin/relays/{host_id}"),
            "admin",
            nil.clone()
        )
        .await,
        conflict
    );
    assert_eq!(
        call(
            &router,
            "DELETE",
            &format!("/v1/admin/agents/{agent_id}"),
            "admin",
            nil.clone()
        )
        .await,
        conflict
    );
    assert_eq!(
        call(
            &router,
            "POST",
            &format!("/v1/admin/agents/{agent_id}/rotate-token"),
            "admin",
            nil.clone()
        )
        .await,
        conflict
    );
    let (_, relays) = call(&router, "GET", "/v1/admin/relays", "admin", nil.clone()).await;
    assert_eq!(relays["relays"][0]["kind"], "host");

    assert!(
        post_batch(
            &router,
            &old_token,
            host_batch(&old_token, "before rotation")
        )
        .await
        .is_success()
    );
    let rotate = format!("/v1/admin/hosts/{host_id}/rotate-token");
    for token in ["", "query", old_token.as_str()] {
        assert_eq!(
            call(&router, "POST", &rotate, token, nil.clone()).await.0,
            401
        );
    }
    let (status, rotated) = call(&router, "POST", &rotate, "admin", nil.clone()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(rotated["relay"]["id"], host_id);
    assert_eq!(rotated["agent"]["id"], agent_id);
    let new_token = rotated["token"].as_str().unwrap().to_owned();
    assert_ne!(new_token, old_token);

    // Старый токен сразу отозван; пачка, подготовленная со старым хешем, принимается новым.
    assert_eq!(
        post_batch(&router, &old_token, host_batch(&old_token, "rejected")).await,
        StatusCode::UNAUTHORIZED
    );
    assert!(
        post_batch(
            &router,
            &new_token,
            host_batch(&old_token, "prepared before")
        )
        .await
        .is_success()
    );
    assert!(
        post_batch(
            &router,
            &new_token,
            host_batch(&new_token, "after rotation")
        )
        .await
        .is_success()
    );

    let from = "2000-01-01T00:00:00Z";
    let to = "2100-01-01T00:00:00Z";
    // Пачки подтверждены после WAL; поиск видит их после материализации.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while core.snapshot().watermark < 3 {
        assert!(std::time::Instant::now() < deadline && core.journal.ready());
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let (status, found) = call(
        &router,
        "POST",
        "/v1/logs/search",
        "query",
        json!({"from":from,"to":to,"filters":{"agent_id":[agent_id.to_string()]},"limit":10}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let events = found["events"].as_array().unwrap();
    let mut messages: Vec<_> = events
        .iter()
        .map(|e| e["message"].as_str().unwrap())
        .collect();
    messages.sort();
    assert_eq!(
        messages,
        ["after rotation", "before rotation", "prepared before"]
    );
    assert!(
        events
            .iter()
            .all(|e| e["relay_id"] == host_id && e["agent_id"] == agent_id)
    );

    assert_eq!(
        call(
            &router,
            "DELETE",
            &format!("/v1/admin/hosts/{host_id}"),
            "admin",
            nil.clone()
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        post_batch(&router, &new_token, host_batch(&new_token, "deleted")).await,
        StatusCode::UNAUTHORIZED
    );
    let (_, relays) = call(&router, "GET", "/v1/admin/relays", "admin", nil).await;
    assert!(relays["relays"].as_array().unwrap().is_empty());
    core.shutdown().unwrap();
}
