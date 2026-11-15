//! Файл хоста → durable spool → нормализация → фейковый core.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::{Router, body::Bytes, extract::State, http::HeaderMap, http::StatusCode, routing::post};
use lognara_hwagent::config::{Config, HostFile};
use lognara_relay::core_wire::CoreBatch;
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct Fake {
    bodies: Vec<Bytes>,
    heartbeats: Vec<serde_json::Value>,
    reject_first: bool,
}

type Shared = Arc<Mutex<Fake>>;

async fn batches(State(fake): State<Shared>, headers: HeaderMap, body: Bytes) -> StatusCode {
    assert_eq!(headers["authorization"], "Bearer lnh_test");
    assert_eq!(headers["content-type"], "application/msgpack");
    assert_eq!(headers["content-encoding"], "zstd");
    let mut fake = fake.lock().unwrap();
    fake.bodies.push(body);
    if std::mem::take(&mut fake.reject_first) {
        return StatusCode::SERVICE_UNAVAILABLE;
    }
    StatusCode::ACCEPTED
}

async fn heartbeat(State(fake): State<Shared>, body: Bytes) -> StatusCode {
    let value = serde_json::from_slice(&body).unwrap();
    fake.lock().unwrap().heartbeats.push(value);
    StatusCode::NO_CONTENT
}

async fn wait(fake: &Shared, done: impl Fn(&Fake) -> bool) {
    for _ in 0..200 {
        if done(&fake.lock().unwrap()) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("condition was not reached");
}

#[tokio::test(flavor = "multi_thread")]
async fn file_lines_reach_core_with_host_identity_and_exact_retries() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("auth.log");
    std::fs::write(
        &log,
        "Accepted publickey for root\nsudo: admin : COMMAND=/bin/sh\n",
    )
    .unwrap();
    let fake: Shared = Arc::new(Mutex::new(Fake {
        reject_first: true,
        ..Fake::default()
    }));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/v1/batches", post(batches))
        .route("/v1/heartbeat", post(heartbeat))
        .with_state(fake.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let host = HostFile::parse(
        format!(
            r#"{{"sources":[{{"id":"auth","type":"file","include":["{}"]}}],"metrics":{{"enabled":false}}}}"#,
            log.display()
        )
        .as_bytes(),
    )
    .unwrap();
    let url = format!("http://{address}/v1/batches");
    let spool = dir.path().join("spool").display().to_string();
    let config = Config::from_lookup(
        |name| match name {
            "LOGNARA_CORE_URL" => Some(url.clone()),
            "LOGNARA_HOST_TOKEN" => Some("lnh_test".into()),
            "LOGNARA_SERVER" => Some("mail".into()),
            "LOGNARA_SPOOL_DIR" => Some(spool.clone()),
            "LOGNARA_FLUSH_INTERVAL_MS" => Some("50".into()),
            "LOGNARA_HEARTBEAT_INTERVAL_MS" => Some("100".into()),
            _ => None,
        },
        host,
    )
    .unwrap();
    let shutdown = CancellationToken::new();
    let task = tokio::spawn(lognara_hwagent::run(config, shutdown.clone()));

    wait(&fake, |fake| {
        fake.bodies.len() >= 2 && !fake.heartbeats.is_empty()
    })
    .await;
    shutdown.cancel();
    task.await.unwrap().unwrap();

    let fake = fake.lock().unwrap();
    // Отклонённая пачка повторена теми же байтами: core дедуплицирует её по SHA-256.
    assert_eq!(fake.bodies[0], fake.bodies[1]);
    let batch: CoreBatch =
        rmp_serde::from_slice(&zstd::decode_all(&fake.bodies[1][..]).unwrap()).unwrap();
    let group = &batch.groups[0];
    let hash: [u8; 32] = Sha256::digest(b"lnh_test").into();
    assert_eq!(group.agent.unwrap().0, hash);
    assert_eq!(group.source.service, "host");
    assert_eq!(group.source.server, "mail");
    let messages: Vec<_> = group
        .events
        .iter()
        .map(|event| event.message.as_str())
        .collect();
    assert_eq!(
        messages,
        [
            "Accepted publickey for root",
            "sudo: admin : COMMAND=/bin/sh"
        ]
    );
    assert_eq!(group.events[0].attributes["lognara.source"], "auth");

    let report = &fake.heartbeats[0];
    let token_hash: String = hash.iter().map(|byte| format!("{byte:02x}")).collect();
    assert_eq!(report["agents"][0]["token_sha256"], token_hash);
    assert_eq!(report["agents"][0]["telemetry"]["source"]["server"], "mail");
}
