use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use lognara_core::{
    api,
    config::Config,
    journal::{IngestError, Journal},
};
use std::sync::Arc;
use tower::ServiceExt;

fn config(dir: &std::path::Path) -> Config {
    let mut config = Config::from_lookup(|key| match key {
        "LOGNARA_INGEST_TOKEN" => Some("ingest".into()),
        "LOGNARA_QUERY_TOKEN" => Some("query".into()),
        _ => None,
    })
    .unwrap();
    config.data_dir = dir.into();
    config.disk_reserve_bytes = 1;
    config
}

const FIXTURE: &[u8] = include_bytes!("fixtures/relay-batch.bin");

fn request(body: Body) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/v1/batches")
        .header("authorization", "Bearer ingest")
        .header("content-type", "application/msgpack")
        .header("content-encoding", "zstd")
        .body(body)
        .unwrap()
}

#[tokio::test]
async fn concurrent_http_retries_wait_and_body_transport_errors_remain_retryable() {
    let dir = tempfile::tempdir().unwrap();
    let journal = Journal::open(config(dir.path())).unwrap();
    let router = api::ingest_router(journal.clone());
    let blocked = journal.ingest_slots.clone().acquire_owned().await.unwrap();
    let mut requests = vec![];
    for _ in 0..8 {
        let router = router.clone();
        requests.push(tokio::spawn(async move {
            router
                .oneshot(request(Body::from(FIXTURE)))
                .await
                .unwrap()
                .status()
        }));
    }
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    assert!(requests.iter().all(|task| !task.is_finished()));
    drop(blocked);
    for task in requests {
        assert_eq!(task.await.unwrap(), StatusCode::NO_CONTENT);
    }
    assert_eq!(
        journal
            .metrics
            .accepted_batches
            .load(std::sync::atomic::Ordering::Relaxed),
        1
    );
    assert_eq!(
        journal
            .metrics
            .duplicate_batches
            .load(std::sync::atomic::Ordering::Relaxed),
        7
    );

    let broken = Body::from_stream(futures::stream::iter([Err::<bytes::Bytes, _>(
        std::io::Error::new(std::io::ErrorKind::ConnectionReset, "reset"),
    )]));
    assert_eq!(
        router.oneshot(request(broken)).await.unwrap().status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    drop(journal);

    let small = tempfile::tempdir().unwrap();
    let mut cfg = config(small.path());
    cfg.max_body_bytes = 10;
    let router = api::ingest_router(Journal::open(cfg).unwrap());
    assert_eq!(
        router
            .oneshot(request(Body::from(FIXTURE)))
            .await
            .unwrap()
            .status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
}

#[test]
fn retry_after_restart_keeps_original_sequence_and_deduplicates_concurrent_delivery() {
    let dir = tempfile::tempdir().unwrap();
    let journal = Journal::open(config(dir.path())).unwrap();
    let first = journal.accept(FIXTURE).unwrap();
    assert!(
        Journal::open(config(dir.path())).is_err(),
        "second writer must be excluded"
    );
    drop(journal);
    let journal = Journal::open(config(dir.path())).unwrap();
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let journal = Arc::clone(&journal);
            std::thread::spawn(move || journal.accept(FIXTURE).unwrap())
        })
        .collect();
    for thread in threads {
        let retry = thread.join().unwrap();
        assert_eq!(retry.batch_id, first.batch_id);
        assert_eq!(retry.first_sequence, first.first_sequence);
    }
    assert_eq!(
        std::fs::read_dir(dir.path().join("wal")).unwrap().count(),
        1
    );
}

#[tokio::test]
async fn http_auth_precedes_decode_and_ack_is_durable() {
    let dir = tempfile::tempdir().unwrap();
    let journal = Journal::open(config(dir.path())).unwrap();
    let router = api::ingest_router(journal.clone());
    let request = |token: &str, body: &[u8]| {
        Request::builder()
            .method("POST")
            .uri("/v1/batches")
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/msgpack")
            .header("content-encoding", "zstd")
            .body(Body::from(body.to_vec()))
            .unwrap()
    };
    assert_eq!(
        router
            .clone()
            .oneshot(request("query", b"invalid"))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        router
            .clone()
            .oneshot(request("ingest", b"invalid"))
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        router
            .clone()
            .oneshot(request("ingest", FIXTURE))
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    drop(router);
    drop(journal);
    let restored = Journal::open(config(dir.path())).unwrap();
    assert_eq!(restored.accept(FIXTURE).unwrap().batch_id, 1);
}

#[test]
fn backpressure_never_acknowledges_unstored_data() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config(dir.path());
    cfg.wal_max_bytes = 1;
    let journal = Journal::open(cfg).unwrap();
    assert!(matches!(
        journal.accept(FIXTURE),
        Err(IngestError::Unavailable)
    ));
    drop(journal);
    let mut cfg = config(dir.path());
    cfg.disk_reserve_bytes = u64::MAX;
    let journal = Journal::open(cfg).unwrap();
    assert!(matches!(
        journal.accept(FIXTURE),
        Err(IngestError::Unavailable)
    ));
    assert_eq!(
        std::fs::read_dir(dir.path().join("wal")).unwrap().count(),
        0
    );
}
