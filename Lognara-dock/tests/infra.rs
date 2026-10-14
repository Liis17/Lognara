//! Инфраструктура: статусы узлов по heartbeat в часах core, core и хранилище.

mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::State;
use axum::http::{Method, StatusCode, Uri};
use common::*;
use serde_json::{Value, json};

type Replies = Arc<Mutex<HashMap<String, (StatusCode, Value)>>>;

async fn reply(State(replies): State<Replies>, uri: Uri) -> (StatusCode, String) {
    let replies = replies.lock().unwrap();
    let (status, body) = replies
        .get(uri.path())
        .cloned()
        .unwrap_or((StatusCode::NOT_FOUND, Value::Null));
    (status, body.to_string())
}

const NOW: &str = "2026-10-15T12:00:00Z";

/// Время за `seconds` секунд до NOW.
fn ago(seconds: i64) -> String {
    let now =
        time::OffsetDateTime::parse(NOW, &time::format_description::well_known::Rfc3339).unwrap();
    (now - time::Duration::seconds(seconds))
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap()
}

fn spool(used: u64) -> Value {
    json!({"used_bytes": used, "limit_bytes": 100, "records": used})
}

fn relay(id: i64, name: &str, heartbeat: Option<i64>) -> Value {
    let mut relay = json!({"id": id, "name": name, "created_at": NOW});
    if let Some(age) = heartbeat {
        relay["heartbeat"] = json!({
            "received_at": ago(age), "version": "0.2.0", "spool": spool(10),
            "last_ingest_at": ago(age + 1), "last_delivery_at": ago(age), "last_failure_at": null,
            "delivery_failures": 0, "events_per_second": 3.5
        });
    }
    relay
}

type HeartbeatSample = (i64, i64, u64, (i64, Option<i64>));

/// heartbeat: (возраст, последнее событие, spool, (последняя доставка, последняя ошибка)).
fn agent(id: i64, name: &str, relay_id: i64, heartbeat: Option<HeartbeatSample>) -> Value {
    let mut agent = json!({"id": id, "name": name, "relay_id": relay_id, "created_at": NOW});
    if let Some((age, record, used, (delivery, failure))) = heartbeat {
        agent["heartbeat"] = json!({
            "received_at": ago(age), "version": "0.1.0",
            "source": {"environment": "prod", "server": name, "backend": "api", "service": "billing", "service_instance": null},
            "spool": spool(used), "last_record_at": ago(record), "last_delivery_at": ago(delivery),
            "last_failure_at": failure.map(ago), "delivery_failures": failure.map_or(0, |_| 2)
        });
    }
    agent
}

fn status() -> Value {
    json!({
        "version": "0.3.0", "as_of": NOW, "ready": true, "visible_sequence": 10, "visibility_lag_ms": 600,
        "wal_bytes": 2048, "wal_limit_bytes": 4096,
        "storage": {"segments": 2, "parquet_bytes": 1000, "index_bytes": 100, "oldest_event_at": ago(86400),
            "retention_seconds": 604800, "disk_total_bytes": 10000, "disk_free_bytes": 4000}
    })
}

#[tokio::test]
async fn nodes_get_distinct_states_from_heartbeat_age_spool_and_delivery() {
    let replies: Replies = Arc::default();
    let router = Router::new().fallback(reply).with_state(replies.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let t = start(&[("liis", "password")], &[("LOGNARA_DOCK_CORE_URL", &url)]);
    let session = login(&t, "liis", "password").await.session();
    let set = |path: &str, status: StatusCode, body: Value| {
        replies.lock().unwrap().insert(path.into(), (status, body));
    };
    set("/v1/status", StatusCode::OK, status());
    set(
        "/v1/admin/relays",
        StatusCode::OK,
        json!({"as_of": NOW, "relays": [
            relay(1, "relay-eu-1", Some(5)), relay(2, "relay-eu-2", Some(120)), relay(3, "relay-new", None),
        ]}),
    );
    set(
        "/v1/admin/agents",
        StatusCode::OK,
        json!({"as_of": NOW, "agents": [
            agent(1, "online", 1, Some((3, 2, 4, (2, None)))),
            agent(2, "idle", 1, Some((3, 600, 0, (600, None)))),
            agent(3, "spool", 1, Some((3, 2, 85, (2, None)))),
            agent(4, "failing", 1, Some((3, 2, 4, (30, Some(5))))),
            agent(5, "recovered", 1, Some((3, 2, 4, (5, Some(30))))),
            agent(6, "offline", 1, Some((90, 95, 4, (95, None)))),
            agent(7, "silent", 1, None),
            agent(8, "behind-offline-relay", 2, Some((130, 130, 4, (130, None)))),
        ]}),
    );
    let get = || {
        send(
            &t.router,
            request(Method::GET, "/api/infrastructure", Some(&session), None),
        )
    };
    let reply = get().await;
    assert_eq!(reply.status, StatusCode::OK);
    let body = reply.body;
    let states: HashMap<String, (Value, Value)> = body["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|node| {
            (
                format!(
                    "{} {}",
                    node["kind"].as_str().unwrap(),
                    node["name"].as_str().unwrap()
                ),
                (node["status"].clone(), node["reason"].clone()),
            )
        })
        .collect();
    let expect = |name: &str, status: &str, reason: Value| {
        assert_eq!(states[name], (json!(status), reason), "{name}");
    };
    expect("relay relay-eu-1", "online", Value::Null);
    expect("relay relay-eu-2", "offline", Value::Null);
    expect("relay relay-new", "unknown", json!("no_heartbeat"));
    expect("agent online", "online", Value::Null);
    expect("agent idle", "idle", Value::Null);
    expect("agent spool", "degraded", json!("spool"));
    expect("agent failing", "degraded", json!("delivery"));
    expect("agent recovered", "online", Value::Null);
    expect("agent offline", "offline", Value::Null);
    expect("agent silent", "unknown", json!("no_heartbeat"));
    expect(
        "agent behind-offline-relay",
        "unknown",
        json!("relay_silent"),
    );
    let order: Vec<_> = body["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|node| node["status"].as_str().unwrap())
        .collect();
    let mut sorted = order.clone();
    let rank = |status: &str| {
        ["offline", "degraded", "unknown", "idle", "online"]
            .iter()
            .position(|s| *s == status)
    };
    sorted.sort_by_key(|status| rank(status));
    assert_eq!(order, sorted, "problem nodes first");
    let online = body["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["name"] == "online")
        .unwrap();
    assert_eq!(online["relay"], "relay-eu-1");
    assert_eq!(online["source"]["server"], "online");
    assert_eq!(online["heartbeat_at"], ago(3));
    assert_eq!(body["as_of"], NOW);
    assert_eq!(body["registry"], true);
    assert_eq!(
        body["core"],
        json!({"status": "online", "version": "0.3.0", "visibility_lag_ms": 600, "wal_bytes": 2048, "wal_limit_bytes": 4096})
    );
    assert_eq!(body["storage"]["parquet_bytes"], 1000);
    assert_eq!(body["dock"]["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(body["dock"]["sessions"], 1);
    assert_eq!(
        body["thresholds"],
        json!({"heartbeat_ttl_seconds": 60, "idle_seconds": 300, "spool_warn_percent": 80})
    );

    // Core не готов: статус отдельно от недоступности, узлы остаются.
    let mut not_ready = status();
    not_ready["ready"] = json!(false);
    set("/v1/status", StatusCode::OK, not_ready);
    assert_eq!(get().await.body["core"]["status"], "not_ready");

    // Недоступный core: нет ни узлов, ни хранилища, но ответ есть.
    replies.lock().unwrap().clear();
    let body = get().await.body;
    assert_eq!(body["core"]["status"], "unavailable");
    assert_eq!(body["registry"], false);
    assert_eq!(body["nodes"], json!([]));
    assert!(body["storage"].is_null());

    let anonymous = send(
        &t.router,
        request(Method::GET, "/api/infrastructure", None, None),
    )
    .await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
}
