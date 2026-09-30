//! Subprocess tests stop the real binary between durability boundaries.
#![cfg(feature = "crash-tests")]
use reqwest::Client;
use std::{
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

struct Process {
    child: Child,
    url: String,
}

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

async fn start(path: &std::path::Path, point: Option<&str>) -> Process {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut command = Command::new(env!("CARGO_BIN_EXE_lognara-core"));
    command
        .env("LOGNARA_DATA_DIR", path)
        .env("LOGNARA_LISTEN_ADDR", format!("127.0.0.1:{port}"))
        .env("LOGNARA_INGEST_TOKEN", "ingest")
        .env("LOGNARA_QUERY_TOKEN", "query")
        .env("LOGNARA_SEGMENT_ROWS", "2")
        .env("LOGNARA_REFRESH_MS", "20")
        .env("LOGNARA_DISK_RESERVE_BYTES", "1")
        .env("RUST_LOG", "error")
        .env_remove("LOGNARA_CRASH_AT")
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    if let Some(point) = point {
        command.env("LOGNARA_CRASH_AT", point);
        if point == "retention_after_mark" {
            command.env("LOGNARA_RETENTION_SECONDS", "1");
        }
    }
    let mut process = Process {
        child: command.spawn().unwrap(),
        url: format!("http://127.0.0.1:{port}"),
    };
    let client = Client::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        assert!(
            process.child.try_wait().unwrap().is_none(),
            "core exited during startup"
        );
        if let Ok(response) = client
            .get(format!("{}/health/ready", process.url))
            .send()
            .await
            && response.status().is_success()
        {
            break;
        }
        assert!(Instant::now() < deadline, "core did not become ready");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    process
}

fn batch() -> Vec<u8> {
    let mut batch =
        lognara_core::wire::decode(include_bytes!("fixtures/relay-batch.bin"), 1 << 20).unwrap();
    let event = batch.groups[0].events[0].clone();
    batch.groups[0].events = (0..5)
        .map(|i| {
            let mut event = event.clone();
            event.message = format!("crash row {i}");
            event
        })
        .collect();
    zstd::encode_all(&rmp_serde::to_vec_named(&batch).unwrap()[..], 3).unwrap()
}

async fn send(client: &Client, url: &str, body: &[u8]) -> reqwest::Result<reqwest::Response> {
    client
        .post(format!("{url}/v1/batches"))
        .bearer_auth("ingest")
        .header("content-type", "application/msgpack")
        .header("content-encoding", "zstd")
        .body(body.to_vec())
        .send()
        .await
}

async fn count(client: &Client, url: &str) -> usize {
    client
        .post(format!("{url}/v1/logs/search"))
        .bearer_auth("query")
        .json(&serde_json::json!({"from":"2026-01-01T00:00:00Z","to":"2027-01-01T00:00:00Z"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap()["events"]
        .as_array()
        .unwrap()
        .len()
}

#[tokio::test]
async fn crashes_at_each_publication_boundary_recover_without_loss_or_duplicates() {
    let client = Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let body = batch();
    for point in [
        "wal_before_sync",
        "wal_before_rename",
        "wal_after_sync",
        "segment_during_parquet",
        "segment_after_parquet",
        "segment_before_catalog",
        "segment_after_catalog",
        "wal_after_prune",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let mut process = start(dir.path(), Some(point)).await;
        let _ = send(&client, &process.url, &body).await;
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(status) = process.child.try_wait().unwrap() {
                assert_eq!(status.code(), Some(86), "{point}");
                break;
            }
            assert!(
                Instant::now() < deadline,
                "crash point {point} was not reached"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        drop(process);
        let process = start(dir.path(), None).await;
        assert_eq!(
            count(&client, &process.url).await,
            if matches!(point, "wal_before_sync" | "wal_before_rename") {
                0
            } else {
                5
            },
            "{point}"
        );
        assert_eq!(
            send(&client, &process.url, &body).await.unwrap().status(),
            204
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while count(&client, &process.url).await != 5 {
            assert!(
                Instant::now() < deadline,
                "replayed batch never became visible"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(
            send(&client, &process.url, &body).await.unwrap().status(),
            204
        );
        assert_eq!(count(&client, &process.url).await, 5);
    }
}

#[tokio::test]
async fn restart_finishes_marked_retention_without_reaccepting_deleted_batch() {
    let dir = tempfile::tempdir().unwrap();
    let client = Client::new();
    let mut decoded = lognara_core::wire::decode(&batch(), 1 << 20).unwrap();
    // Две полные секции, без открытого остатка на момент удаления.
    decoded.groups[0].events.truncate(4);
    let body = zstd::encode_all(&rmp_serde::to_vec_named(&decoded).unwrap()[..], 3).unwrap();
    let mut process = start(dir.path(), Some("retention_after_mark")).await;
    assert_eq!(
        send(&client, &process.url, &body).await.unwrap().status(),
        204
    );
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(status) = process.child.try_wait().unwrap() {
            assert_eq!(status.code(), Some(86));
            break;
        }
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    drop(process);
    let process = start(dir.path(), None).await;
    assert_eq!(count(&client, &process.url).await, 0);
    assert_eq!(
        send(&client, &process.url, &body).await.unwrap().status(),
        204
    );
    assert_eq!(count(&client, &process.url).await, 0);
}

#[tokio::test]
async fn sigkill_after_successful_ack_preserves_the_batch() {
    let dir = tempfile::tempdir().unwrap();
    let client = Client::new();
    let body = batch();
    let mut process = start(dir.path(), None).await;
    assert_eq!(
        send(&client, &process.url, &body).await.unwrap().status(),
        204
    );
    process.child.kill().unwrap();
    process.child.wait().unwrap();
    drop(process);
    let process = start(dir.path(), None).await;
    assert_eq!(count(&client, &process.url).await, 5);
    assert_eq!(
        send(&client, &process.url, &body).await.unwrap().status(),
        204
    );
    assert_eq!(count(&client, &process.url).await, 5);
}
