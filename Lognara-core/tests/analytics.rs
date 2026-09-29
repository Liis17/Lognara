use lognara_core::{
    analytics::{self, GroupRequest, HistogramRequest},
    config::Config,
    model::format_timestamp,
    storage::Core,
    wire,
};
use std::{
    collections::BTreeMap,
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
    cfg.segment_rows = 2;
    cfg.refresh_interval = Duration::from_millis(20);
    cfg
}

fn batch() -> Vec<u8> {
    let mut batch = wire::decode(include_bytes!("fixtures/relay-batch.bin"), 1 << 20).unwrap();
    let base = batch.groups[0].events[0].clone();
    batch.groups[0].events = [-1, 0, 999_999_999, 1_000_000_000, 3_000_000_000]
        .into_iter()
        .enumerate()
        .map(|(i, ts)| {
            let mut event = base.clone();
            event.timestamp = ts;
            event.level = if i == 2 {
                wire::LogLevel::Unknown
            } else {
                wire::LogLevel::Error
            };
            event
        })
        .collect();
    batch.groups[0].source.environment = None;
    zstd::encode_all(&rmp_serde::to_vec_named(&batch).unwrap()[..], 3).unwrap()
}

async fn wait(core: &Core, sequence: u64) {
    let end = Instant::now() + Duration::from_secs(10);
    while core.snapshot().watermark < sequence {
        assert!(Instant::now() < end && core.journal.ready());
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn histogram_and_groups_count_one_snapshot_across_parquet_and_open_arrow() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(config(dir.path())).unwrap();
    core.journal.accept(&batch()).unwrap();
    wait(&core, 5).await;
    let histogram = || HistogramRequest {
        from: format_timestamp(-1),
        to: format_timestamp(3_000_000_000),
        filters: BTreeMap::new(),
        interval_seconds: 1,
    };
    let result = analytics::histogram(&core, histogram()).await.unwrap();
    assert_eq!(
        result
            .buckets
            .iter()
            .map(|bucket| bucket.count)
            .collect::<Vec<_>>(),
        [1, 2, 1, 0]
    );
    assert_eq!(result.buckets[0].timestamp, "1969-12-31T23:59:59Z");
    let group = GroupRequest {
        from: format_timestamp(-1),
        to: format_timestamp(3_000_000_001),
        filters: BTreeMap::new(),
        group_by: vec!["level".into(), "environment".into()],
        limit: 10,
    };
    let groups = analytics::group_by(&core, group).await.unwrap().groups;
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0].count, 4);
    assert_eq!(groups[0].values["level"].as_deref(), Some("error"));
    assert_eq!(groups[0].values["environment"], None);
    assert_eq!(groups[1].count, 1);
    assert_eq!(groups[1].values["level"].as_deref(), Some("unknown"));
    let mut filtered = histogram();
    filtered
        .filters
        .insert("level".into(), vec!["unknown".into()]);
    filtered.filters.insert(
        "trace_id".into(),
        vec!["8c21f0a4b6c8d0e12a4b6c8d0e1f3a5b".into()],
    );
    assert_eq!(
        analytics::histogram(&core, filtered)
            .await
            .unwrap()
            .buckets
            .iter()
            .map(|bucket| bucket.count)
            .sum::<u64>(),
        1
    );
    core.shutdown().unwrap();
    assert_eq!(
        analytics::histogram(&core, histogram())
            .await
            .unwrap()
            .buckets
            .iter()
            .map(|bucket| bucket.count)
            .collect::<Vec<_>>(),
        [1, 2, 1, 0]
    );
}

#[tokio::test]
async fn empty_histogram_has_zeros_and_invalid_analytics_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(config(dir.path())).unwrap();
    let request = HistogramRequest {
        from: format_timestamp(0),
        to: format_timestamp(2_000_000_000),
        filters: BTreeMap::new(),
        interval_seconds: 1,
    };
    assert_eq!(
        analytics::histogram(&core, request)
            .await
            .unwrap()
            .buckets
            .iter()
            .map(|bucket| bucket.count)
            .collect::<Vec<_>>(),
        [0, 0]
    );
    let invalid = GroupRequest {
        from: format_timestamp(0),
        to: format_timestamp(1),
        filters: BTreeMap::new(),
        group_by: vec!["request_id".into()],
        limit: 10,
    };
    assert!(analytics::group_by(&core, invalid).await.is_err());
    assert!(serde_json::from_value::<HistogramRequest>(serde_json::json!({"from":"1970-01-01T00:00:00Z","to":"1970-01-01T00:00:01Z","text":{"query":"ignored?"}})).is_err());
    core.shutdown().unwrap();
}
