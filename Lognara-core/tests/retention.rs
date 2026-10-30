use lognara_core::{config::Config, storage::Core};
use std::time::{Duration, Instant};

fn wait(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(Instant::now() < deadline, "retention timed out");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn retention_waits_for_readers_preserves_dedup_and_uses_core_receipt_time() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config::from_lookup(|key| match key {
        "LOGNARA_ADMIN_TOKEN" => Some("ingest".into()),
        "LOGNARA_QUERY_TOKEN" => Some("query".into()),
        _ => None,
    })
    .unwrap();
    cfg.data_dir = dir.path().into();
    cfg.disk_reserve_bytes = 1;
    cfg.segment_rows = 1;
    cfg.retention = Duration::from_millis(500);
    cfg.refresh_interval = Duration::from_millis(20);
    let mut batch =
        lognara_core::wire::decode(include_bytes!("fixtures/relay-batch.bin"), 1 << 20).unwrap();
    batch.groups[0].events[0].timestamp = -1;
    let body = zstd::encode_all(&rmp_serde::to_vec_named(&batch).unwrap()[..], 3).unwrap();
    let core = Core::open(cfg.clone()).unwrap();
    let receipt = core.journal.accept(&body).unwrap();
    wait(|| !core.snapshot().closed.is_empty());
    let pinned = core.snapshot();
    let path = pinned.closed[0].path.clone();
    assert!(
        path.exists(),
        "old event time must not expire newly received data"
    );
    wait(|| core.snapshot().closed.is_empty());
    assert!(core.snapshot().generation > pinned.generation);
    assert!(path.exists(), "existing query pins physical files");
    drop(pinned);
    wait(|| !path.exists());
    assert_eq!(
        core.journal.accept(&body).unwrap().batch_id,
        receipt.batch_id
    );
    core.shutdown().unwrap();
    drop(core);
    let core = Core::open(cfg).unwrap();
    assert_eq!(
        core.journal.accept(&body).unwrap().batch_id,
        receipt.batch_id
    );
    assert!(core.snapshot().closed.is_empty());
    core.shutdown().unwrap();
}

#[test]
fn size_limit_removes_oldest_segments_first() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config::from_lookup(|key| match key {
        "LOGNARA_ADMIN_TOKEN" => Some("ingest".into()),
        "LOGNARA_QUERY_TOKEN" => Some("query".into()),
        _ => None,
    })
    .unwrap();
    cfg.data_dir = dir.path().into();
    cfg.disk_reserve_bytes = 1;
    cfg.segment_rows = 1;
    cfg.refresh_interval = Duration::from_millis(20);
    let mut batch =
        lognara_core::wire::decode(include_bytes!("fixtures/relay-batch.bin"), 1 << 20).unwrap();
    let event = batch.groups[0].events[0].clone();
    batch.groups[0].events = (0..4)
        .map(|i| {
            let mut event = event.clone();
            event.message = format!("row {i}");
            event
        })
        .collect();
    let body = zstd::encode_all(&rmp_serde::to_vec_named(&batch).unwrap()[..], 3).unwrap();
    let core = Core::open(cfg.clone()).unwrap();
    core.journal.accept(&body).unwrap();
    wait(|| core.snapshot().closed.len() == 4);
    core.shutdown().unwrap();
    let closed = core.snapshot().closed.clone();
    let size = |i: usize| {
        let (parquet, index) = closed[i].sizes().unwrap();
        parquet + index
    };
    // Предел вмещает ровно два новейших сегмента.
    cfg.retention_max_bytes = Some(size(2) + size(3));
    let newest: Vec<_> = closed[2..].iter().map(|s| s.meta.id.clone()).collect();
    let oldest: Vec<_> = closed[..2].iter().map(|s| s.path.clone()).collect();
    drop(closed);
    drop(core);
    let core = Core::open(cfg).unwrap();
    wait(|| core.snapshot().closed.len() == 2);
    let snapshot = core.snapshot();
    let kept: Vec<_> = snapshot.closed.iter().map(|s| s.meta.id.clone()).collect();
    assert_eq!(kept, newest);
    assert!(snapshot.generation > 0);
    drop(snapshot);
    wait(|| oldest.iter().all(|path| !path.exists()));
    core.shutdown().unwrap();
}
