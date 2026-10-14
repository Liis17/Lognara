use datafusion::arrow::array::{
    Array, Float64Array, Int64Array, RecordBatch, StringArray, TimestampNanosecondArray,
};
use lognara_core::{
    catalog::Catalog,
    columns,
    config::Config,
    model::now_nanos,
    rollup::{self, RollupFile, RollupMeta},
    storage::Core,
    wire,
};
use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

const MIN: i64 = 60 * 1_000_000_000;
const HOUR: i64 = 60 * MIN;
const HOURLY: u32 = 3600;
const HALF_DAY: u32 = 43_200;

fn config(path: &Path, segment_rows: usize) -> Config {
    let mut cfg = Config::from_lookup(|key| match key {
        "LOGNARA_INGEST_TOKEN" => Some("ingest".into()),
        "LOGNARA_QUERY_TOKEN" => Some("query".into()),
        _ => None,
    })
    .unwrap();
    cfg.data_dir = path.into();
    cfg.disk_reserve_bytes = 1;
    cfg.segment_rows = segment_rows;
    cfg.refresh_interval = Duration::from_millis(20);
    cfg.series_rollup_pause = Duration::from_millis(1);
    cfg
}

/// Полночь UTC дня, который был days дней назад.
fn midnight_days_ago(days: i64) -> i64 {
    (OffsetDateTime::now_utc() - time::Duration::days(days))
        .date()
        .midnight()
        .assume_utc()
        .unix_timestamp_nanos() as i64
}

fn day_key(nanos: i64) -> String {
    let date = OffsetDateTime::from_unix_timestamp_nanos(nanos as i128)
        .unwrap()
        .date();
    format!(
        "{:04}-{:02}-{:02}",
        date.year(),
        date.month() as u8,
        date.day()
    )
}

fn stamp(nanos: i64) -> String {
    OffsetDateTime::from_unix_timestamp_nanos(nanos as i128)
        .unwrap()
        .format(&Rfc3339)
        .unwrap()
}

/// Отчёт с одним счётчиком runs во времени отчёта.
fn report(nanos: i64, runs: u64) -> Value {
    json!({"SchemaVersion": 2, "Counters": {"runs": runs}, "Timestamp": stamp(nanos)})
}

/// Пачка из фикстуры: по одной записи на элемент; None - обычный лог без отчёта.
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
        assert!(Instant::now() < deadline, "rollups timed out");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn column<'a, T: Array + 'static>(batch: &'a RecordBatch, name: &str) -> &'a T {
    batch
        .column_by_name(name)
        .unwrap()
        .as_any()
        .downcast_ref::<T>()
        .unwrap()
}

#[derive(Debug, Clone, PartialEq)]
struct Row {
    bucket: i64,
    name: String,
    sum: f64,
    count: i64,
    min: f64,
    max: f64,
    last: f64,
    last_ts: i64,
}

fn row(bucket: i64, sum: f64, count: i64, min: f64, max: f64, last: f64, last_ts: i64) -> Row {
    Row {
        bucket,
        name: "runs".into(),
        sum,
        count,
        min,
        max,
        last,
        last_ts,
    }
}

/// Строки агрегата файла по корзине и имени; схема файла совпадает с rollup::schema.
fn read_rollup(path: &Path) -> Vec<Row> {
    let mut rows = vec![];
    for batch in columns::read_all(path).unwrap() {
        assert_eq!(batch.schema(), rollup::schema());
        let buckets = column::<TimestampNanosecondArray>(&batch, "bucket");
        let names = column::<StringArray>(&batch, "name");
        let sums = column::<Float64Array>(&batch, "sum");
        let counts = column::<Int64Array>(&batch, "count");
        let mins = column::<Float64Array>(&batch, "min");
        let maxes = column::<Float64Array>(&batch, "max");
        let lasts = column::<Float64Array>(&batch, "last");
        let last_times = column::<TimestampNanosecondArray>(&batch, "last_ts");
        for i in 0..batch.num_rows() {
            rows.push(Row {
                bucket: buckets.value(i),
                name: names.value(i).into(),
                sum: sums.value(i),
                count: counts.value(i),
                min: mins.value(i),
                max: maxes.value(i),
                last: lasts.value(i),
                last_ts: last_times.value(i),
            });
        }
    }
    rows.sort_by(|a, b| (a.bucket, &a.name).cmp(&(b.bucket, &b.name)));
    rows
}

/// Файл агрегата суток нужной корзины из опубликованного списка.
fn current(core: &Core, day: i64, resolution: u32) -> Option<Arc<RollupFile>> {
    let key = day_key(day);
    core.rollups
        .read()
        .unwrap()
        .iter()
        .find(|file| file.meta.day == key && file.meta.resolution == resolution)
        .cloned()
}

fn rows_of(core: &Core, day: i64, resolution: u32) -> Option<Vec<Row>> {
    current(core, day, resolution).map(|file| read_rollup(&file.path))
}

#[test]
fn hourly_rollup_sums_each_hour_and_counts_a_redelivered_report_once() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(config(dir.path(), 1)).unwrap();
    let day = midnight_days_ago(10);
    core.journal
        .accept(&batch(&[
            Some(report(day + HOUR + 10 * MIN, 1)),
            Some(report(day + HOUR + 20 * MIN, 3)),
        ]))
        .unwrap();
    // Тот же отчёт в другом теле WAL: отдельный сегмент и отдельный файл серий.
    core.journal
        .accept(&batch(&[None, Some(report(day + HOUR + 20 * MIN, 3))]))
        .unwrap();
    core.journal
        .accept(&batch(&[Some(report(day + 5 * HOUR + 30 * MIN, 2))]))
        .unwrap();
    let expected = vec![
        row(day + HOUR, 4.0, 2, 1.0, 3.0, 3.0, day + HOUR + 20 * MIN),
        row(
            day + 5 * HOUR,
            2.0,
            1,
            2.0,
            2.0,
            2.0,
            day + 5 * HOUR + 30 * MIN,
        ),
    ];
    wait(|| rows_of(&core, day, HOURLY) == Some(expected.clone()));
    assert_eq!(core.snapshot().series_files.len(), 4);
    assert!(current(&core, day, HALF_DAY).is_none());
    core.shutdown().unwrap();
}

#[test]
fn days_older_than_the_hourly_window_get_twelve_hour_buckets() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(config(dir.path(), 1)).unwrap();
    let day = midnight_days_ago(40);
    core.journal
        .accept(&batch(&[
            Some(report(day + 30 * MIN, 1)),
            Some(report(day + 13 * HOUR, 2)),
            Some(report(day + 13 * HOUR + 30 * MIN, 3)),
        ]))
        .unwrap();
    let expected = vec![
        row(day, 1.0, 1, 1.0, 1.0, 1.0, day + 30 * MIN),
        row(
            day + 12 * HOUR,
            5.0,
            2,
            2.0,
            3.0,
            3.0,
            day + 13 * HOUR + 30 * MIN,
        ),
    ];
    wait(|| rows_of(&core, day, HALF_DAY) == Some(expected.clone()));
    assert!(current(&core, day, HOURLY).is_none());
    core.shutdown().unwrap();
}

#[test]
fn late_point_makes_the_day_stale_and_the_old_file_waits_for_its_readers() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(config(dir.path(), 1)).unwrap();
    let day = midnight_days_ago(10);
    core.journal
        .accept(&batch(&[Some(report(day + 2 * HOUR, 1))]))
        .unwrap();
    wait(|| {
        rows_of(&core, day, HOURLY)
            == Some(vec![row(
                day + 2 * HOUR,
                1.0,
                1,
                1.0,
                1.0,
                1.0,
                day + 2 * HOUR,
            )])
    });
    let old_list = core.rollups.read().unwrap().clone();
    let old_path = current(&core, day, HOURLY).unwrap().path.clone();
    core.journal
        .accept(&batch(&[Some(report(day + 2 * HOUR + 30 * MIN, 4))]))
        .unwrap();
    wait(|| {
        rows_of(&core, day, HOURLY)
            == Some(vec![row(
                day + 2 * HOUR,
                5.0,
                2,
                1.0,
                4.0,
                4.0,
                day + 2 * HOUR + 30 * MIN,
            )])
    });
    assert!(old_path.is_file(), "a held list keeps the replaced file");
    drop(old_list);
    wait(|| !old_path.exists());
    core.shutdown().unwrap();
}

#[test]
fn restart_with_shorter_hourly_window_replaces_the_hourly_rollup() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path(), 1);
    let day = midnight_days_ago(20);
    let core = Core::open(cfg.clone()).unwrap();
    core.journal
        .accept(&batch(&[
            Some(report(day + HOUR + 10 * MIN, 1)),
            Some(report(day + 5 * HOUR + 30 * MIN, 2)),
        ]))
        .unwrap();
    wait(|| {
        rows_of(&core, day, HOURLY)
            == Some(vec![
                row(day + HOUR, 1.0, 1, 1.0, 1.0, 1.0, day + HOUR + 10 * MIN),
                row(
                    day + 5 * HOUR,
                    2.0,
                    1,
                    2.0,
                    2.0,
                    2.0,
                    day + 5 * HOUR + 30 * MIN,
                ),
            ])
    });
    let hourly = current(&core, day, HOURLY).unwrap().path.clone();
    core.shutdown().unwrap();
    drop(core);
    let mut cfg = cfg;
    cfg.series_hourly_days = 10;
    let core = Core::open(cfg).unwrap();
    wait(|| {
        rows_of(&core, day, HALF_DAY)
            == Some(vec![row(
                day,
                3.0,
                2,
                1.0,
                2.0,
                2.0,
                day + 5 * HOUR + 30 * MIN,
            )])
    });
    wait(|| !hourly.exists());
    assert!(current(&core, day, HOURLY).is_none());
    core.shutdown().unwrap();
    drop(core);
    let catalog = Catalog::open(&dir.path().join("catalog.sqlite")).unwrap();
    assert!(catalog.rollups("deleting").unwrap().is_empty());
    assert_eq!(catalog.rollups("ready").unwrap().len(), 1);
}

#[test]
fn nightly_pass_rebuilds_existing_rollups_with_the_same_values() {
    let dir = tempfile::tempdir().unwrap();
    let day = midnight_days_ago(10);
    let expected = vec![row(
        day + HOUR,
        1.0,
        1,
        1.0,
        1.0,
        1.0,
        day + HOUR + 10 * MIN,
    )];
    let core = Core::open(config(dir.path(), 1)).unwrap();
    core.journal
        .accept(&batch(&[Some(report(day + HOUR + 10 * MIN, 1))]))
        .unwrap();
    wait(|| rows_of(&core, day, HOURLY) == Some(expected.clone()));
    let before = current(&core, day, HOURLY).unwrap();
    let (before_id, before_built) = (before.meta.id.clone(), before.meta.built_at);
    drop(before);
    core.shutdown().unwrap();
    drop(core);
    let mut cfg = config(dir.path(), 1);
    cfg.series_rollup_hour_utc = OffsetDateTime::now_utc().hour();
    let core = Core::open(cfg).unwrap();
    wait(|| current(&core, day, HOURLY).is_some_and(|file| file.meta.id != before_id));
    let after = current(&core, day, HOURLY).unwrap();
    assert!(after.meta.built_at > before_built);
    assert_eq!(rows_of(&core, day, HOURLY), Some(expected));
    core.shutdown().unwrap();
}

#[test]
fn restart_removes_stray_files_and_finishes_deleting_rollups() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path(), 1);
    let core = Core::open(cfg.clone()).unwrap();
    core.shutdown().unwrap();
    drop(core);
    let stray_dir = dir.path().join("rollups/1h/2026/01/02");
    fs::create_dir_all(&stray_dir).unwrap();
    fs::write(stray_dir.join("stray.parquet"), b"partial").unwrap();
    fs::write(stray_dir.join("stray.parquet.tmp"), b"partial").unwrap();
    // Файл уже на месте, а строка осталась deleting: замена прервалась после каталога.
    let meta = RollupMeta {
        id: Uuid::now_v7().to_string(),
        day: "2026-01-03".into(),
        resolution: HALF_DAY,
        path: format!("rollups/12h/2026/01/03/{}.parquet", Uuid::now_v7()),
        fingerprint: "unused".into(),
        built_at: now_nanos(),
    };
    let deleting = dir.path().join(&meta.path);
    fs::create_dir_all(deleting.parent().unwrap()).unwrap();
    fs::write(&deleting, b"old").unwrap();
    let mut catalog = Catalog::open(&dir.path().join("catalog.sqlite")).unwrap();
    catalog.replace_rollups(Some(&meta), &[]).unwrap();
    catalog
        .replace_rollups(None, std::slice::from_ref(&meta.id))
        .unwrap();
    drop(catalog);
    let core = Core::open(cfg).unwrap();
    wait(|| {
        !stray_dir.join("stray.parquet").exists()
            && !stray_dir.join("stray.parquet.tmp").exists()
            && !deleting.exists()
    });
    core.shutdown().unwrap();
    drop(core);
    let catalog = Catalog::open(&dir.path().join("catalog.sqlite")).unwrap();
    assert!(catalog.rollups("deleting").unwrap().is_empty());
    assert!(catalog.rollups("ready").unwrap().is_empty());
}

#[test]
fn ready_rollup_without_its_file_is_dropped_and_rebuilt_on_start() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path(), 1);
    let day = midnight_days_ago(10);
    let expected = vec![row(
        day + HOUR,
        1.0,
        1,
        1.0,
        1.0,
        1.0,
        day + HOUR + 10 * MIN,
    )];
    let core = Core::open(cfg.clone()).unwrap();
    core.journal
        .accept(&batch(&[Some(report(day + HOUR + 10 * MIN, 1))]))
        .unwrap();
    wait(|| rows_of(&core, day, HOURLY) == Some(expected.clone()));
    let before: Arc<RollupFile> = current(&core, day, HOURLY).unwrap();
    let (old_id, old_path) = (before.meta.id.clone(), before.path.clone());
    drop(before);
    core.shutdown().unwrap();
    drop(core);
    fs::remove_file(&old_path).unwrap();
    let core = Core::open(cfg).unwrap();
    wait(|| current(&core, day, HOURLY).is_some_and(|file| file.meta.id != old_id));
    assert_eq!(rows_of(&core, day, HOURLY), Some(expected));
    core.shutdown().unwrap();
    drop(core);
    let catalog = Catalog::open(&dir.path().join("catalog.sqlite")).unwrap();
    let ready = catalog.rollups("ready").unwrap();
    assert_eq!(ready.len(), 1);
    assert_ne!(ready[0].id, old_id);
    assert!(catalog.rollups("deleting").unwrap().is_empty());
}

#[test]
fn points_from_the_last_seven_days_get_no_rollup() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(config(dir.path(), 1)).unwrap();
    let old = midnight_days_ago(10);
    core.journal
        .accept(&batch(&[Some(report(now_nanos(), 1))]))
        .unwrap();
    core.journal
        .accept(&batch(&[Some(report(old + HOUR, 1))]))
        .unwrap();
    wait(|| rows_of(&core, old, HOURLY).is_some());
    // Сегмент с сегодняшними точками уже запечатан и обработан: проверяем, что агрегата нет.
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(core.rollups.read().unwrap().len(), 1);
    assert!(current(&core, now_nanos(), HOURLY).is_none());
    core.shutdown().unwrap();
}
