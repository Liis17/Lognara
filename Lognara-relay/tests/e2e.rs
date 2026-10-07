//! Сквозные тесты: relay принимает пачки агентов по HTTP и доставляет события в фейковый core.

use std::io;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use lognara_relay::agent_wire::{Batch, Payload, Record};
use lognara_relay::config::Config;
use lognara_relay::core_wire::{self, CoreBatch, Group, LogLevel, Source};
use lognara_relay::normalize;
use lognara_relay::spool::Spool;
use reqwest::Url;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};
use tokio_util::sync::CancellationToken;

const WAIT: Duration = Duration::from_secs(5);
const TOKEN: &str = "secret";
const BATCH_HEADERS: [(&str, &str); 3] = [
    ("authorization", "Bearer relay-secret"),
    ("content-type", "application/msgpack"),
    ("content-encoding", "zstd"),
];

/// Отвечает статусом `status`; на 200 складывает принятые пачки в канал.
struct FakeCore {
    url: Url,
    batches: mpsc::UnboundedReceiver<CoreBatch>,
    requests: Arc<AtomicUsize>,
    status: Arc<Mutex<StatusCode>>,
    bodies: Arc<Mutex<Vec<Bytes>>>,
}

#[derive(Clone)]
struct CoreState {
    batches: mpsc::UnboundedSender<CoreBatch>,
    requests: Arc<AtomicUsize>,
    status: Arc<Mutex<StatusCode>>,
    bodies: Arc<Mutex<Vec<Bytes>>>,
}

impl FakeCore {
    fn respond(&self, status: StatusCode) {
        *self.status.lock().unwrap() = status;
    }

    fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }

    async fn next_batch(&mut self) -> CoreBatch {
        timeout(WAIT, self.batches.recv())
            .await
            .expect("core received no batch")
            .unwrap()
    }
}

async fn fake_core() -> FakeCore {
    let (batches, receiver) = mpsc::unbounded_channel();
    let state = CoreState {
        batches,
        requests: Arc::default(),
        status: Arc::new(Mutex::new(StatusCode::OK)),
        bodies: Arc::default(),
    };
    let requests = state.requests.clone();
    let status = state.status.clone();
    let bodies = state.bodies.clone();
    let app = Router::new()
        .route("/v1/batches", post(receive))
        .with_state(state);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1/batches", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    FakeCore {
        url: url.parse().unwrap(),
        batches: receiver,
        requests,
        status,
        bodies,
    }
}

async fn receive(State(state): State<CoreState>, headers: HeaderMap, body: Bytes) -> StatusCode {
    // Статус читается до учёта запроса, чтобы тест мог сменить его, дождавшись счётчика.
    let status = *state.status.lock().unwrap();
    state.bodies.lock().unwrap().push(body.clone());
    state.requests.fetch_add(1, Ordering::SeqCst);
    if status != StatusCode::OK {
        return status;
    }

    assert_eq!(headers["authorization"], format!("Bearer {TOKEN}"));
    assert_eq!(headers["content-type"], "application/msgpack");
    assert_eq!(headers["content-encoding"], "zstd");
    state.batches.send(decode(&body)).unwrap();
    StatusCode::OK
}

fn decode(body: &[u8]) -> CoreBatch {
    let packed = zstd::decode_all(body).unwrap();
    rmp_serde::from_slice(&packed).unwrap()
}

struct Relay {
    addr: SocketAddr,
    shutdown: CancellationToken,
    task: JoinHandle<io::Result<()>>,
}

impl Relay {
    async fn stop(self) {
        self.shutdown.cancel();
        timeout(WAIT, self.task)
            .await
            .expect("relay did not stop")
            .unwrap()
            .unwrap();
    }
}

fn config(core: &FakeCore, spool_dir: &Path) -> Config {
    Config {
        core_url: core.url.clone(),
        core_token: TOKEN.into(),
        relay_token: "relay-secret".into(),
        flush_interval: Duration::from_millis(100),
        batch_size: 1000,
        max_buffer: 10_000,
        listen_addr: "127.0.0.1:0".parse().unwrap(),
        spool_dir: spool_dir.to_owned(),
        spool_max_bytes: u64::MAX,
        core_max_body_bytes: 64 << 20,
        core_max_decoded_bytes: 256 << 20,
        core_max_model_bytes: 256 << 20,
    }
}

async fn start_relay(config: Config) -> Relay {
    let spool = Spool::open(&config.spool_dir, config.spool_max_bytes)
        .await
        .unwrap();
    let listener = TcpListener::bind(config.listen_addr).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let shutdown = CancellationToken::new();
    let task = tokio::spawn(lognara_relay::run(
        config,
        listener,
        spool,
        shutdown.clone(),
    ));

    Relay {
        addr,
        shutdown,
        task,
    }
}

fn source(service: &str) -> Source {
    Source {
        environment: Some("production".into()),
        server: "eu-prod-01".into(),
        backend: "barkcloud".into(),
        service: service.into(),
        service_instance: Some(format!("{service}-1")),
    }
}

fn record(message: &str) -> Record {
    Record {
        received_at: 1,
        payload: Payload::Text(message.into()),
    }
}

/// Пачка агента сервиса `service` с текстовыми записями.
fn agent_batch(service: &str, messages: &[&str]) -> Batch {
    let source = source(service);
    Batch {
        service: source.service,
        server: source.server,
        backend: source.backend,
        environment: source.environment,
        service_instance: source.service_instance,
        sent_at: 2,
        dropped: 0,
        records: messages.iter().map(|message| record(message)).collect(),
    }
}

/// Отправляет пачку так же, как агент.
async fn send(relay: &Relay, batch: &Batch) -> StatusCode {
    let packed = rmp_serde::to_vec_named(batch).unwrap();
    let body = zstd::encode_all(packed.as_slice(), 3).unwrap();
    post_body(relay, &BATCH_HEADERS, body).await
}

async fn post_body(relay: &Relay, headers: &[(&str, &str)], body: Vec<u8>) -> StatusCode {
    let mut request = reqwest::Client::new().post(format!("http://{}/v1/batches", relay.addr));
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    request.body(body).send().await.unwrap().status()
}

fn messages(group: &Group) -> Vec<&str> {
    group
        .events
        .iter()
        .map(|event| event.message.as_str())
        .collect()
}

/// Сколько пачек лежит в spool.
fn spooled(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .unwrap()
        .filter(|item| {
            let path = item.as_ref().unwrap().path();
            path.extension()
                .is_some_and(|extension| extension == "batch")
        })
        .count()
}

async fn eventually(condition: impl Fn() -> bool) {
    timeout(WAIT, async {
        while !condition() {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("condition was not met in time");
}

#[tokio::test]
async fn groups_events_by_source_on_interval() {
    let mut core = fake_core().await;
    let spool = tempfile::tempdir().unwrap();
    let mut config = config(&core, spool.path());
    config.flush_interval = Duration::from_millis(500);
    let relay = start_relay(config).await;

    let mut api = agent_batch("api", &["a1", "a2"]);
    api.dropped = 3;
    assert_eq!(send(&relay, &api).await, StatusCode::ACCEPTED);
    assert_eq!(
        send(&relay, &agent_batch("worker", &["w1"])).await,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        send(&relay, &agent_batch("api", &["a3"])).await,
        StatusCode::ACCEPTED
    );

    let batch = core.next_batch().await;
    assert_eq!(batch.dropped, 0);
    assert_eq!(batch.groups.len(), 2);
    assert_eq!(batch.groups[0].source, source("api"));
    assert_eq!(batch.groups[0].dropped, 3);
    assert_eq!(messages(&batch.groups[0]), ["a1", "a2", "a3"]);
    assert_eq!(batch.groups[1].source, source("worker"));
    assert_eq!(messages(&batch.groups[1]), ["w1"]);

    let event = &batch.groups[1].events[0];
    assert_eq!(event.level, LogLevel::Unknown);
    assert_eq!(event.ingested_at, 1);
    assert_eq!(spooled(spool.path()), 0);
}

#[tokio::test]
async fn sends_early_when_batch_size_is_reached() {
    let mut core = fake_core().await;
    let spool = tempfile::tempdir().unwrap();
    let mut config = config(&core, spool.path());
    config.flush_interval = Duration::from_secs(60);
    config.batch_size = 3;
    let relay = start_relay(config).await;

    send(&relay, &agent_batch("api", &["1", "2"])).await;
    send(&relay, &agent_batch("api", &["3"])).await;

    let batch = core.next_batch().await;
    assert_eq!(messages(&batch.groups[0]), ["1", "2", "3"]);
}

#[tokio::test]
async fn spools_while_core_is_down_and_sends_oldest_first() {
    let mut core = fake_core().await;
    core.respond(StatusCode::SERVICE_UNAVAILABLE);
    let spool = tempfile::tempdir().unwrap();
    let relay = start_relay(config(&core, spool.path())).await;

    send(&relay, &agent_batch("api", &["old"])).await;
    eventually(|| spooled(spool.path()) == 1).await;
    send(&relay, &agent_batch("api", &["new"])).await;
    eventually(|| spooled(spool.path()) == 2).await;

    core.respond(StatusCode::OK);
    assert_eq!(messages(&core.next_batch().await.groups[0]), ["old"]);
    assert_eq!(messages(&core.next_batch().await.groups[0]), ["new"]);
    eventually(|| spooled(spool.path()) == 0).await;
}

#[tokio::test]
async fn delivers_spool_left_from_previous_run_first() {
    let mut core = fake_core().await;
    let spool = tempfile::tempdir().unwrap();
    let leftover = CoreBatch {
        dropped: 4,
        groups: vec![Group {
            source: source("api"),
            dropped: 0,
            events: vec![normalize::event(record("leftover"))],
        }],
    };
    let mut previous = Spool::open(spool.path(), u64::MAX).await.unwrap();
    previous.push(&core_wire::encode(&leftover), 1).await;
    drop(previous);

    let mut config = config(&core, spool.path());
    config.flush_interval = Duration::from_millis(500);
    let relay = start_relay(config).await;
    send(&relay, &agent_batch("api", &["fresh"])).await;

    assert_eq!(core.next_batch().await, leftover);
    assert_eq!(messages(&core.next_batch().await.groups[0]), ["fresh"]);
    assert_eq!(spooled(spool.path()), 0);
}

#[tokio::test]
async fn keeps_events_in_spool_on_shutdown_when_core_is_down() {
    let core = fake_core().await;
    core.respond(StatusCode::SERVICE_UNAVAILABLE);
    let spool = tempfile::tempdir().unwrap();
    let mut config = config(&core, spool.path());
    config.flush_interval = Duration::from_secs(60);
    let relay = start_relay(config).await;

    send(&relay, &agent_batch("api", &["late"])).await;
    relay.stop().await;

    assert_eq!(core.requests(), 1);
    let mut saved = Spool::open(spool.path(), u64::MAX).await.unwrap();
    assert_eq!(saved.len(), 1);
    let batch = decode(&saved.oldest().await.unwrap());
    assert_eq!(messages(&batch.groups[0]), ["late"]);
}

#[tokio::test]
async fn split_batches_survive_spool_restart_with_identical_retry_bytes() {
    let mut core = fake_core().await;
    core.respond(StatusCode::SERVICE_UNAVAILABLE);
    let spool = tempfile::tempdir().unwrap();
    let mut cfg = config(&core, spool.path());
    cfg.core_max_decoded_bytes = 900;
    let relay = start_relay(cfg.clone()).await;
    let messages: Vec<_> = (0..6)
        .map(|id| format!("{id} {}", "x".repeat(350)))
        .collect();
    let refs: Vec<_> = messages.iter().map(String::as_str).collect();
    assert_eq!(
        send(&relay, &agent_batch("api", &refs)).await,
        StatusCode::ACCEPTED
    );
    eventually(|| spooled(spool.path()) == 6).await;
    relay.stop().await;
    let failed_body = core.bodies.lock().unwrap()[0].clone();
    let mut saved = Spool::open(spool.path(), u64::MAX).await.unwrap();
    assert_eq!(saved.oldest().await.unwrap().as_slice(), &failed_body[..]);
    drop(saved);
    core.respond(StatusCode::OK);
    let restarted = start_relay(cfg).await;
    let mut restored = vec![];
    for _ in 0..6 {
        let batch = core.next_batch().await;
        assert!(rmp_serde::to_vec_named(&batch).unwrap().len() <= 900);
        restored.extend(
            batch
                .groups
                .into_iter()
                .flat_map(|group| group.events)
                .map(|event| event.message),
        );
    }
    assert_eq!(restored, messages);
    assert!(
        core.bodies
            .lock()
            .unwrap()
            .iter()
            .filter(|body| **body == failed_body)
            .count()
            >= 2
    );
    restarted.stop().await;
}

#[tokio::test]
async fn drops_batch_rejected_by_core() {
    let mut core = fake_core().await;
    core.respond(StatusCode::BAD_REQUEST);
    let spool = tempfile::tempdir().unwrap();
    let relay = start_relay(config(&core, spool.path())).await;

    send(&relay, &agent_batch("api", &["rejected"])).await;
    eventually(|| core.requests() == 1).await;
    core.respond(StatusCode::OK);
    send(&relay, &agent_batch("api", &["accepted"])).await;

    assert_eq!(messages(&core.next_batch().await.groups[0]), ["accepted"]);
    assert_eq!(spooled(spool.path()), 0);
}

#[tokio::test]
async fn rejects_unauthenticated_batches_without_delivering_or_spooling() {
    let mut core = fake_core().await;
    let spool = tempfile::tempdir().unwrap();
    let mut cfg = config(&core, spool.path());
    cfg.flush_interval = Duration::from_secs(60);
    cfg.batch_size = 1;
    let relay = start_relay(cfg).await;
    let mut rejected = agent_batch("api", &["rejected"]);
    rejected.dropped = 17;
    let packed = rmp_serde::to_vec_named(&rejected).unwrap();
    let body = zstd::encode_all(packed.as_slice(), 3).unwrap();

    let response = reqwest::Client::new()
        .post(format!("http://{}/v1/batches", relay.addr))
        .header("content-type", "application/msgpack")
        .header("content-encoding", "zstd")
        .body(body.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(response.headers()["www-authenticate"], "Bearer");

    send(&relay, &agent_batch("api", &["accepted"])).await;
    let accepted = core.next_batch().await;
    assert_eq!(messages(&accepted.groups[0]), ["accepted"]);
    assert_eq!(accepted.dropped, 0);
    assert_eq!(accepted.groups[0].dropped, 0);
    relay.stop().await;
    assert_eq!(core.requests(), 1);
    assert_eq!(spooled(spool.path()), 0);

    // При недоступном core отказ тоже не должен создавать spool на остановке.
    core.respond(StatusCode::SERVICE_UNAVAILABLE);
    let relay = start_relay(config(&core, spool.path())).await;
    assert_eq!(
        post_body(&relay, &BATCH_HEADERS[1..], body).await,
        StatusCode::UNAUTHORIZED
    );
    relay.stop().await;
    assert_eq!(core.requests(), 1);
    assert_eq!(spooled(spool.path()), 0);
}

#[tokio::test]
async fn rejects_invalid_requests_and_overflow() {
    let core = fake_core().await;
    let spool = tempfile::tempdir().unwrap();
    let mut config = config(&core, spool.path());
    config.flush_interval = Duration::from_secs(60);
    config.max_buffer = 2;
    let relay = start_relay(config).await;

    let text = [
        ("authorization", "Bearer relay-secret"),
        ("content-type", "text/plain"),
        ("content-encoding", "zstd"),
    ];
    assert_eq!(
        post_body(&relay, &text, b"hi".to_vec()).await,
        StatusCode::UNSUPPORTED_MEDIA_TYPE
    );
    assert_eq!(
        post_body(&relay, &BATCH_HEADERS[..1], b"hi".to_vec()).await,
        StatusCode::UNSUPPORTED_MEDIA_TYPE
    );
    assert_eq!(
        post_body(&relay, &BATCH_HEADERS, b"not zstd".to_vec()).await,
        StatusCode::BAD_REQUEST
    );

    assert_eq!(
        send(&relay, &agent_batch("api", &["1", "2"])).await,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        send(&relay, &agent_batch("api", &["3"])).await,
        StatusCode::SERVICE_UNAVAILABLE
    );
}
