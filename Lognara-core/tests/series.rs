use datafusion::arrow::array::{Array, Float64Array, StringArray, TimestampNanosecondArray};
use lognara_core::{
    catalog::{Catalog, SeriesName},
    columns,
    config::Config,
    model::now_nanos,
    series::{self, SeriesFileMeta},
    series_query::{self, QueryRequest, Split},
    storage::Core,
    wire,
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant},
};
use uuid::Uuid;

const REPORT_TIME: &str = "2026-10-13T15:14:00.4480321Z";
const REPORT_NANOS: i64 = 1_791_904_440_448_032_100;

fn config(path: &Path) -> Config {
    let mut cfg = Config::from_lookup(|key| match key {
        "LOGNARA_ADMIN_TOKEN" => Some("admin".into()),
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

/// Отчёт runs с собственным временем: каждая строка пачки уходит в свой сегмент и свой бакет.
fn report_at(time: &str, runs: u64) -> Value {
    json!({"SchemaVersion": 2, "Counters": {"runs": runs}, "Timestamp": time})
}

/// Значения серии runs по минутам 15:10-15:13; сравниваются до и после слияния.
async fn runs_values(core: &Arc<Core>) -> Value {
    let request = QueryRequest {
        service: fixture_service(),
        name: "runs".into(),
        from: "2026-10-13T15:10:00Z".into(),
        to: "2026-10-13T15:13:00Z".into(),
        step_seconds: 60,
        split: Split::None,
        origin: None,
        environment: None,
    };
    let reply = series_query::query(core.clone(), request).await.unwrap();
    serde_json::to_value(reply.series).unwrap()
}

#[tokio::test]
async fn compaction_merges_a_day_and_keeps_query_values() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config(dir.path());
    cfg.segment_rows = 1;
    let core = Core::open(cfg.clone()).unwrap();
    core.journal
        .accept(&batch(&[
            Some(report_at("2026-10-13T15:10:10Z", 1)),
            Some(report_at("2026-10-13T15:11:10Z", 2)),
            Some(report_at("2026-10-13T15:12:10Z", 4)),
        ]))
        .unwrap();
    wait(|| core.snapshot().series_files.len() == 3);
    let before = runs_values(&core).await;
    assert_eq!(before, json!([{"label": "", "values": [1.0, 2.0, 4.0]}]));
    core.shutdown().unwrap();
    drop(core);
    // Порог в несколько миллисекунд: слияние дня происходит сразу после старта.
    cfg.series_compact_quiet = Duration::from_millis(5);
    let core = Core::open(cfg).unwrap();
    wait(|| core.snapshot().series_files.len() == 1);
    let merged = core.snapshot().series_files[0].clone();
    assert_eq!(merged.meta.rows, 3);
    assert_eq!(read_series(&merged.path).len(), 3);
    assert_eq!(runs_values(&core).await, before);
    core.shutdown().unwrap();
}

#[tokio::test]
async fn snapshot_taken_before_compaction_keeps_source_files_until_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config(dir.path());
    cfg.segment_rows = 1;
    cfg.series_compact_quiet = Duration::from_secs(1);
    let core = Core::open(cfg).unwrap();
    core.journal
        .accept(&batch(&[
            Some(report_at("2026-10-13T15:10:10Z", 1)),
            Some(report_at("2026-10-13T15:11:10Z", 2)),
        ]))
        .unwrap();
    wait(|| core.snapshot().series_files.len() == 2);
    let held = core.snapshot();
    let sources: Vec<PathBuf> = held
        .series_files
        .iter()
        .map(|file| file.path.clone())
        .collect();
    wait(|| core.snapshot().series_files.len() == 1);
    // Слияние уже опубликовано, но держатель старого снимка не даёт удалить исходники.
    std::thread::sleep(Duration::from_millis(1500));
    assert!(sources.iter().all(|path| path.is_file()));
    drop(held);
    wait(|| sources.iter().all(|path| !path.exists()));
    core.shutdown().unwrap();
}

#[tokio::test]
async fn restart_after_compaction_registers_only_the_merged_file() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config(dir.path());
    cfg.segment_rows = 1;
    cfg.series_compact_quiet = Duration::from_millis(5);
    let core = Core::open(cfg.clone()).unwrap();
    core.journal
        .accept(&batch(&[
            Some(report_at("2026-10-13T15:10:10Z", 1)),
            Some(report_at("2026-10-13T15:11:10Z", 2)),
        ]))
        .unwrap();
    wait(|| {
        let snapshot = core.snapshot();
        snapshot.series_files.len() == 1 && snapshot.series_files[0].meta.rows == 2
    });
    let merged = core.snapshot().series_files[0].clone();
    let day = dir
        .path()
        .join(merged.meta.path.rsplit_once('/').unwrap().0);
    wait(|| std::fs::read_dir(&day).unwrap().count() == 1);
    core.shutdown().unwrap();
    drop(core);
    let core = Core::open(cfg).unwrap();
    let after = core.snapshot();
    assert_eq!(after.series_files.len(), 1);
    assert_eq!(after.series_files[0].meta.id, merged.meta.id);
    assert_eq!(read_series(&after.series_files[0].path).len(), 2);
    assert_eq!(std::fs::read_dir(&day).unwrap().count(), 1);
    core.shutdown().unwrap();
    drop(core);
    let catalog = Catalog::open(&dir.path().join("catalog.sqlite")).unwrap();
    assert!(catalog.series_files("deleting").unwrap().is_empty());
    let ready = catalog.series_files("ready").unwrap();
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].id, merged.meta.id);
}

/// Два закрытых сегмента с точками; порог слияния по умолчанию, поэтому слияния не будет.
fn seal_two_series(cfg: &Config) {
    let core = Core::open(cfg.clone()).unwrap();
    core.journal
        .accept(&batch(&[
            Some(report_at("2026-10-13T15:10:10Z", 1)),
            Some(report_at("2026-10-13T15:11:10Z", 2)),
        ]))
        .unwrap();
    wait(|| core.snapshot().series_files.len() == 2);
    core.shutdown().unwrap();
}

/// Записывает слитый файл дня без регистрации в каталоге, как после обрыва до mark.
fn write_merged(dir: &Path, sources: &[SeriesFileMeta]) -> SeriesFileMeta {
    let day = sources[0].path.rsplit_once('/').unwrap().0;
    let id = Uuid::now_v7().to_string();
    let path = format!("{day}/{id}.parquet");
    let batches: Vec<_> = sources
        .iter()
        .flat_map(|meta| columns::read_all(&dir.join(&meta.path)).unwrap())
        .collect();
    columns::write_parquet_with(&dir.join(&path), series::schema(), &batches, "unused").unwrap();
    SeriesFileMeta {
        version: 1,
        id,
        path,
        rows: sources.iter().map(|meta| meta.rows).sum(),
        min_timestamp: sources.iter().map(|meta| meta.min_timestamp).min().unwrap(),
        max_timestamp: sources.iter().map(|meta| meta.max_timestamp).max().unwrap(),
        closed_at: now_nanos(),
    }
}

#[test]
fn crash_before_mark_removes_the_unregistered_merged_file() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config(dir.path());
    cfg.segment_rows = 1;
    seal_two_series(&cfg);
    let sources = Catalog::open(&dir.path().join("catalog.sqlite"))
        .unwrap()
        .series_files("ready")
        .unwrap();
    // Состояние после series_compact_after_parquet: слитый файл есть, в каталоге его нет.
    let merged = write_merged(dir.path(), &sources);
    let core = Core::open(cfg).unwrap();
    let snapshot = core.snapshot();
    assert_eq!(snapshot.series_files.len(), 2);
    assert!(!dir.path().join(&merged.path).exists());
    assert!(
        sources
            .iter()
            .all(|meta| dir.path().join(&meta.path).is_file())
    );
    drop(snapshot);
    core.shutdown().unwrap();
}

#[test]
fn crash_after_mark_keeps_only_the_merged_file_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config(dir.path());
    cfg.segment_rows = 1;
    seal_two_series(&cfg);
    let mut catalog = Catalog::open(&dir.path().join("catalog.sqlite")).unwrap();
    let sources = catalog.series_files("ready").unwrap();
    let merged = write_merged(dir.path(), &sources);
    // Состояние после series_compact_after_mark: слитый файл ready, исходные deleting.
    let ids: Vec<String> = sources.iter().map(|meta| meta.id.clone()).collect();
    catalog.replace_series(&merged, &ids).unwrap();
    drop(catalog);
    let core = Core::open(cfg).unwrap();
    let snapshot = core.snapshot();
    assert_eq!(snapshot.series_files.len(), 1);
    assert_eq!(snapshot.series_files[0].meta.id, merged.id);
    assert_eq!(read_series(&snapshot.series_files[0].path).len(), 2);
    assert!(
        sources
            .iter()
            .all(|meta| !dir.path().join(&meta.path).exists())
    );
    drop(snapshot);
    core.shutdown().unwrap();
    drop(core);
    let catalog = Catalog::open(&dir.path().join("catalog.sqlite")).unwrap();
    assert!(catalog.series_files("deleting").unwrap().is_empty());
}
