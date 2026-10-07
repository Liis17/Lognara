//! Сквозные тесты: агент принимает логи по HTTP и доставляет пачки в фейковый relay.

use std::collections::VecDeque;
use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use lognara_agent::config::Config;
use lognara_agent::wire::{Batch, Payload};
use reqwest::Url;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

const WAIT: Duration = Duration::from_secs(5);

/// Отвечает статусами из `failures`, затем 200 и складывает принятые пачки в канал.
struct FakeRelay {
    url: Url,
    batches: mpsc::UnboundedReceiver<Batch>,
    requests: Arc<AtomicUsize>,
    bodies: Arc<Mutex<Vec<Bytes>>>,
    failures: Arc<Mutex<VecDeque<StatusCode>>>,
}

#[derive(Clone)]
struct RelayState {
    batches: mpsc::UnboundedSender<Batch>,
    failures: Arc<Mutex<VecDeque<StatusCode>>>,
    requests: Arc<AtomicUsize>,
    bodies: Arc<Mutex<Vec<Bytes>>>,
}

async fn fake_relay(failures: &[StatusCode]) -> FakeRelay {
    let (batches, receiver) = mpsc::unbounded_channel();
    let state = RelayState {
        batches,
        failures: Arc::new(Mutex::new(failures.iter().copied().collect())),
        requests: Arc::default(),
        bodies: Arc::default(),
    };
    let requests = state.requests.clone();
    let bodies = state.bodies.clone();
    let failures = state.failures.clone();
    let app = Router::new()
        .route("/v1/batches", post(receive))
        .with_state(state);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1/batches", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    FakeRelay {
        url: url.parse().unwrap(),
        batches: receiver,
        requests,
        bodies,
        failures,
    }
}

async fn receive(State(state): State<RelayState>, headers: HeaderMap, body: Bytes) -> StatusCode {
    assert_eq!(headers["authorization"], "Bearer relay-secret");
    state.requests.fetch_add(1, Ordering::SeqCst);
    state.bodies.lock().unwrap().push(body.clone());
    if let Some(status) = state.failures.lock().unwrap().pop_front() {
        return status;
    }

    assert_eq!(headers["content-type"], "application/msgpack");
    assert_eq!(headers["content-encoding"], "zstd");
    let packed = zstd::decode_all(&body[..]).unwrap();
    state
        .batches
        .send(rmp_serde::from_slice(&packed).unwrap())
        .unwrap();
    StatusCode::OK
}

struct Agent {
    addr: SocketAddr,
    shutdown: CancellationToken,
    task: JoinHandle<io::Result<()>>,
    _dir: tempfile::TempDir,
}

async fn start_agent(relay_url: Url, batch_size: usize, flush_interval: Duration) -> Agent {
    let dir = tempfile::tempdir().unwrap();
    let config = Config {
        service: "api".into(),
        server: "eu-prod-01".into(),
        backend: "barkcloud".into(),
        environment: Some("production".into()),
        service_instance: Some("api-2".into()),
        batch_size,
        flush_interval,
        max_buffer: 10_000,
        max_buffer_bytes: 64 << 20,
        max_batch_bytes: 8 << 20,
        max_record_bytes: 2 << 20,
        spool_dir: dir.path().to_path_buf(),
        spool_max_bytes: 1 << 30,
        listen_addr: "127.0.0.1:0".parse().unwrap(),
        relay_url,
        relay_token: "relay-secret".into(),
    };
    let listener = TcpListener::bind(config.listen_addr).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let shutdown = CancellationToken::new();
    let task = tokio::spawn(lognara_agent::run(config, listener, shutdown.clone()));

    Agent {
        addr,
        shutdown,
        task,
        _dir: dir,
    }
}

async fn send_log(agent: &Agent, content_type: &str, body: impl Into<reqwest::Body>) {
    let response = reqwest::Client::new()
        .post(format!("http://{}/v1/logs", agent.addr))
        .header("content-type", content_type)
        .body(body)
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::ACCEPTED);
}

async fn next_batch(relay: &mut FakeRelay) -> Batch {
    timeout(WAIT, relay.batches.recv())
        .await
        .expect("relay received no batch")
        .unwrap()
}

fn payloads(batch: Batch) -> Vec<Payload> {
    batch
        .records
        .into_iter()
        .map(|record| record.payload)
        .collect()
}

fn text(value: &str) -> Payload {
    Payload::Text(value.to_owned())
}

#[tokio::test]
async fn sends_batch_when_size_is_reached() {
    let mut relay = fake_relay(&[]).await;
    let agent = start_agent(relay.url.clone(), 3, Duration::from_secs(60)).await;

    send_log(&agent, "text/plain", "hello").await;
    send_log(&agent, "application/json", r#"{"level":"error"}"#).await;
    send_log(&agent, "application/octet-stream", vec![0u8, 159, 146, 150]).await;

    let batch = next_batch(&mut relay).await;
    assert_eq!(batch.service, "api");
    assert_eq!(batch.server, "eu-prod-01");
    assert_eq!(batch.backend, "barkcloud");
    assert_eq!(batch.environment.as_deref(), Some("production"));
    assert_eq!(batch.service_instance.as_deref(), Some("api-2"));
    assert_eq!(batch.dropped, 0);
    assert!(
        batch
            .records
            .iter()
            .all(|record| 0 < record.received_at && record.received_at <= batch.sent_at)
    );
    assert_eq!(
        payloads(batch),
        [
            text("hello"),
            Payload::Json(r#"{"level":"error"}"#.to_owned()),
            Payload::Binary(vec![0, 159, 146, 150]),
        ]
    );
}

#[tokio::test]
async fn sends_partial_batch_on_interval() {
    let mut relay = fake_relay(&[]).await;
    let agent = start_agent(relay.url.clone(), 1000, Duration::from_millis(200)).await;

    send_log(&agent, "text/plain", "tick").await;

    assert_eq!(payloads(next_batch(&mut relay).await), [text("tick")]);
}

#[tokio::test]
async fn does_not_send_empty_batches_on_interval_or_shutdown() {
    let mut relay = fake_relay(&[]).await;
    let agent = start_agent(relay.url.clone(), 1000, Duration::from_millis(20)).await;

    send_log(&agent, "application/json", "[]").await;
    assert!(
        timeout(Duration::from_millis(100), relay.batches.recv())
            .await
            .is_err()
    );
    assert_eq!(relay.requests.load(Ordering::SeqCst), 0);

    send_log(&agent, "text/plain", "one").await;
    assert_eq!(payloads(next_batch(&mut relay).await), [text("one")]);
    assert!(
        timeout(Duration::from_millis(100), relay.batches.recv())
            .await
            .is_err()
    );

    agent.shutdown.cancel();
    timeout(WAIT, agent.task)
        .await
        .expect("agent did not stop")
        .unwrap()
        .unwrap();
    assert_eq!(relay.requests.load(Ordering::SeqCst), 1);
    assert!(relay.batches.try_recv().is_err());
}

#[tokio::test]
async fn retries_while_relay_is_unavailable() {
    let mut relay = fake_relay(&[
        StatusCode::SERVICE_UNAVAILABLE,
        StatusCode::INTERNAL_SERVER_ERROR,
    ])
    .await;
    let agent = start_agent(relay.url.clone(), 1, Duration::from_secs(60)).await;

    send_log(&agent, "text/plain", "eventually").await;

    assert_eq!(payloads(next_batch(&mut relay).await), [text("eventually")]);
    assert_eq!(relay.requests.load(Ordering::SeqCst), 3);
    let bodies = relay.bodies.lock().unwrap();
    assert!(bodies.windows(2).all(|v| v[0] == v[1]));
}

#[tokio::test]
async fn retains_batch_rejected_by_relay() {
    let mut relay = fake_relay(&[StatusCode::BAD_REQUEST]).await;
    let agent = start_agent(relay.url.clone(), 1, Duration::from_secs(60)).await;

    send_log(&agent, "text/plain", "rejected").await;
    assert_eq!(payloads(next_batch(&mut relay).await), [text("rejected")]);
    assert_eq!(relay.requests.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn retains_unauthorized_batch_for_retry() {
    let mut relay = fake_relay(&[StatusCode::UNAUTHORIZED]).await;
    let agent = start_agent(relay.url.clone(), 1, Duration::from_secs(60)).await;

    send_log(&agent, "text/plain", "rejected").await;
    assert_eq!(payloads(next_batch(&mut relay).await), [text("rejected")]);
    assert_eq!(relay.requests.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn delivers_remaining_records_on_shutdown() {
    let mut relay = fake_relay(&[]).await;
    let agent = start_agent(relay.url.clone(), 1000, Duration::from_secs(60)).await;

    send_log(&agent, "text/plain", "one").await;
    send_log(&agent, "text/plain", "two").await;
    agent.shutdown.cancel();
    timeout(WAIT, agent.task)
        .await
        .expect("agent did not stop")
        .unwrap()
        .unwrap();

    let batch = relay
        .batches
        .try_recv()
        .expect("remaining records were not sent");
    assert_eq!(payloads(batch), [text("one"), text("two")]);
}

struct ProcessAgent(std::process::Child);
impl Drop for ProcessAgent {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
async fn process_agent(dir: &std::path::Path, relay: &Url, addr: SocketAddr) -> ProcessAgent {
    let child = std::process::Command::new(env!("CARGO_BIN_EXE_lognara-agent"))
        .env("LOGNARA_SERVICE", "api")
        .env("LOGNARA_SERVER", "server")
        .env("LOGNARA_BACKEND", "backend")
        .env("LOGNARA_RELAY_TOKEN", "relay-secret")
        .env("LOGNARA_RELAY_URL", relay.as_str())
        .env("LOGNARA_SPOOL_DIR", dir)
        .env("LOGNARA_LISTEN_ADDR", addr.to_string())
        .env("LOGNARA_BATCH_SIZE", "1")
        .env("LOGNARA_FLUSH_INTERVAL_MS", "100")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let process = ProcessAgent(child);
    timeout(WAIT, async {
        loop {
            if reqwest::get(format!("http://{addr}/v1/logs")).await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("process did not listen");
    process
}
#[tokio::test]
async fn sigkill_after_202_restores_every_accepted_record_and_exact_pending_bytes() {
    let mut relay = fake_relay(&vec![StatusCode::SERVICE_UNAVAILABLE; 1000]).await;
    let dir = tempfile::tempdir().unwrap();
    let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = socket.local_addr().unwrap();
    drop(socket);
    let process = process_agent(dir.path(), &relay.url, addr).await;
    let response = reqwest::Client::new()
        .post(format!("http://{addr}/v1/logs"))
        .header("content-type", "application/json")
        .body(r#"["one",{"two":2},3]"#)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    timeout(WAIT, async {
        while relay.requests.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let original = relay.bodies.lock().unwrap()[0].clone();
    drop(process); // SIGKILL, никакого graceful shutdown
    assert!(dir.path().join("pending/body").exists());
    relay.failures.lock().unwrap().clear();
    let _restarted = process_agent(dir.path(), &relay.url, addr).await;
    let mut received = Vec::new();
    for _ in 0..3 {
        received.extend(payloads(next_batch(&mut relay).await));
    }
    assert_eq!(
        received,
        [
            Payload::Json(r#""one""#.into()),
            Payload::Json(r#"{"two":2}"#.into()),
            Payload::Json("3".into())
        ]
    );
    let bodies = relay.bodies.lock().unwrap();
    assert!(bodies.iter().skip(1).any(|body| *body == original));
}
