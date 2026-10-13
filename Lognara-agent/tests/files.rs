use std::fs::{self, OpenOptions};
use std::io::Write;
use std::time::Duration;

use axum::{Router, body::Bytes, routing::post};
use lognara_agent::{
    config::Config,
    sources::Sources,
    wire::{Batch, Payload},
};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

async fn setup(dir: &std::path::Path, include: &str) -> (Config, mpsc::UnboundedReceiver<Batch>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1/batches", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route(
                "/v1/batches",
                post(move |body: Bytes| {
                    let tx = tx.clone();
                    async move {
                        let batch =
                            rmp_serde::from_slice(&zstd::decode_all(&body[..]).unwrap()).unwrap();
                        tx.send(batch).unwrap();
                        axum::http::StatusCode::ACCEPTED
                    }
                }),
            ),
        )
        .await
        .unwrap();
    });
    let mut config = Config::from_lookup(|key| match key {
        "LOGNARA_SERVICE" | "LOGNARA_SERVER" | "LOGNARA_BACKEND" | "LOGNARA_RELAY_TOKEN" => {
            Some("test".into())
        }
        "LOGNARA_RELAY_URL" => Some(url.clone()),
        "LOGNARA_FLUSH_INTERVAL_MS" => Some("20".into()),
        _ => None,
    })
    .unwrap();
    config.spool_dir = dir.join("spool");
    config.sources = Sources::parse(
        serde_json::to_string(
            &serde_json::json!({"sources":[{"id":"app","type":"file","include":[include]}]}),
        )
        .unwrap()
        .as_bytes(),
        false,
    )
    .unwrap();
    (config, rx)
}

async fn start(
    config: Config,
) -> (
    CancellationToken,
    tokio::task::JoinHandle<std::io::Result<()>>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let stop = CancellationToken::new();
    let token = stop.clone();
    (
        stop,
        tokio::spawn(lognara_agent::run(config, listener, token)),
    )
}

async fn next(rx: &mut mpsc::UnboundedReceiver<Batch>) -> Batch {
    tokio::time::timeout(Duration::from_secs(8), rx.recv())
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn resumes_files_and_finds_renamed_files_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.log");
    fs::write(&path, "first\n").unwrap();
    let (config, mut rx) = setup(dir.path(), path.to_str().unwrap()).await;
    let (stop, task) = start(config.clone()).await;
    let batch = next(&mut rx).await;
    assert_eq!(batch.records[0].payload, Payload::Text("first".into()));
    assert_eq!(batch.records[0].origin.as_ref().unwrap().offset, 0);
    stop.cancel();
    task.await.unwrap().unwrap();
    OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"old tail\n")
        .unwrap();
    fs::rename(&path, dir.path().join("app.log.1")).unwrap();
    fs::write(&path, "new file\n").unwrap();
    let (stop, task) = start(config).await;
    let mut messages = Vec::new();
    while messages.len() < 2 {
        for record in next(&mut rx).await.records {
            let Payload::Text(text) = record.payload else {
                panic!("text expected")
            };
            messages.push(text);
        }
    }
    assert_eq!(messages, ["old tail", "new file"]);
    stop.cancel();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn corrupt_checkpoint_fails_before_http_service_starts() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.log");
    let (config, _rx) = setup(dir.path(), path.to_str().unwrap()).await;
    fs::create_dir_all(&config.spool_dir).unwrap();
    fs::write(config.spool_dir.join("sources.checkpoint"), "broken").unwrap();
    let (_stop, task) = start(config).await;
    assert!(task.await.unwrap().is_err());
}

#[tokio::test]
async fn waits_for_a_complete_line_then_recovers_from_truncation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.log");
    fs::write(&path, "hello").unwrap();
    let (config, mut rx) = setup(dir.path(), path.to_str().unwrap()).await;
    let (stop, task) = start(config).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(300), rx.recv())
            .await
            .is_err()
    );
    OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"\n")
        .unwrap();
    assert_eq!(
        next(&mut rx).await.records[0].payload,
        Payload::Text("hello".into())
    );
    fs::write(&path, "short\n").unwrap();
    let record = next(&mut rx).await.records.remove(0);
    assert_eq!(record.payload, Payload::Text("short".into()));
    assert_eq!(record.origin.unwrap().offset, 0);
    stop.cancel();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn ignores_special_files_without_blocking_other_files_or_shutdown() {
    let dir = tempfile::tempdir().unwrap();
    let fifo = std::ffi::CString::new(dir.path().join("fifo.log").to_str().unwrap()).unwrap();
    // SAFETY: valid NUL-terminated path; mkfifo does not retain the pointer.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    std::os::unix::fs::symlink("/dev/stdout", dir.path().join("console.log")).unwrap();
    fs::write(dir.path().join("app.log"), "regular\n").unwrap();
    let (config, mut rx) = setup(dir.path(), dir.path().join("*.log").to_str().unwrap()).await;
    let (stop, task) = start(config).await;
    assert_eq!(
        next(&mut rx).await.records[0].payload,
        Payload::Text("regular".into())
    );
    stop.cancel();
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn resumes_a_fragmented_line_after_restart_without_losing_its_tail() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.log");
    fs::write(&path, "abcdefghijklmno").unwrap();
    let (mut config, mut rx) = setup(dir.path(), path.to_str().unwrap()).await;
    config.max_record_bytes = 8;
    let (stop, task) = start(config.clone()).await;
    let first = next(&mut rx).await.records.remove(0);
    assert_eq!(first.payload, Payload::Text("abcdefgh".into()));
    assert!(first.origin.unwrap().continued);
    stop.cancel();
    task.await.unwrap().unwrap();
    OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"\n")
        .unwrap();
    let (stop, task) = start(config).await;
    let last = next(&mut rx).await.records.remove(0);
    assert_eq!(last.payload, Payload::Text("ijklmno".into()));
    let origin = last.origin.unwrap();
    assert_eq!(origin.offset, 8);
    assert_eq!(origin.fragment, 1);
    assert!(!origin.continued);
    stop.cancel();
    task.await.unwrap().unwrap();
}
