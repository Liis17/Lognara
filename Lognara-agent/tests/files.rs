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

#[tokio::test]
async fn all_65_growing_files_get_a_turn_despite_the_64_descriptor_limit() {
    let dir = tempfile::tempdir().unwrap();
    let paths: Vec<_> = (0..65)
        .map(|n| dir.path().join(format!("{n:03}.log")))
        .collect();
    for path in &paths {
        fs::write(path, "first\n").unwrap();
    }
    let (config, mut rx) = setup(dir.path(), dir.path().join("*.log").to_str().unwrap()).await;
    let (stop, task) = start(config).await;
    let growing = stop.clone();
    let producer = tokio::spawn(async move {
        while !growing.is_cancelled() {
            for path in &paths {
                OpenOptions::new()
                    .append(true)
                    .open(path)
                    .unwrap()
                    .write_all(b"next\n")
                    .unwrap();
            }
            tokio::time::sleep(Duration::from_millis(400)).await;
        }
    });
    let mut observed = std::collections::HashSet::new();
    let completed = tokio::time::timeout(Duration::from_secs(25), async {
        while observed.len() < 65 {
            for record in rx.recv().await.unwrap().records {
                observed.insert(record.origin.unwrap().path.unwrap());
            }
        }
    })
    .await;
    stop.cancel();
    producer.await.unwrap();
    task.await.unwrap().unwrap();
    assert!(
        completed.is_ok(),
        "only {} of 65 growing files collected",
        observed.len()
    );
}

#[tokio::test]
async fn checkpoint_failure_pauses_without_duplicates_and_sigkill_replays_saved_data() {
    use std::process::Stdio;
    use tokio::process::Command;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.log");
    fs::write(&path, "first\n").unwrap();
    let (config, mut rx) = setup(dir.path(), path.to_str().unwrap()).await;
    let settings = dir.path().join("sources.json");
    fs::write(&settings, serde_json::to_vec(&config.sources).unwrap()).unwrap();
    let launch = || {
        Command::new(env!("CARGO_BIN_EXE_lognara-agent"))
            .env_clear()
            .env("LOGNARA_SERVICE", "test")
            .env("LOGNARA_SERVER", "test")
            .env("LOGNARA_BACKEND", "test")
            .env("LOGNARA_RELAY_TOKEN", "test")
            .env("LOGNARA_RELAY_URL", config.relay_url.as_str())
            .env("LOGNARA_SPOOL_DIR", &config.spool_dir)
            .env("LOGNARA_SOURCES_FILE", &settings)
            .env("LOGNARA_LISTEN_ADDR", "127.0.0.1:0")
            .env("LOGNARA_FLUSH_INTERVAL_MS", "20")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap()
    };
    let mut child = launch();
    assert_eq!(
        next(&mut rx).await.records[0].payload,
        Payload::Text("first".into())
    );
    let checkpoint = config.spool_dir.join("sources.checkpoint");
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let value: serde_json::Value =
                serde_json::from_slice(&fs::read(&checkpoint).unwrap()).unwrap();
            if value["files"][0]["offset"] == 6 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    // Реальный отказ atomic checkpoint после durable append, без тестового hook.
    let blocked = config.spool_dir.join("sources.checkpoint.tmp");
    fs::create_dir(&blocked).unwrap();
    OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"saved\n")
        .unwrap();
    assert_eq!(
        next(&mut rx).await.records[0].payload,
        Payload::Text("saved".into())
    );
    OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"old tail\n")
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(400), rx.recv())
            .await
            .is_err()
    );
    let old: serde_json::Value = serde_json::from_slice(&fs::read(&checkpoint).unwrap()).unwrap();
    assert_eq!(old["files"][0]["offset"], 6);
    child.kill().await.unwrap(); // SIGKILL, финальный flush не выполняется.
    child.wait().await.unwrap();
    fs::remove_dir(blocked).unwrap();
    fs::rename(&path, dir.path().join("app.log.1")).unwrap();
    fs::write(&path, "new file\n").unwrap();
    let mut child = launch();
    let mut messages = Vec::new();
    while messages.len() < 3 {
        messages.extend(
            next(&mut rx)
                .await
                .records
                .into_iter()
                .map(|r| match r.payload {
                    Payload::Text(text) => text,
                    _ => panic!("text expected"),
                }),
        );
    }
    child.kill().await.unwrap();
    child.wait().await.unwrap();
    // Спool уже доставил saved, но старый checkpoint допускает его повтор после crash.
    assert_eq!(messages, ["saved", "old tail", "new file"]);
}

#[tokio::test]
async fn retries_permissions_and_deduplicates_aliases_while_excluding_archives_and_spool() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.log");
    fs::write(&path, "readable\n").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
    std::os::unix::fs::symlink(&path, dir.path().join("alias.log")).unwrap();
    fs::write(dir.path().join("archive.gz"), "archive\n").unwrap();
    let (config, mut rx) = setup(dir.path(), dir.path().join("*").to_str().unwrap()).await;
    fs::create_dir_all(&config.spool_dir).unwrap();
    fs::write(config.spool_dir.join("internal.log"), "internal\n").unwrap();
    let (stop, task) = start(config).await;
    // Root обходил бы mode 000; обычный пользователь должен ждать восстановления прав.
    if unsafe { libc::geteuid() } != 0 {
        assert!(
            tokio::time::timeout(Duration::from_millis(300), rx.recv())
                .await
                .is_err()
        );
    }
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let records = next(&mut rx).await.records;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].payload, Payload::Text("readable".into()));
    assert!(
        tokio::time::timeout(Duration::from_millis(1200), rx.recv())
            .await
            .is_err()
    );
    stop.cancel();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn updated_exclusions_apply_to_checkpoint_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.log");
    let b = dir.path().join("b.log");
    fs::write(&a, "a\n").unwrap();
    fs::write(&b, "b\n").unwrap();
    let (mut config, mut rx) = setup(dir.path(), dir.path().join("*.log").to_str().unwrap()).await;
    let (stop, task) = start(config.clone()).await;
    let mut count = 0;
    while count < 2 {
        count += next(&mut rx).await.records.len();
    }
    stop.cancel();
    task.await.unwrap().unwrap();
    // Mock отдаёт batch до HTTP ACK; остановка может законно повторить старый batch.
    while rx.try_recv().is_ok() {}
    config.sources.sources[0]
        .exclude
        .push(a.to_str().unwrap().into());
    OpenOptions::new()
        .append(true)
        .open(&a)
        .unwrap()
        .write_all(b"excluded\n")
        .unwrap();
    OpenOptions::new()
        .append(true)
        .open(&b)
        .unwrap()
        .write_all(b"included\n")
        .unwrap();
    let (stop, task) = start(config).await;
    let records = next(&mut rx).await.records;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].payload, Payload::Text("included".into()));
    assert!(
        tokio::time::timeout(Duration::from_millis(1200), rx.recv())
            .await
            .is_err()
    );
    stop.cancel();
    task.await.unwrap().unwrap();
}
