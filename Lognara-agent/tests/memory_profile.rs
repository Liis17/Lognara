//! Отдельный процесс нагрузки: конкурентные запросы, недоступность и восстановление relay.
use axum::{
    Router,
    body::Bytes,
    extract::{DefaultBodyLimit, State},
    http::StatusCode,
    routing::post,
};
use lognara_agent::{
    config::Config,
    wire::{Batch, Payload},
};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
struct Relay {
    ready: Arc<AtomicBool>,
    delivered: Arc<Mutex<HashSet<u64>>>,
}
async fn receive(State(state): State<Relay>, body: Bytes) -> StatusCode {
    if !state.ready.load(Ordering::Acquire) {
        return StatusCode::SERVICE_UNAVAILABLE;
    }
    assert!(body.len() <= 8 << 20);
    let raw = zstd::decode_all(&body[..]).unwrap();
    assert!(raw.len() <= 8 << 20);
    let batch: Batch = rmp_serde::from_slice(&raw).unwrap();
    for record in batch.records {
        let Payload::Binary(bytes) = record.payload else {
            panic!("unexpected payload");
        };
        state
            .delivered
            .lock()
            .unwrap()
            .insert(u64::from_le_bytes(bytes[..8].try_into().unwrap()));
    }
    StatusCode::OK
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "run through check-memory-profile.py"]
async fn bounded_agent_memory_profile() {
    let relay = Relay {
        ready: Arc::default(),
        delivered: Arc::default(),
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let relay_url = format!("http://{}/v1/batches", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/v1/batches", post(receive))
        .layer(DefaultBodyLimit::max(8 << 20))
        .with_state(relay.clone());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config::from_lookup(|key| match key {
        "LOGNARA_SERVICE" => Some("api".into()),
        "LOGNARA_SERVER" => Some("server".into()),
        "LOGNARA_BACKEND" => Some("backend".into()),
        "LOGNARA_RELAY_TOKEN" => Some("secret".into()),
        "LOGNARA_RELAY_URL" => Some(relay_url.clone()),
        _ => None,
    })
    .unwrap();
    cfg.spool_dir = dir.path().into();
    cfg.spool_max_bytes = 16 << 20;
    cfg.flush_interval = Duration::from_millis(20);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1/logs", listener.local_addr().unwrap());
    let stop = CancellationToken::new();
    let task = tokio::spawn(lognara_agent::run(cfg, listener, stop.clone()));
    let mut random = vec![0u8; 2 << 20];
    let mut generator = 1u64;
    for byte in &mut random {
        generator ^= generator << 13;
        generator ^= generator >> 7;
        generator ^= generator << 17;
        *byte = generator as u8;
    }
    let mut accepted = HashSet::new();
    let mut rejected = 0;
    let mut transport_errors = 0;
    for round in 0..4u64 {
        let mut clients = Vec::new();
        for index in 0..16u64 {
            let id = 16 * round + index;
            let mut body = random.clone();
            body[..8].copy_from_slice(&id.to_le_bytes());
            let url = url.clone();
            clients.push(tokio::spawn(async move {
                let status = reqwest::Client::new()
                    .post(url)
                    .header("content-type", "application/octet-stream")
                    .body(body)
                    .send()
                    .await
                    .map(|response| response.status());
                (id, status)
            }));
        }
        for client in clients {
            let (id, status) = client.await.unwrap();
            match status {
                Ok(StatusCode::ACCEPTED) => {
                    accepted.insert(id);
                }
                Ok(StatusCode::SERVICE_UNAVAILABLE) => rejected += 1,
                Err(_) => transport_errors += 1,
                _ => panic!("unexpected status {status:?}"),
            }
        }
    }
    assert!(!accepted.is_empty());
    assert!(rejected + transport_errors > 0);
    // После насыщения одной записью на запрос добираем дисковую квоту без вытеснения.
    for id in 64..80u64 {
        let mut body = random.clone();
        body[..8].copy_from_slice(&id.to_le_bytes());
        let status = reqwest::Client::new()
            .post(&url)
            .header("content-type", "application/octet-stream")
            .body(body)
            .send()
            .await
            .unwrap()
            .status();
        if status == StatusCode::ACCEPTED {
            accepted.insert(id);
        } else {
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        }
    }
    // 30 секунд полной очереди: несколько циклов максимального backoff,
    // повторные неподтверждённые запросы не вытесняют подтверждённые данные.
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let response = reqwest::Client::new()
            .post(&url)
            .header("content-type", "application/octet-stream")
            .body(random.clone())
            .send()
            .await;
        match response {
            Ok(response) => assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE),
            Err(_) => transport_errors += 1,
        }
        assert!(relay.delivered.lock().unwrap().is_empty());
    }
    relay.ready.store(true, Ordering::Release);
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if accepted.is_subset(&relay.delivered.lock().unwrap()) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("confirmed logs missing after recovery");
    stop.cancel();
    tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    println!(
        "accepted={}, admission_rejections={rejected}, transport_errors={transport_errors}; all confirmed records delivered",
        accepted.len()
    );
}
