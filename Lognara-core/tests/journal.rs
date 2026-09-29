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
    let router = api::router(journal.clone());
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
