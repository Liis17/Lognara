use std::process::Stdio;
use std::time::Duration;

use axum::{Router, body::Bytes, routing::post};
use lognara_agent::wire::{Batch, Payload};
use tokio::net::TcpListener;
use tokio::process::Command;
use tokio::sync::mpsc;

async fn relay() -> (String, mpsc::UnboundedReceiver<Batch>) {
    relay_status(axum::http::StatusCode::ACCEPTED).await
}

async fn relay_status(status: axum::http::StatusCode) -> (String, mpsc::UnboundedReceiver<Batch>) {
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
                        tx.send(
                            rmp_serde::from_slice(&zstd::decode_all(&body[..]).unwrap()).unwrap(),
                        )
                        .unwrap();
                        status
                    }
                }),
            ),
        )
        .await
        .unwrap();
    });
    (url, rx)
}

fn agent(dir: &std::path::Path, url: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_lognara-agent"));
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LOGNARA_SERVICE", "app")
        .env("LOGNARA_SERVER", "local")
        .env("LOGNARA_BACKEND", "test")
        .env("LOGNARA_RELAY_TOKEN", "relay-secret")
        .env("LOGNARA_RELAY_URL", url)
        .env("LOGNARA_SPOOL_DIR", dir.join("spool"))
        .env("LOGNARA_LISTEN_ADDR", "127.0.0.1:0")
        .env("LOGNARA_FLUSH_INTERVAL_MS", "20")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command
}

#[tokio::test]
async fn captures_both_streams_echoes_bytes_removes_token_and_preserves_exit_code() {
    let dir = tempfile::tempdir().unwrap();
    let (url, mut rx) = relay().await;
    let output = tokio::time::timeout(Duration::from_secs(8), agent(dir.path(), &url)
        .args(["run", "--", "/bin/sh", "-c", "sleep .1; printf 'out\\n'; printf 'err\\n' >&2; test -z \"$LOGNARA_RELAY_TOKEN\" || exit 99; sleep .2; exit 7"])
        .output()).await.unwrap().unwrap();
    assert_eq!(output.status.code(), Some(7));
    assert_eq!(output.stdout, b"out\n");
    assert!(String::from_utf8_lossy(&output.stderr).contains("err\n"));
    let mut records = Vec::new();
    while records.len() < 2 {
        records.extend(
            tokio::time::timeout(Duration::from_secs(3), rx.recv())
                .await
                .unwrap()
                .unwrap()
                .records,
        );
    }
    assert!(
        records
            .iter()
            .any(|r| r.payload == Payload::Text("out".into())
                && r.origin.as_ref().unwrap().stream.as_deref() == Some("stdout"))
    );
    assert!(
        records
            .iter()
            .any(|r| r.payload == Payload::Text("err".into())
                && r.origin.as_ref().unwrap().stream.as_deref() == Some("stderr"))
    );
}

#[tokio::test]
async fn refuses_to_launch_program_when_spool_or_source_configuration_is_invalid() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("spool"), "not a directory").unwrap();
    let marker = dir.path().join("started");
    let output = agent(dir.path(), "http://127.0.0.1:1/v1/batches")
        .args(["run", "--", "/usr/bin/touch"])
        .arg(&marker)
        .output()
        .await
        .unwrap();
    assert!(!output.status.success());
    assert!(!marker.exists());
}

#[tokio::test]
async fn uses_stream_formats_and_flushes_multiline_at_eof() {
    let dir = tempfile::tempdir().unwrap();
    let settings = dir.path().join("sources.json");
    std::fs::write(&settings, r#"{"sources":[{"id":"out","type":"stdout","format":"ndjson"},{"id":"err","type":"stderr","multiline":{"start_pattern":"^ERROR"}}]}"#).unwrap();
    let (url, mut rx) = relay().await;
    let output = agent(dir.path(), &url)
        .env("LOGNARA_SOURCES_FILE", settings)
        .args([
            "run",
            "--",
            "/bin/sh",
            "-c",
            "printf '{\"message\":\"ok\"}\\n'; printf 'ERROR first\\n  trace\\nERROR last' >&2",
        ])
        .output()
        .await
        .unwrap();
    assert!(output.status.success());
    let mut records = Vec::new();
    while records.len() < 3 {
        records.extend(
            tokio::time::timeout(Duration::from_secs(3), rx.recv())
                .await
                .unwrap()
                .unwrap()
                .records,
        );
    }
    assert!(
        records
            .iter()
            .any(|r| r.payload == Payload::Json(r#"{"message":"ok"}"#.into()))
    );
    assert!(
        records
            .iter()
            .any(|r| r.payload == Payload::Text("ERROR first\n  trace".into()))
    );
    assert!(
        records
            .iter()
            .any(|r| r.payload == Payload::Text("ERROR last".into()))
    );
}

#[tokio::test]
async fn forwards_control_and_stop_signals_and_collects_the_final_message() {
    let dir = tempfile::tempdir().unwrap();
    let (url, mut rx) = relay().await;
    let script = "import os,signal,time\ndef handle(sig,frame):\n os.write(1,('signal %s\\n'%sig).encode())\n if sig==signal.SIGQUIT: os.write(1,b'final\\n'); os._exit(23)\nfor sig in (signal.SIGTERM,signal.SIGINT,signal.SIGQUIT,signal.SIGHUP,signal.SIGUSR1,signal.SIGUSR2): signal.signal(sig,handle)\nos.write(1,b'ready\\n')\nwhile True: time.sleep(.05)\n";
    let child = agent(dir.path(), &url)
        .args(["run", "--", "/usr/bin/python3", "-c", script])
        .spawn()
        .unwrap();
    let ready = tokio::time::timeout(Duration::from_secs(8), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ready.records[0].payload, Payload::Text("ready".into()));
    for number in [
        libc::SIGHUP,
        libc::SIGUSR1,
        libc::SIGUSR2,
        libc::SIGINT,
        libc::SIGTERM,
    ] {
        // SAFETY: PID belongs to the still-running child spawned above.
        assert_eq!(unsafe { libc::kill(child.id().unwrap() as i32, number) }, 0);
        let batch = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            batch.records[0].payload,
            Payload::Text(format!("signal {number}"))
        );
    }
    // SAFETY: PID still belongs to our agent; QUIT is forwarded to its program.
    assert_eq!(
        unsafe { libc::kill(child.id().unwrap() as i32, libc::SIGQUIT) },
        0
    );
    let output = tokio::time::timeout(Duration::from_secs(8), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(output.status.code(), Some(23));
    assert!(String::from_utf8_lossy(&output.stdout).contains("final\n"));
    let mut final_seen = false;
    while let Ok(batch) = rx.try_recv() {
        final_seen |= batch
            .records
            .iter()
            .any(|r| r.payload == Payload::Text("final".into()));
    }
    assert!(final_seen);
}

#[tokio::test]
async fn full_spool_and_an_unread_console_do_not_stop_the_program() {
    use tokio::io::AsyncReadExt;
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("finished");
    let begin = dir.path().join("begin");
    let (url, mut rx) = relay_status(axum::http::StatusCode::SERVICE_UNAVAILABLE).await;
    let mut child = agent(dir.path(), &url)
        .env("LOGNARA_MAX_BUFFER", "1").env("LOGNARA_BATCH_SIZE", "1")
        .args(["run", "--", "/bin/sh", "-c", "printf 'seed\\n'; while [ ! -f \"$1\" ]; do sleep .01; done; dd if=/dev/zero bs=65536 count=256 2>/dev/null; dd if=/dev/zero bs=65536 count=256 >&2 2>/dev/null; touch \"$2\"", "fixture"])
        .arg(&begin).arg(&marker).spawn().unwrap();
    let seed = tokio::time::timeout(Duration::from_secs(8), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(seed.records[0].payload, Payload::Text("seed".into()));
    std::fs::write(begin, "go").unwrap();
    // Both console pipes stay unread throughout the flood.
    tokio::time::timeout(Duration::from_secs(5), async {
        while !marker.exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    // Once the application has finished, unblock stderr so diagnostics can be observed.
    let mut stderr = child.stderr.take().unwrap();
    let mut error_output = Vec::new();
    let (status, read) = tokio::join!(
        tokio::time::timeout(Duration::from_secs(15), child.wait()),
        stderr.read_to_end(&mut error_output)
    );
    assert!(status.unwrap().unwrap().success());
    read.unwrap();
    let text = String::from_utf8_lossy(&error_output);
    assert!(
        text.contains("stream collection losses"),
        "loss diagnostic missing"
    );
    assert!(text.contains("echo_dropped_bytes="));
    assert!(text.contains("dropped_bytes="));
}

#[tokio::test]
async fn returns_standard_codes_when_the_command_cannot_be_executed() {
    let dir = tempfile::tempdir().unwrap();
    let missing = agent(dir.path(), "http://127.0.0.1:1/v1/batches")
        .args(["run", "--", "/definitely/no/such/command"])
        .output()
        .await
        .unwrap();
    assert_eq!(missing.status.code(), Some(127));
    let file = dir.path().join("not-executable");
    std::fs::write(&file, "plain text").unwrap();
    let denied = agent(dir.path(), "http://127.0.0.1:1/v1/batches")
        .args(["run", "--"])
        .arg(file)
        .output()
        .await
        .unwrap();
    assert_eq!(denied.status.code(), Some(126));
}

#[tokio::test]
async fn inherited_pipes_in_a_descendant_do_not_prevent_shutdown() {
    let dir = tempfile::tempdir().unwrap();
    let (url, _rx) = relay().await;
    let start = std::time::Instant::now();
    let output = tokio::time::timeout(
        Duration::from_secs(7),
        agent(dir.path(), &url)
            .args([
                "run",
                "--",
                "/bin/sh",
                "-c",
                "(trap '' TERM; sleep 60) & printf 'done\\n'; sleep .2; exit 0",
            ])
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(output.status.success());
    assert!(start.elapsed() < Duration::from_secs(6));
    assert!(String::from_utf8_lossy(&output.stderr).contains("shutdown_cutoffs="));
}

#[tokio::test]
async fn refuses_to_launch_on_invalid_sources_or_occupied_http_port() {
    let dir = tempfile::tempdir().unwrap();
    let settings = dir.path().join("sources.json");
    std::fs::write(&settings, r#"{"sources":[],"unknown":true}"#).unwrap();
    let marker = dir.path().join("started");
    let output = agent(dir.path(), "http://127.0.0.1:1/v1/batches")
        .env("LOGNARA_SOURCES_FILE", &settings)
        .args(["run", "--", "/usr/bin/touch"])
        .arg(&marker)
        .output()
        .await
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(!marker.exists());
    let occupied = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let output = agent(dir.path(), "http://127.0.0.1:1/v1/batches")
        .env(
            "LOGNARA_LISTEN_ADDR",
            occupied.local_addr().unwrap().to_string(),
        )
        .args(["run", "--", "/usr/bin/touch"])
        .arg(&marker)
        .output()
        .await
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(!marker.exists());
}
