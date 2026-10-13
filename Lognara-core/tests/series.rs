use datafusion::arrow::array::{Array, Float64Array, StringArray, TimestampNanosecondArray};
use lognara_core::{
    catalog::{Catalog, SeriesName},
    columns,
    config::Config,
    storage::Core,
    wire,
};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

const REPORT_TIME: &str = "2026-10-13T15:14:00.4480321Z";
const REPORT_NANOS: i64 = 1_791_904_440_448_032_100;

fn config(path: &Path) -> Config {
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

fn report(counters: Value, gauges: Value) -> Value {
    json!({
        "SchemaVersion": 2,
        "ServiceName": "BarkFluff.Onliner",
        "Counters": counters,
        "Gauges": gauges,
        "Timestamp": REPORT_TIME
    })
}

/// Пачка из фикстуры: по одной записи на элемент; None - обычный лог без атрибута Metrics.
fn batch(metrics: &[Option<Value>]) -> Vec<u8> {
    let mut batch = wire::decode(include_bytes!("fixtures/relay-batch.bin"), 1 << 20).unwrap();
    let base = batch.groups[0].events[0].clone();
    batch.groups[0].events = metrics
        .iter()
        .enumerate()
        .map(|(i, metric)| {
            let mut event = base.clone();
            event.message = format!("row {i}");
            event.attributes.clear();
            if let Some(metric) = metric {
                event.attributes.insert("Metrics".into(), metric.clone());
            }
            event
        })
        .collect();
    zstd::encode_all(&rmp_serde::to_vec_named(&batch).unwrap()[..], 3).unwrap()
}

fn wait(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(Instant::now() < deadline, "series timed out");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Строки серии: (name, origin, value, timestamp), отсортированные по имени.
fn read_series(path: &Path) -> Vec<(String, String, f64, i64)> {
    let mut rows = vec![];
    for batch in columns::read_all(path).unwrap() {
        let column = |name: &str| batch.column_by_name(name).unwrap().clone();
        let names = column("name");
        let names = names.as_any().downcast_ref::<StringArray>().unwrap();
        let origins = column("origin");
        let origins = origins.as_any().downcast_ref::<StringArray>().unwrap();
        let values = column("value");
        let values = values.as_any().downcast_ref::<Float64Array>().unwrap();
        let timestamps = column("timestamp");
        let timestamps = timestamps
            .as_any()
            .downcast_ref::<TimestampNanosecondArray>()
            .unwrap();
        for i in 0..batch.num_rows() {
            rows.push((
                names.value(i).to_string(),
                origins.value(i).to_string(),
                values.value(i),
                timestamps.value(i),
            ));
        }
    }
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    rows
}

fn fixture_service() -> String {
    wire::decode(include_bytes!("fixtures/relay-batch.bin"), 1 << 20)
        .unwrap()
        .groups[0]
        .source
        .service
        .clone()
}

#[test]
fn reports_become_points_stamped_with_report_time_in_a_sealed_series_file() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(config(dir.path())).unwrap();
    core.journal
        .accept(&batch(&[
            Some(report(
                json!({"offline_detection_runs": 10, "presence_interest_reports": 1}),
                json!({"queue_depth": 7.5}),
            )),
            None,
            Some(json!({"SchemaVersion": 3, "Counters": {"runs": 1}})),
        ]))
        .unwrap();
    wait(|| !core.snapshot().series_files.is_empty());
    let snapshot = core.snapshot();
    assert_eq!(snapshot.series_files.len(), 1);
    let file = &snapshot.series_files[0];
    assert_eq!(file.meta.rows, 3);
    assert_eq!(file.meta.min_timestamp, REPORT_NANOS);
    assert_eq!(file.meta.max_timestamp, REPORT_NANOS);
    assert!(file.meta.path.starts_with("metrics/"));
    assert_eq!(
        read_series(&file.path),
        vec![
            (
                "offline_detection_runs".into(),
                "counter".into(),
                10.0,
                REPORT_NANOS
            ),
            (
                "presence_interest_reports".into(),
                "counter".into(),
                1.0,
                REPORT_NANOS
            ),
            ("queue_depth".into(), "gauge".into(), 7.5, REPORT_NANOS),
        ]
    );
    drop(snapshot);
    core.shutdown().unwrap();
    let names = Catalog::open(&dir.path().join("catalog.sqlite"))
        .unwrap()
        .series_names()
        .unwrap();
    let service = fixture_service();
    let name = |name: &str, origin: &str| SeriesName {
        service: service.clone(),
        name: name.into(),
        origin: origin.into(),
        first_seen: REPORT_NANOS,
        last_seen: REPORT_NANOS,
    };
    assert_eq!(
        names,
        vec![
            name("offline_detection_runs", "counter"),
            name("presence_interest_reports", "counter"),
            name("queue_depth", "gauge"),
        ]
    );
}

#[test]
fn series_file_survives_restart_with_the_same_rows() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let core = Core::open(cfg.clone()).unwrap();
    core.journal
        .accept(&batch(&[
            Some(report(json!({"runs": 10}), json!({}))),
            Some(report(json!({"runs": 11}), json!({}))),
        ]))
        .unwrap();
    wait(|| !core.snapshot().series_files.is_empty());
    let before = core.snapshot().series_files[0].clone();
    let rows = read_series(&before.path);
    core.shutdown().unwrap();
    drop(core);
    let core = Core::open(cfg).unwrap();
    let after = core.snapshot();
    assert_eq!(after.series_files.len(), 1);
    assert_eq!(after.series_files[0].meta.id, before.meta.id);
    assert_eq!(after.series_files[0].path, before.path);
    assert_eq!(read_series(&after.series_files[0].path), rows);
    assert_eq!(rows.len(), 2);
    core.shutdown().unwrap();
}

#[test]
fn log_retention_deletes_segment_but_keeps_series_file() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config(dir.path());
    cfg.segment_rows = 1;
    cfg.retention = Duration::from_millis(500);
    let core = Core::open(cfg.clone()).unwrap();
    core.journal
        .accept(&batch(&[Some(report(json!({"runs": 1}), json!({})))]))
        .unwrap();
    wait(|| !core.snapshot().series_files.is_empty());
    let snapshot = core.snapshot();
    let log_dir = snapshot.closed[0].path.clone();
    let series = snapshot.series_files[0].path.clone();
    drop(snapshot);
    wait(|| !log_dir.exists());
    assert!(series.is_file(), "retention must not touch series files");
    assert_eq!(core.snapshot().series_files.len(), 1);
    core.shutdown().unwrap();
    drop(core);
    let core = Core::open(cfg).unwrap();
    // Retention выполняется не реже раза в секунду; ждём хотя бы один его проход после старта.
    std::thread::sleep(Duration::from_millis(1200));
    assert!(core.snapshot().closed.is_empty());
    assert_eq!(core.snapshot().series_files.len(), 1);
    assert!(series.is_file());
    assert_eq!(read_series(&series).len(), 1);
    core.shutdown().unwrap();
}

#[test]
fn invalid_reports_are_counted_and_ingestion_continues() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(config(dir.path())).unwrap();
    core.journal
        .accept(&batch(&[
            Some(json!({"SchemaVersion": 3, "Counters": {"runs": 1}})),
            Some(report(json!({"ok": 1, "bad": "x"}), json!({}))),
            None,
        ]))
        .unwrap();
    wait(|| core.snapshot().watermark >= 3);
    let metrics = &core.journal.metrics;
    assert_eq!(metrics.series_parse_errors.load(Ordering::Relaxed), 2);
    assert_eq!(metrics.series_points.load(Ordering::Relaxed), 1);
    assert!(core.journal.ready());
    core.journal.accept(&batch(&[None])).unwrap();
    wait(|| core.snapshot().watermark >= 4);
    assert_eq!(metrics.series_parse_errors.load(Ordering::Relaxed), 2);
    core.shutdown().unwrap();
}

#[test]
fn unregistered_series_files_are_removed_on_restart() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let core = Core::open(cfg.clone()).unwrap();
    core.shutdown().unwrap();
    drop(core);
    let day = dir.path().join("metrics/2026/10/13");
    std::fs::create_dir_all(&day).unwrap();
    std::fs::write(day.join("stray.parquet"), b"partial").unwrap();
    std::fs::write(day.join("stray.parquet.tmp"), b"partial").unwrap();
    let core = Core::open(cfg).unwrap();
    assert!(!day.join("stray.parquet").exists());
    assert!(!day.join("stray.parquet.tmp").exists());
    assert!(core.snapshot().series_files.is_empty());
    core.shutdown().unwrap();
}

#[test]
fn series_names_keep_first_and_last_seen_across_segments() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config(dir.path());
    cfg.segment_rows = 1;
    let core = Core::open(cfg).unwrap();
    let later =
        json!({"SchemaVersion": 2, "Counters": {"runs": 1}, "Timestamp": "2026-10-13T16:00:00Z"});
    core.journal.accept(&batch(&[Some(later)])).unwrap();
    wait(|| core.snapshot().series_files.len() == 1);
    core.journal
        .accept(&batch(&[Some(report(json!({"runs": 2}), json!({})))]))
        .unwrap();
    wait(|| core.snapshot().series_files.len() == 2);
    core.shutdown().unwrap();
    let names = Catalog::open(&dir.path().join("catalog.sqlite"))
        .unwrap()
        .series_names()
        .unwrap();
    assert_eq!(
        names,
        vec![SeriesName {
            service: fixture_service(),
            name: "runs".into(),
            origin: "counter".into(),
            first_seen: REPORT_NANOS,
            last_seen: 1_791_907_200_000_000_000,
        }]
    );
}
