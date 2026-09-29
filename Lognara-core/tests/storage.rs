use lognara_core::{columns, config::Config, storage::Core, wire};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

fn config(path: &std::path::Path) -> Config {
    let mut cfg = Config::from_lookup(|key| match key {
        "LOGNARA_INGEST_TOKEN" => Some("ingest".into()),
        "LOGNARA_QUERY_TOKEN" => Some("query".into()),
        _ => None,
    })
    .unwrap();
    cfg.data_dir = path.into();
    cfg.disk_reserve_bytes = 1;
    cfg.refresh_interval = Duration::from_millis(20);
    cfg.segment_rows = 2;
    cfg
}

fn batch(n: usize) -> Vec<u8> {
    let mut batch = wire::decode(include_bytes!("fixtures/relay-batch.bin"), 1 << 20).unwrap();
    let event = batch.groups[0].events[0].clone();
    batch.groups[0].events = (0..n)
        .map(|i| {
            let mut event = event.clone();
            event.message = format!("row {i}");
            event
        })
        .collect();
    zstd::encode_all(&rmp_serde::to_vec_named(&batch).unwrap()[..], 3).unwrap()
}

fn wait_visible(core: &Arc<Core>, count: u64) {
    let until = Instant::now() + Duration::from_secs(10);
    while core.snapshot().watermark < count {
        assert!(Instant::now() < until, "publication timed out");
        assert!(core.journal.ready(), "worker failed");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn batch_spans_segments_and_remains_deduplicated_after_wal_cleanup_and_restart() {
    let dir = tempfile::tempdir().unwrap();
    let body = batch(5);
    let core = Core::open(config(dir.path())).unwrap();
    let accepted = core.journal.accept(&body).unwrap();
    wait_visible(&core, 5);
    core.shutdown().unwrap();
    let snapshot = core.snapshot();
    assert_eq!(snapshot.closed.len(), 3);
    let mut rows = vec![];
    for segment in &snapshot.closed {
        for batch in columns::read_all(&segment.path.join("logs.parquet")).unwrap() {
            rows.extend(columns::decode(&batch).unwrap());
        }
        assert_eq!(
            core.indexes
                .searcher(&segment.path, segment.meta.rows)
                .unwrap()
                .num_docs(),
            segment.meta.rows as u64
        );
    }
    rows.sort_by_key(|row| row.sequence);
    assert_eq!(
        rows.iter()
            .map(|row| row.event.message.as_str())
            .collect::<Vec<_>>(),
        vec!["row 0", "row 1", "row 2", "row 3", "row 4"]
    );
    assert_eq!(
        std::fs::read_dir(dir.path().join("wal")).unwrap().count(),
        0
    );
    drop(snapshot);
    drop(core);
    let core = Core::open(config(dir.path())).unwrap();
    assert_eq!(
        core.journal.accept(&body).unwrap().batch_id,
        accepted.batch_id
    );
    assert_eq!(core.snapshot().watermark, 5);
    core.shutdown().unwrap();
}

#[test]
fn open_segment_is_visible_and_missing_index_is_rebuilt_from_parquet() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config(dir.path());
    cfg.segment_rows = 1000;
    let core = Core::open(cfg.clone()).unwrap();
    core.journal.accept(&batch(1)).unwrap();
    wait_visible(&core, 1);
    let snapshot = core.snapshot();
    assert!(snapshot.closed.is_empty());
    assert_eq!(snapshot.active.as_ref().unwrap().searcher.num_docs(), 1);
    assert_eq!(
        columns::decode(&snapshot.active.as_ref().unwrap().batches[0]).unwrap()[0]
            .event
            .message,
        "row 0"
    );
    drop(snapshot);
    core.shutdown().unwrap();
    let path = core.snapshot().closed[0].path.clone();
    drop(core);
    std::fs::remove_dir_all(path.join("index")).unwrap();
    let core = Core::open(cfg).unwrap();
    assert_eq!(core.indexes.searcher(&path, 1).unwrap().num_docs(), 1);
    core.shutdown().unwrap();
}
