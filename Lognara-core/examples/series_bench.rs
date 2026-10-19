//! Стенд графиков метрик: год файлов серий и время series_query::query. Данные во временном каталоге.
use std::{
    fs,
    path::Path,
    time::{Duration, Instant},
};

use anyhow::{Result, ensure};
use lognara_core::{
    catalog::{Catalog, SeriesName},
    columns,
    config::Config,
    series::{self, Origin, Point, SeriesFileMeta},
    series_query::{self, QueryRequest, Split},
    storage::Core,
    wire::Source,
};
use time::OffsetDateTime;
use uuid::Uuid;

const DAYS: i64 = 365;
/// 2025-10-13T00:00:00Z; последний день заканчивается 2026-10-13T00:00:00Z.
const START_SECONDS: i64 = 1_760_313_600;
const DAY_SECONDS: i64 = 86_400;
const POINT_SECONDS: i64 = 10;
const SERVICES: [&str; 3] = ["api", "worker", "web"];
const COUNTERS: [&str; 3] = ["requests", "errors", "jobs"];
const INSTANCES: [&str; 2] = ["eu-1", "eu-2"];

fn source(service: &str, instance: &str) -> Source {
    Source {
        environment: Some("production".into()),
        server: "bench-01".into(),
        backend: "barkcloud".into(),
        service: service.into(),
        service_instance: Some(instance.into()),
    }
}

/// Один файл серий на день; точка каждые 10 секунд у каждой пары сервис-счётчик-экземпляр.
fn write_days(data: &Path) -> Result<Vec<SeriesFileMeta>> {
    let mut files = vec![];
    for day in 0..DAYS {
        let day_start = START_SECONDS + day * DAY_SECONDS;
        let mut points = Vec::new();
        for sample in 0..DAY_SECONDS / POINT_SECONDS {
            let timestamp = (day_start + sample * POINT_SECONDS) * 1_000_000_000;
            for service in SERVICES {
                for instance in INSTANCES {
                    for name in COUNTERS {
                        points.push(Point {
                            timestamp,
                            source: source(service, instance),
                            name: name.into(),
                            origin: Origin::Counter,
                            rule_id: None,
                            labels: String::new(),
                            value: (sample % 5 + 1) as f64,
                            event_id: None,
                        });
                    }
                }
            }
        }
        let date = OffsetDateTime::from_unix_timestamp(day_start)?;
        let id = Uuid::now_v7().to_string();
        let relative = format!(
            "metrics/{:04}/{:02}/{:02}/{id}.parquet",
            date.year(),
            date.month() as u8,
            date.day()
        );
        let path = data.join(&relative);
        fs::create_dir_all(path.parent().unwrap())?;
        let batch = series::encode(&points)?;
        columns::write_parquet_with(&path, series::schema(), &[batch], "series_bench")?;
        let first = points.first().unwrap().timestamp;
        let last = points.last().unwrap().timestamp;
        files.push(SeriesFileMeta {
            version: 1,
            id,
            path: relative,
            rows: points.len(),
            min_timestamp: first,
            max_timestamp: last,
            closed_at: last,
        });
    }
    Ok(files)
}

/// Каталог получает файлы и имена серий так же, как материализатор после seal.
fn register(data: &Path, files: &[SeriesFileMeta]) -> Result<()> {
    let mut catalog = Catalog::open(&data.join("catalog.sqlite"))?;
    let (checkpoint, next_batch, next_sequence) = catalog.progress()?;
    let (first, last) = (
        files.first().unwrap().min_timestamp,
        files.last().unwrap().max_timestamp,
    );
    let names: Vec<SeriesName> = SERVICES
        .iter()
        .flat_map(|service| COUNTERS.iter().map(move |name| (*service, *name)))
        .map(|(service, name)| SeriesName {
            service: service.into(),
            name: name.into(),
            origin: "counter".into(),
            first_seen: first,
            last_seen: last,
        })
        .collect();
    catalog.publish(
        None,
        None,
        &names,
        &[],
        checkpoint,
        &[],
        (next_batch, next_sequence),
    )?;
    for file in files {
        catalog.publish(
            None,
            Some(file),
            &[],
            &[],
            checkpoint,
            &[],
            (next_batch, next_sequence),
        )?;
    }
    Ok(())
}

/// Суток старше недельного порога LOGNARA_SERIES_RAW_DAYS по умолчанию: им положен агрегат.
fn eligible_days() -> usize {
    let cutoff = OffsetDateTime::now_utc().unix_timestamp() - 7 * DAY_SECONDS;
    (0..DAYS)
        .filter(|day| START_SECONDS + (day + 1) * DAY_SECONDS <= cutoff)
        .count()
}

fn dir_bytes(path: &Path) -> Result<u64> {
    let mut total = 0;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let meta = entry.metadata()?;
        total += if meta.is_dir() {
            dir_bytes(&entry.path())?
        } else {
            meta.len()
        };
    }
    Ok(total)
}

#[tokio::main]
async fn main() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let started = Instant::now();
    let files = write_days(dir.path())?;
    register(dir.path(), &files)?;
    let rows: usize = files.iter().map(|file| file.rows).sum();
    println!(
        "scale: {} services x {} counters x {} instances, one point per {POINT_SECONDS} s, {DAYS} days",
        SERVICES.len(),
        COUNTERS.len(),
        INSTANCES.len()
    );
    println!("build: {:.1} s", started.elapsed().as_secs_f64());
    println!("rows (points): {rows}");
    println!("files: {}", files.len());
    println!(
        "metrics/ size: {:.1} MiB",
        dir_bytes(&dir.path().join("metrics"))? as f64 / (1024.0 * 1024.0)
    );

    let mut config = Config::from_lookup(|key| match key {
        "LOGNARA_ADMIN_TOKEN" => Some("admin".into()),
        "LOGNARA_QUERY_TOKEN" => Some("query".into()),
        _ => None,
    })?;
    config.data_dir = dir.path().into();
    // Пауза по умолчанию растянула бы стенд на минуты; время ниже - только построение агрегатов.
    config.series_rollup_pause = Duration::from_millis(1);
    let eligible = eligible_days();
    let started = Instant::now();
    let core = Core::open(config)?;
    while core.rollups.read().unwrap().len() < eligible {
        ensure!(
            started.elapsed() < Duration::from_secs(3600),
            "rollups were not built within an hour"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let rollups = core.rollups.read().unwrap().clone();
    let hourly = rollups
        .iter()
        .filter(|file| file.meta.resolution == 3600)
        .count();
    println!(
        "rollups: {} days ({hourly} hourly, {} 12-hour) built in {:.1} s",
        rollups.len(),
        rollups.len() - hourly,
        started.elapsed().as_secs_f64()
    );
    println!(
        "rollups/ size: {:.1} MiB",
        dir_bytes(&dir.path().join("rollups"))? as f64 / (1024.0 * 1024.0)
    );
    let cases = [
        (
            "1 year, step 1 day",
            "2025-10-13T00:00:00Z",
            "2026-10-13T00:00:00Z",
            86_400,
            Split::None,
        ),
        (
            "30 days, step 1 hour",
            "2026-09-13T00:00:00Z",
            "2026-10-13T00:00:00Z",
            3_600,
            Split::None,
        ),
        (
            "24 hours, step 1 minute",
            "2026-10-12T00:00:00Z",
            "2026-10-13T00:00:00Z",
            60,
            Split::None,
        ),
        (
            "1 year, step 1 day, split instance",
            "2025-10-13T00:00:00Z",
            "2026-10-13T00:00:00Z",
            86_400,
            Split::Instance,
        ),
    ];
    for (label, from, to, step_seconds, split) in cases {
        let mut timings = vec![];
        let mut shape = String::new();
        for _ in 0..3 {
            let request = QueryRequest {
                server: None,
                service: "api".into(),
                name: "requests".into(),
                from: from.into(),
                to: to.into(),
                step_seconds,
                split,
                origin: None,
                environment: None,
            };
            let started = Instant::now();
            let reply = series_query::query(core.clone(), request).await?;
            timings.push(format!("{:.1}", started.elapsed().as_secs_f64() * 1000.0));
            let values: usize = reply
                .series
                .iter()
                .map(|line| line.values.iter().flatten().count())
                .sum();
            shape = format!(
                "series={} buckets={} values={values}",
                reply.series.len(),
                reply.buckets.len()
            );
        }
        println!("{label}: {shape} ms=[{}]", timings.join(", "));
    }
    core.shutdown()?;
    drop(core);
    dir.close()?;
    Ok(())
}
