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
    delivered_sources: Arc<Mutex<HashSet<String>>>,
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
        if let Some(origin) = record.origin {
            state
                .delivered_sources
                .lock()
                .unwrap()
                .insert(origin.source);
            continue;
        }
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
    profile(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "run through check-memory-profile.py --sources"]
async fn bounded_source_memory_profile() {
    profile(true).await;
}

async fn profile(sources: bool) {
    let relay = Relay {
        ready: Arc::default(),
        delivered: Arc::default(),
        delivered_sources: Arc::default(),
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
    cfg.spool_dir = dir.path().join("spool");
    let marker = dir.path().join("stop");
    if sources {
        let file = dir.path().join("app.log");
        std::fs::write(&file, "event 1\n  trace\n".repeat(1_000)).unwrap();
        cfg.sources = lognara_agent::sources::Sources::parse(
            serde_json::to_string(&serde_json::json!({"sources":[{
                "id":"file", "type":"file", "include":[file],
                "multiline":{"start_pattern":"^event", "flush_timeout_ms":100}
            }]}))
            .unwrap()
            .as_bytes(),
            true,
        )
        .unwrap();
    }
    cfg.spool_max_bytes = 16 << 20;
    cfg.flush_interval = Duration::from_millis(20);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1/logs", listener.local_addr().unwrap());
    let stop = CancellationToken::new();
    let token = stop.clone();
    let stop_marker = marker.clone();
    let task = tokio::spawn(async move {
        if sources {
            let args = vec![
                "/bin/sh".into(), "-c".into(),
                "dd if=/dev/zero bs=65536 count=64 2>/dev/null; dd if=/dev/zero bs=65536 count=64 1>&2 2>/dev/null; while [ ! -f \"$1\" ]; do printf 'stream\\n'; printf 'stderr\\n' >&2; sleep 0.2; done".into(),
                "profile".into(), stop_marker.into_os_string(),
            ];
            assert_eq!(
                lognara_agent::run_program(cfg, listener, args, None).await?,
                0
            );
            Ok(())
        } else {
            lognara_agent::run(cfg, listener, token).await
        }
    });
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
    // Дождаться готовности и подтвердить HTTP-запись до конкурентной нагрузки.
    // Файловый сбор делит с HTTP один slot и может первым занять его при старте.
    if sources {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if task.is_finished() {
            panic!("agent stopped during startup: {:?}", task.await);
        }
        let mut seed = random.clone();
        seed[..8].copy_from_slice(&80u64.to_le_bytes());
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if reqwest::Client::new()
                    .post(&url)
                    .header("content-type", "application/octet-stream")
                    .body(seed.clone())
                    .send()
                    .await
                    .is_ok_and(|r| r.status() == StatusCode::ACCEPTED)
                {
                    accepted.insert(80);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("HTTP admission never became available");
    }
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
            .map(|r| r.status());
        match status {
            Ok(StatusCode::ACCEPTED) => {
                accepted.insert(id);
            }
            Ok(StatusCode::SERVICE_UNAVAILABLE) => rejected += 1,
            Err(_) => transport_errors += 1,
            other => panic!("unexpected response {other:?}"),
        }
    }
    // 30 секунд полной очереди: несколько циклов максимального backoff,
    // повторные неподтверждённые запросы не вытесняют подтверждённые данные.
    for round in 0..30u64 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let mut body = random.clone();
        let id = 100 + round;
        body[..8].copy_from_slice(&id.to_le_bytes());
        let response = reqwest::Client::new()
            .post(&url)
            .header("content-type", "application/octet-stream")
            .body(body)
            .send()
            .await;
        match response {
            Ok(response) if sources && response.status() == StatusCode::ACCEPTED => {
                accepted.insert(id);
            }
            Ok(response) => assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE),
            Err(_) => transport_errors += 1,
        }
        assert!(relay.delivered.lock().unwrap().is_empty());
    }
    relay.ready.store(true, Ordering::Release);
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let all_sources = !sources
                || ["file", "stdout", "stderr"]
                    .iter()
                    .all(|id| relay.delivered_sources.lock().unwrap().contains(*id));
            if accepted.is_subset(&relay.delivered.lock().unwrap()) && all_sources {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("confirmed logs missing after recovery");
    if sources {
        std::fs::write(&marker, "stop").unwrap();
    } else {
        stop.cancel();
    }
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
