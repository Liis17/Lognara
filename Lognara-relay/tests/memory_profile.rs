//! Воспроизводимая нагрузка для внешнего контроля RSS; обычный cargo test её пропускает.

use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::http::StatusCode;
use axum::routing::post;
use lognara_relay::agent_wire::{Batch, Payload, Record};
use lognara_relay::config::Config;
use lognara_relay::spool::Spool;
use reqwest::Client;
use tokio::net::TcpListener;
use tokio::task::JoinSet;
use tokio::time::{sleep, timeout};
use tokio_util::sync::CancellationToken;

const MIB: usize = 1 << 20;

fn batch_body(payloads: Vec<Payload>) -> Bytes {
    let batch = Batch {
        service: "memory-profile".into(),
        server: "local".into(),
        backend: "test".into(),
        environment: None,
        service_instance: None,
        sent_at: 1,
        dropped: 0,
        records: payloads
            .into_iter()
            .map(|payload| Record {
                received_at: 1,
                payload,
            })
            .collect(),
    };
    zstd::encode_all(&rmp_serde::to_vec_named(&batch).unwrap()[..], 3)
        .unwrap()
        .into()
}
fn body(payload: Payload) -> Bytes {
    batch_body(vec![payload])
}

fn incompressible(length: usize) -> Vec<u8> {
    let mut seed = 1u64;
    let mut bytes = Vec::with_capacity(length);
    while bytes.len() < length {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        bytes.extend_from_slice(&seed.to_le_bytes());
    }
    bytes.truncate(length);
    bytes
}

async fn send(client: &Client, url: &str, body: Bytes) -> StatusCode {
    client
        .post(url)
        .header("authorization", "Bearer relay-secret")
        .header("content-type", "application/msgpack")
        .header("content-encoding", "zstd")
        .body(body)
        .send()
        .await
        .unwrap()
        .status()
}

async fn eventually(client: &Client, url: &str, body: Bytes, expected: StatusCode) {
    timeout(Duration::from_secs(30), async {
        loop {
            let status = send(client, url, body.clone()).await;
            if status == expected {
                return;
            }
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("relay did not finish the preceding bounded work");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "run with tests/check-memory-profile.py to enforce and report the RSS threshold"]
async fn bounded_relay_memory_profile() {
    // Производитель фикстур тоже входит в измеряемый процесс. Сохраняются только
    // сжатые тела; исходные строки и MessagePack освобождаются до старта relay.
    let text = batch_body(
        (0..48)
            .map(|_| Payload::Text("x".repeat(2 * MIB)))
            .collect(),
    );
    let large_body = batch_body(
        (0..30)
            .map(|_| Payload::Binary(incompressible(2 * MIB)))
            .collect(),
    );
    assert!((59 * MIB..64 * MIB).contains(&large_body.len()));
    let near_model_limit = body(Payload::Text("x".repeat(256 * MIB - 65536)));
    let binary = body(Payload::Binary(vec![0; 120 * MIB]));
    let json = body(Payload::Json(format!(
        "{{\"items\":[{}null]}}",
        "null,".repeat(1 << 20)
    )));
    let bomb = body(Payload::Text("x".repeat(256 * MIB + 1)));

    // Недоступный core не хранит копии тел и не раздувает измерение тестовыми данными.
    let core_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let core_url = format!("http://{}/v1/batches", core_listener.local_addr().unwrap());
    let core = tokio::spawn(async move {
        let app = Router::new().route(
            "/v1/batches",
            post(|| async { StatusCode::SERVICE_UNAVAILABLE }),
        );
        axum::serve(core_listener, app).await.unwrap();
    });
    let dir = tempfile::tempdir().unwrap();
    let config = Config::from_lookup(|name| match name {
        "LOGNARA_CORE_URL" => Some(core_url.clone()),
        "LOGNARA_CORE_TOKEN" => Some("core-secret".into()),
        "LOGNARA_RELAY_TOKEN" => Some("relay-secret".into()),
        "LOGNARA_FLUSH_INTERVAL_MS" => Some("20".into()),
        _ => None,
    })
    .unwrap();
    assert_eq!(config.memory_bytes, 1792 * MIB);
    assert_eq!(config.max_ingest_concurrency, 1);
    let spool = Spool::open(dir.path(), 1024 * MIB as u64).await.unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1/batches", listener.local_addr().unwrap());
    let shutdown = CancellationToken::new();
    let relay = tokio::spawn(lognara_relay::run(
        config,
        listener,
        spool,
        shutdown.clone(),
    ));
    let client = Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap();

    let mut accepted = 0;
    let mut busy = 0;
    for _ in 0..4 {
        eventually(&client, &url, text.clone(), StatusCode::ACCEPTED).await;
        accepted += 1;
        let mut requests = JoinSet::new();
        for _ in 0..16 {
            let client = client.clone();
            let url = url.clone();
            let body = text.clone();
            requests.spawn(async move { send(&client, &url, body).await });
        }
        while let Some(status) = requests.join_next().await {
            match status.unwrap() {
                StatusCode::ACCEPTED => accepted += 1,
                StatusCode::SERVICE_UNAVAILABLE => busy += 1,
                status => panic!("unexpected status for a bounded text batch: {status}"),
            }
        }
    }
    assert!(busy > 0, "concurrent requests did not exercise admission");
    // Большое сжатое тело упражняет чтение 64 MiB, Binary — base64 и отправку
    // крупного результата. Большая Text проходит модель, но не байтовый буфер.
    eventually(&client, &url, large_body, StatusCode::ACCEPTED).await;
    accepted += 1;
    for body in [json, binary, bomb, near_model_limit] {
        eventually(&client, &url, body, StatusCode::PAYLOAD_TOO_LARGE).await;
    }

    shutdown.cancel();
    timeout(Duration::from_secs(10), relay)
        .await
        .expect("relay did not stop with core unavailable")
        .unwrap()
        .unwrap();
    assert!(std::fs::read_dir(dir.path()).unwrap().next().is_some());
    core.abort();
    println!(
        "memory profile: accepted={accepted}, admission_rejections={busy}, resource_rejections=4, compressed_body=60MiB, core=503, shutdown=ok"
    );
}
