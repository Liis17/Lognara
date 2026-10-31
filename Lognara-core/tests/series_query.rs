use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use lognara_core::{
    api,
    config::Config,
    model::parse_timestamp,
    storage::Core,
    wire::{self, Event, Group, LogId, LogLevel, Source},
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs,
    path::Path,
    time::{Duration, Instant},
};
use time::{Date, OffsetDateTime, format_description::well_known::Rfc3339};
use tower::ServiceExt;
use uuid::Uuid;

const FIXTURE: &[u8] = include_bytes!("fixtures/relay-batch.bin");
const DEFINITIONS: &str = "/v1/admin/series/definitions";
const LIST: &str = "/v1/series";
const QUERY: &str = "/v1/series/query";
const ALERT_QUERY: &str = "/v1/series/alerts/query";
const FROM: &str = "2026-10-13T10:00:00Z";
const TO: &str = "2026-10-13T10:01:00Z";
const HOURLY: u32 = 3600;
const HALF_DAY: u32 = 43_200;
const WEEK: i64 = 604_800;

fn config(path: &Path, segment_rows: usize) -> Config {
    let mut cfg = Config::from_lookup(|key| match key {
        "LOGNARA_QUERY_TOKEN" => Some("query".into()),
        "LOGNARA_ADMIN_TOKEN" => Some("admin".into()),
        _ => None,
    })
    .unwrap();
    cfg.data_dir = path.into();
    cfg.disk_reserve_bytes = 1;
    cfg.segment_rows = segment_rows;
    cfg.refresh_interval = Duration::from_millis(20);
    cfg
}

/// Ответ без JSON-тела (204, текст 401) возвращается как Null.
async fn send(
    router: &Router,
    method: &str,
    uri: &str,
    token: Option<&str>,
    mut body: Option<Value>,
) -> (StatusCode, Value) {
    if uri.starts_with(DEFINITIONS)
        && matches!(method, "POST" | "PUT")
        && let Some(Value::Object(value)) = body.as_mut()
    {
        value
            .entry("server")
            .or_insert(json!(source("api", None).server));
    }
    let mut request = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let body = match body {
        Some(body) => {
            request = request.header("content-type", "application/json");
            Body::from(body.to_string())
        }
        None => Body::empty(),
    };
    let response = router
        .clone()
        .oneshot(request.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn source(service: &str, instance: Option<&str>) -> Source {
    Source {
        environment: Some("production".into()),
        server: "eu-prod-01".into(),
        backend: "barkcloud".into(),
        service: service.into(),
        service_instance: instance.map(Into::into),
    }
}

fn source_in_environment(service: &str, instance: Option<&str>, environment: &str) -> Source {
    Source {
        environment: Some(environment.into()),
        server: "eu-prod-01".into(),
        backend: "barkcloud".into(),
        service: service.into(),
        service_instance: instance.map(Into::into),
    }
}

fn event(id: u128, timestamp: i64, message: &str) -> Event {
    Event {
        id: LogId(Uuid::from_u128(id)),
        timestamp,
        ingested_at: timestamp,
        action: None,
        level: LogLevel::Info,
        message: message.into(),
        trace_id: None,
        span_id: None,
        parent_span_id: None,
        request_id: None,
        attributes: HashMap::new(),
    }
}

/// Событие с отчётом ServiceMetrics; время отчёта становится временем точек.
fn report(id: u128, timestamp: &str, counters: Value, gauges: Value) -> Event {
    let mut event = event(id, parse_timestamp(timestamp).unwrap(), "ServiceMetrics");
    event.attributes.insert(
        "Metrics".into(),
        json!({"SchemaVersion": 2, "Counters": counters, "Gauges": gauges, "Timestamp": timestamp}),
    );
    event
}

/// Пачка из фикстуры с заданными источниками; каждая группа несёт свои события.
fn batch(groups: Vec<(Source, Vec<Event>)>) -> Vec<u8> {
    let mut batch = wire::decode(FIXTURE, 1 << 20).unwrap();
    batch.groups = groups
        .into_iter()
        .map(|(source, events)| Group {
            source,
            dropped: 0,
            events,
        })
        .collect();
    zstd::encode_all(&rmp_serde::to_vec_named(&batch).unwrap()[..], 3).unwrap()
}

fn wait(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(Instant::now() < deadline, "series query timed out");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Тело запроса одной серии сервиса api с суммой по умолчанию.
fn query_body(name: &str, from: &str, to: &str, step: u64) -> Value {
    json!({"service": "api", "name": name, "from": from, "to": to, "step_seconds": step})
}

#[tokio::test]
async fn duplicate_deliveries_of_one_report_are_counted_once() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(config(dir.path(), 1000)).unwrap();
    let router = api::router(core.clone());
    // Один отчёт в двух разных пачках: другие event id, те же источник, время и значения.
    for id in [1, 2] {
        core.journal
            .accept(&batch(vec![(
                source("api", Some("eu-1")),
                vec![report(
                    id,
                    "2026-10-13T10:00:10Z",
                    json!({"runs": 10}),
                    json!({}),
                )],
            )]))
            .unwrap();
    }
    wait(|| core.snapshot().watermark >= 2);
    let (status, body) = send(
        &router,
        "POST",
        QUERY,
        Some("query"),
        Some(query_body("runs", FROM, TO, 60)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["series"], json!([{"label": "", "values": [10.0]}]));
    core.shutdown().unwrap();
}

#[tokio::test]
async fn alert_query_uses_exact_raw_windows_baselines_and_selected_environment() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(config(dir.path(), 1000)).unwrap();
    let router = api::router(core.clone());
    let now = OffsetDateTime::now_utc();
    let ago = |seconds: i64| {
        (now - time::Duration::seconds(seconds))
            .format(&Rfc3339)
            .unwrap()
    };
    let reports = vec![
        report(1, &ago(1000), json!({"requests": 1}), json!({})),
        report(2, &ago(260), json!({"requests": 2}), json!({})),
        report(3, &ago(120), json!({"requests": 4}), json!({})),
        report(4, &ago(30), json!({"requests": 8}), json!({})),
        // A retried native report has a new event ID but the same raw dedup identity.
        report(5, &ago(30), json!({"requests": 8}), json!({})),
    ];
    core.journal
        .accept(&batch(vec![
            (source("api", Some("eu-1")), reports),
            (
                source_in_environment("api", Some("eu-2"), "staging"),
                vec![report(6, &ago(30), json!({"requests": 100}), json!({}))],
            ),
        ]))
        .unwrap();
    wait(|| core.snapshot().watermark >= 6);

    let request = json!({
        "server": "eu-prod-01",
        "service": "api",
        "name": "requests",
        "origin": "counter",
        "environment": "production",
        "windows_seconds": [60, 180, 300]
    });
    // Occupying both UI analytics slots must not block the dedicated alert slot.
    let _first = core.analytics_slots.clone().try_acquire_owned().unwrap();
    let _second = core.analytics_slots.clone().try_acquire_owned().unwrap();
    let (status, reply) = send(
        &router,
        "POST",
        ALERT_QUERY,
        Some("query"),
        Some(request.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply["windows"][0]["count"], 1);
    assert_eq!(reply["windows"][0]["sum"], 8.0);
    assert_eq!(reply["windows"][0]["avg"], 8.0);
    assert_eq!(reply["windows"][0]["min"], 8.0);
    assert_eq!(reply["windows"][0]["max"], 8.0);
    assert_eq!(reply["windows"][0]["increase"], 4.0);
    assert_eq!(reply["windows"][1]["count"], 2);
    assert_eq!(reply["windows"][1]["sum"], 12.0);
    assert_eq!(reply["windows"][1]["avg"], 6.0);
    assert_eq!(reply["windows"][1]["min"], 4.0);
    assert_eq!(reply["windows"][1]["max"], 8.0);
    assert_eq!(reply["windows"][1]["increase"], 6.0);
    assert_eq!(reply["windows"][2]["count"], 3);
    assert_eq!(reply["windows"][2]["sum"], 14.0);
    assert_eq!(reply["windows"][2]["avg"], 14.0 / 3.0);
    assert_eq!(reply["windows"][2]["min"], 2.0);
    assert_eq!(reply["windows"][2]["max"], 8.0);
    assert_eq!(reply["windows"][2]["increase"], 7.0);
    assert!(reply["as_of"].as_str().is_some());
    assert!(reply["last_point_at"].as_str().is_some());

    let mut all_environments = request;
    all_environments
        .as_object_mut()
        .unwrap()
        .remove("environment");
    let (_, all) = send(
        &router,
        "POST",
        ALERT_QUERY,
        Some("query"),
        Some(all_environments.clone()),
    )
    .await;
    assert_eq!(all["windows"][0]["count"], 2);
    assert_eq!(all["windows"][0]["sum"], 108.0);

    let mut nonincrease = all_environments;
    nonincrease["need_increase"] = json!(false);
    let (_, nonincrease) = send(
        &router,
        "POST",
        ALERT_QUERY,
        Some("query"),
        Some(nonincrease),
    )
    .await;
    assert_eq!(nonincrease["windows"][0]["sum"], 108.0);
    assert!(nonincrease["windows"][0]["increase"].is_null());
    assert!(nonincrease["last_point_at"].as_str().is_some());

    let (status, _) = send(
        &router,
        "POST",
        ALERT_QUERY,
        None,
        Some(json!({
            "server": "eu-prod-01", "service": "api", "name": "requests",
            "origin": "counter", "windows_seconds": [60]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    core.shutdown().unwrap();
}

#[tokio::test]
async fn alert_query_uses_production_aggregate_for_parquet_reset_baselines() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(config(dir.path(), 1)).unwrap();
    let router = api::router(core.clone());
    let now = OffsetDateTime::now_utc();
    let ago = |seconds: i64| {
        (now - time::Duration::seconds(seconds))
            .format(&Rfc3339)
            .unwrap()
    };
    let groups = vec![
        (
            source("api", Some("eu-1")),
            vec![
                report(1, &ago(400), json!({"requests": 50}), json!({})),
                report(2, &ago(200), json!({"requests": 70}), json!({})),
                report(3, &ago(90), json!({"requests": 3}), json!({})),
                report(4, &ago(30), json!({"requests": 8}), json!({})),
            ],
        ),
        (
            source("api", Some("singleton")),
            vec![report(5, &ago(20), json!({"singleton": 17}), json!({}))],
        ),
        (
            source("api", Some("multi-a")),
            vec![report(6, &ago(25), json!({"multiple": 4}), json!({}))],
        ),
        (
            source("api", Some("multi-b")),
            vec![report(7, &ago(15), json!({"multiple": 9}), json!({}))],
        ),
        (
            source("api", Some("zero")),
            vec![
                report(8, &ago(40), json!({"zero": 7}), json!({})),
                report(9, &ago(20), json!({"zero": 7}), json!({})),
            ],
        ),
    ];
    core.journal.accept(&batch(groups)).unwrap();
    wait(|| {
        let snapshot = core.snapshot();
        snapshot.watermark >= 4
            && snapshot
                .series_files
                .iter()
                .map(|file| file.meta.rows)
                .sum::<usize>()
                >= 9
    });

    let (status, reply) = send(
        &router,
        "POST",
        ALERT_QUERY,
        Some("query"),
        Some(json!({
            "server": "eu-prod-01", "service": "api", "name": "requests",
            "origin": "counter", "environment": "production", "windows_seconds": [60, 180, 300]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert!(
        core.snapshot()
            .series_files
            .iter()
            .map(|file| file.meta.rows)
            .sum::<usize>()
            >= 4
    );
    assert_eq!(reply["windows"][0]["count"], 1);
    assert_eq!(reply["windows"][0]["increase"], 5.0);
    assert_eq!(reply["windows"][1]["count"], 2);
    assert_eq!(reply["windows"][1]["increase"], 8.0);
    assert_eq!(reply["windows"][2]["count"], 3);
    assert_eq!(reply["windows"][2]["increase"], 28.0);

    for (name, expected_count) in [("singleton", 1), ("multiple", 2)] {
        let (status, reply) = send(
            &router,
            "POST",
            ALERT_QUERY,
            Some("query"),
            Some(json!({
                "server": "eu-prod-01", "service": "api", "name": name,
                "origin": "counter", "environment": "production", "windows_seconds": [60]
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{reply}");
        assert_eq!(reply["windows"][0]["count"], expected_count);
        assert!(reply["windows"][0]["increase"].is_null(), "{reply}");
    }

    let (status, zero) = send(
        &router,
        "POST",
        ALERT_QUERY,
        Some("query"),
        Some(json!({
            "server": "eu-prod-01", "service": "api", "name": "zero",
            "origin": "counter", "environment": "production", "windows_seconds": [60]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{zero}");
    assert_eq!(zero["windows"][0]["count"], 2);
    assert_eq!(zero["windows"][0]["increase"], 0.0);
    core.shutdown().unwrap();
}

#[tokio::test]
async fn alert_query_nonincrease_freshness_is_limited_to_five_minutes() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(config(dir.path(), 1)).unwrap();
    let router = api::router(core.clone());
    let timestamp = (OffsetDateTime::now_utc() - time::Duration::minutes(10))
        .format(&Rfc3339)
        .unwrap();
    core.journal
        .accept(&batch(vec![(
            source("api", Some("eu-1")),
            vec![report(1, &timestamp, json!({"requests": 9}), json!({}))],
        )]))
        .unwrap();
    wait(|| core.snapshot().watermark >= 1 && !core.snapshot().series_files.is_empty());

    let (status, reply) = send(
        &router,
        "POST",
        ALERT_QUERY,
        Some("query"),
        Some(json!({
            "server": "eu-prod-01", "service": "api", "name": "requests",
            "origin": "counter", "windows_seconds": [60], "need_increase": false
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply["windows"][0]["count"], 0);
    assert!(reply["windows"][0]["increase"].is_null());
    assert!(reply["last_point_at"].is_null());
    core.shutdown().unwrap();
}

#[tokio::test]
async fn alert_query_has_no_series_cutoff_and_returns_zero_for_empty_sum_and_count() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(config(dir.path(), 1000)).unwrap();
    let router = api::router(core.clone());
    let now = OffsetDateTime::now_utc();
    let timestamp = (now - time::Duration::seconds(20))
        .format(&Rfc3339)
        .unwrap();
    let groups = (0..25)
        .map(|index| {
            let instance = format!("instance-{index:02}");
            (
                source("api", Some(&instance)),
                vec![report(
                    index as u128 + 1,
                    &timestamp,
                    json!({"requests": 1}),
                    json!({}),
                )],
            )
        })
        .collect();
    core.journal.accept(&batch(groups)).unwrap();
    wait(|| core.snapshot().watermark >= 25);

    let (status, reply) = send(
        &router,
        "POST",
        ALERT_QUERY,
        Some("query"),
        Some(json!({
            "server": "eu-prod-01", "service": "api", "name": "requests",
            "origin": "counter", "windows_seconds": [60]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply["windows"][0]["count"], 25);
    assert_eq!(reply["windows"][0]["sum"], 25.0);

    let (_, empty) = send(
        &router,
        "POST",
        ALERT_QUERY,
        Some("query"),
        Some(json!({
            "server": "eu-prod-01", "service": "api", "name": "not_seen",
            "origin": "counter", "windows_seconds": [60]
        })),
    )
    .await;
    assert_eq!(empty["windows"][0]["count"], 0);
    assert_eq!(empty["windows"][0]["sum"], 0.0);
    assert!(empty["windows"][0]["avg"].is_null());
    assert!(empty["windows"][0]["min"].is_null());
    assert!(empty["windows"][0]["max"].is_null());
    assert!(empty["windows"][0]["increase"].is_null());
    assert!(empty["last_point_at"].is_null());
    core.shutdown().unwrap();
}

#[tokio::test]
async fn seven_day_alert_query_scans_exact_raw_points_within_the_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(config(dir.path(), 2_000)).unwrap();
    let router = api::router(core.clone());
    let now = OffsetDateTime::now_utc();
    const POINTS: usize = 1_008;
    let reports = (0..POINTS)
        .map(|index| {
            let ago = 604_700 - (index as i64 * 600);
            let timestamp = (now - time::Duration::seconds(ago))
                .format(&Rfc3339)
                .unwrap();
            report(
                index as u128 + 1,
                &timestamp,
                json!({"requests": index + 1}),
                json!({}),
            )
        })
        .collect();
    core.journal
        .accept(&batch(vec![(source("api", Some("eu-1")), reports)]))
        .unwrap();
    wait(|| core.snapshot().watermark >= POINTS as u64);

    let started = Instant::now();
    let (status, reply) = send(
        &router,
        "POST",
        ALERT_QUERY,
        Some("query"),
        Some(json!({
            "server": "eu-prod-01", "service": "api", "name": "requests",
            "origin": "counter", "environment": "production", "windows_seconds": [WEEK]
        })),
    )
    .await;
    let elapsed = started.elapsed();
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply["windows"][0]["count"], POINTS);
    assert_eq!(reply["windows"][0]["increase"], (POINTS - 1) as f64);
    assert!(
        elapsed < Duration::from_secs(30),
        "7-day raw query took {elapsed:?}"
    );
    eprintln!("7-day alert query: {POINTS} raw points in {elapsed:?}");
    core.shutdown().unwrap();
}

#[tokio::test]
async fn series_cutoff_reports_the_number_of_truncated_series() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(config(dir.path(), 1000)).unwrap();
    let router = api::router(core.clone());
    let groups = (0..21)
        .map(|index| {
            let instance = format!("instance-{index:02}");
            (
                source("api", Some(&instance)),
                vec![
                    report(
                        index * 2 + 1,
                        "2026-10-13T10:00:10Z",
                        json!({"runs": 0}),
                        json!({}),
                    ),
                    report(
                        index * 2 + 2,
                        "2026-10-13T10:00:40Z",
                        json!({"runs": index + 1}),
                        json!({}),
                    ),
                ],
            )
        })
        .collect();
    core.journal.accept(&batch(groups)).unwrap();
    wait(|| core.snapshot().watermark >= 42);

    let (status, body) = send(
        &router,
        "POST",
        QUERY,
        Some("query"),
        Some(query_with(
            "runs",
            FROM,
            TO,
            60,
            json!({"split":"instance"}),
        )),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["series"].as_array().unwrap().len(), 20);
    assert_eq!(body["truncated"], true);
    assert_eq!(body["truncated_count"], 1);
    core.shutdown().unwrap();
}

#[tokio::test]
async fn delta_sums_instances_and_splits_them_by_instance() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(config(dir.path(), 1000)).unwrap();
    let router = api::router(core.clone());
    core.journal
        .accept(&batch(vec![
            (
                source("api", Some("eu-1")),
                vec![
                    report(1, "2026-10-13T10:00:10Z", json!({"runs": 3}), json!({})),
                    report(2, "2026-10-13T10:00:40Z", json!({"runs": 1}), json!({})),
                ],
            ),
            (
                source("api", Some("eu-2")),
                vec![report(
                    3,
                    "2026-10-13T10:00:20Z",
                    json!({"runs": 5}),
                    json!({}),
                )],
            ),
        ]))
        .unwrap();
    wait(|| core.snapshot().watermark >= 3);
    let mut body = query_body("runs", FROM, TO, 60);
    let (status, reply) = send(&router, "POST", QUERY, Some("query"), Some(body.clone())).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(reply["kind"], "delta");
    assert_eq!(reply["aggregation"], "sum");
    assert_eq!(reply["unit"], "");
    assert_eq!(reply["buckets"], json!([FROM]));
    assert_eq!(reply["series"], json!([{"label": "", "values": [9.0]}]));

    body["split"] = json!("instance");
    let (_, reply) = send(&router, "POST", QUERY, Some("query"), Some(body)).await;
    assert_eq!(
        reply["series"],
        json!([
            {"label": "eu-1", "values": [4.0]},
            {"label": "eu-2", "values": [5.0]}
        ])
    );
    core.shutdown().unwrap();
}

#[tokio::test]
async fn cumulative_definition_turns_counter_values_into_increases() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(config(dir.path(), 1000)).unwrap();
    let router = api::router(core.clone());
    let (status, _) = send(
        &router,
        "POST",
        DEFINITIONS,
        Some("admin"),
        Some(json!({"service": "api", "name": "bytes_sent", "kind": "cumulative", "unit": "B"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    // Значения накопления 10, 20, 5, 15 в четырёх соседних минутах.
    let values = [
        ("2026-10-13T10:00:30Z", 10),
        ("2026-10-13T10:01:30Z", 20),
        ("2026-10-13T10:02:30Z", 5),
        ("2026-10-13T10:03:30Z", 15),
    ];
    let events = values
        .iter()
        .enumerate()
        .map(|(index, (time, value))| {
            report(
                index as u128 + 1,
                time,
                json!({"bytes_sent": value}),
                json!({}),
            )
        })
        .collect();
    core.journal
        .accept(&batch(vec![(source("api", Some("eu-1")), events)]))
        .unwrap();
    wait(|| core.snapshot().watermark >= 4);

    let (status, reply) = send(
        &router,
        "POST",
        QUERY,
        Some("query"),
        Some(query_body("bytes_sent", FROM, "2026-10-13T10:04:00Z", 60)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(reply["kind"], "cumulative");
    assert_eq!(reply["aggregation"], "sum");
    assert_eq!(reply["unit"], "B");
    assert_eq!(
        reply["buckets"],
        json!([
            "2026-10-13T10:00:00Z",
            "2026-10-13T10:01:00Z",
            "2026-10-13T10:02:00Z",
            "2026-10-13T10:03:00Z"
        ])
    );
    assert_eq!(
        reply["series"],
        json!([{"label": "", "values": [null, 10.0, 5.0, 10.0]}])
    );

    // Начало окна на 10:01: приращение первой минуты считается от предыдущей минуты (10).
    let (_, reply) = send(
        &router,
        "POST",
        QUERY,
        Some("query"),
        Some(query_body(
            "bytes_sent",
            "2026-10-13T10:01:00Z",
            "2026-10-13T10:04:00Z",
            60,
        )),
    )
    .await;
    assert_eq!(
        reply["series"],
        json!([{"label": "", "values": [10.0, 5.0, 10.0]}])
    );
    core.shutdown().unwrap();
}

#[tokio::test]
async fn gauge_aggregations_follow_the_definition() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(config(dir.path(), 1000)).unwrap();
    let router = api::router(core.clone());
    core.journal
        .accept(&batch(vec![
            (
                source("api", Some("eu-1")),
                vec![
                    report(
                        1,
                        "2026-10-13T10:00:10Z",
                        json!({}),
                        json!({"queue_depth": 4}),
                    ),
                    report(
                        2,
                        "2026-10-13T10:00:20Z",
                        json!({}),
                        json!({"queue_depth": 10}),
                    ),
                ],
            ),
            (
                source("api", Some("eu-2")),
                vec![report(
                    3,
                    "2026-10-13T10:00:15Z",
                    json!({}),
                    json!({"queue_depth": 6}),
                )],
            ),
        ]))
        .unwrap();
    wait(|| core.snapshot().watermark >= 3);
    let avg = 20.0 / 3.0;

    // Без определения точки gauge считаются средним.
    let (_, reply) = send(
        &router,
        "POST",
        QUERY,
        Some("query"),
        Some(query_body("queue_depth", FROM, TO, 60)),
    )
    .await;
    assert_eq!(reply["kind"], "gauge");
    assert_eq!(reply["aggregation"], "avg");
    let value = reply["series"][0]["values"][0].as_f64().unwrap();
    assert!((value - avg).abs() < 1e-9, "{value}");

    let (status, created) = send(
        &router,
        "POST",
        DEFINITIONS,
        Some("admin"),
        Some(
            json!({"service": "api", "name": "queue_depth", "kind": "gauge", "aggregation": "max"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let uri = format!("{DEFINITIONS}/{}", created["id"].as_str().unwrap());
    // Максимум 10; минимум 4; последние значения серий 10 и 6 дают 16; сумма 20.
    for (aggregation, expected) in [
        ("max", 10.0),
        ("min", 4.0),
        ("last", 16.0),
        ("sum", 20.0),
        ("avg", avg),
    ] {
        let (status, _) = send(
            &router,
            "PUT",
            &uri,
            Some("admin"),
            Some(json!({"service": "api", "name": "queue_depth", "kind": "gauge", "aggregation": aggregation})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (_, reply) = send(
            &router,
            "POST",
            QUERY,
            Some("query"),
            Some(query_body("queue_depth", FROM, TO, 60)),
        )
        .await;
        assert_eq!(reply["aggregation"], aggregation);
        let value = reply["series"][0]["values"][0].as_f64().unwrap();
        assert!((value - expected).abs() < 1e-9, "{aggregation}: {value}");
    }
    core.shutdown().unwrap();
}

#[tokio::test]
async fn open_segment_points_are_visible_and_match_after_sealing() {
    let dir = tempfile::tempdir().unwrap();
    // Большой сегмент и возраст по умолчанию: точки остаются в открытом сегменте.
    let cfg = config(dir.path(), 1000);
    let core = Core::open(cfg.clone()).unwrap();
    let router = api::router(core.clone());
    core.journal
        .accept(&batch(vec![(
            source("api", Some("eu-1")),
            vec![
                report(
                    1,
                    "2026-10-13T10:00:10Z",
                    json!({"runs": 3}),
                    json!({"queue_depth": 1.5}),
                ),
                report(
                    2,
                    "2026-10-13T10:00:40Z",
                    json!({"runs": 1}),
                    json!({"queue_depth": 2.25}),
                ),
            ],
        )]))
        .unwrap();
    wait(|| core.snapshot().watermark >= 2);
    assert!(core.snapshot().series_files.is_empty());
    assert!(core.snapshot().active.is_some());
    let mut open = vec![];
    for name in ["runs", "queue_depth"] {
        let (status, reply) = send(
            &router,
            "POST",
            QUERY,
            Some("query"),
            Some(query_body(name, FROM, TO, 60)),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        open.push(reply);
    }
    assert_eq!(open[0]["series"], json!([{"label": "", "values": [4.0]}]));
    assert_eq!(open[1]["series"], json!([{"label": "", "values": [1.875]}]));
    drop(router);
    // Остановка запечатывает открытый сегмент; тот же запрос к Parquet даёт те же значения.
    core.shutdown().unwrap();
    drop(core);
    let core = Core::open(cfg).unwrap();
    assert_eq!(core.snapshot().series_files.len(), 1);
    let router = api::router(core.clone());
    for (name, mut expected) in ["runs", "queue_depth"].into_iter().zip(open) {
        let (_, mut sealed) = send(
            &router,
            "POST",
            QUERY,
            Some("query"),
            Some(query_body(name, FROM, TO, 60)),
        )
        .await;
        expected.as_object_mut().unwrap().remove("as_of");
        sealed.as_object_mut().unwrap().remove("as_of");
        assert_eq!(sealed, expected, "{name}");
    }
    drop(router);
    core.shutdown().unwrap();
}

#[tokio::test]
async fn list_joins_observed_series_with_definitions_and_adds_definitions_without_points() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path(), 1000);
    let core = Core::open(cfg.clone()).unwrap();
    core.journal
        .accept(&batch(vec![(
            source("api", Some("eu-1")),
            vec![report(
                1,
                "2026-10-13T10:00:10Z",
                json!({"runs": 2}),
                json!({"queue_depth": 1.5}),
            )],
        )]))
        .unwrap();
    wait(|| core.snapshot().watermark >= 1);
    // Первая точка уходит в каталог, вторая остаётся в открытом сегменте после перезапуска.
    core.shutdown().unwrap();
    drop(core);
    let core = Core::open(cfg).unwrap();
    core.journal
        .accept(&batch(vec![(
            source("api", Some("eu-1")),
            vec![report(
                2,
                "2026-10-13T10:05:10Z",
                json!({"runs": 3}),
                json!({}),
            )],
        )]))
        .unwrap();
    wait(|| {
        core.snapshot()
            .active
            .as_ref()
            .is_some_and(|active| active.meta.rows == 1)
    });
    let router = api::router(core.clone());
    for body in [
        json!({"service": "api", "name": "errors", "kind": "delta", "rule": {"contains": "boom", "pattern": "boom"}}),
        json!({"service": "api", "name": "idle", "kind": "gauge"}),
        json!({"service": "api", "name": "runs", "kind": "delta", "title": "Запуски", "unit": "count"}),
    ] {
        let (status, _) = send(&router, "POST", DEFINITIONS, Some("admin"), Some(body)).await;
        assert_eq!(status, StatusCode::CREATED);
    }

    let (status, reply) = send(&router, "GET", LIST, Some("query"), None).await;
    assert_eq!(status, StatusCode::OK);
    let metrics = reply["metrics"].as_array().unwrap();
    let summary: Vec<Value> = metrics
        .iter()
        .map(|metric| {
            json!([
                metric["name"],
                metric["origin"],
                metric["first_seen"],
                metric["last_seen"],
                !metric["definition"].is_null()
            ])
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            json!(["errors", "rule", null, null, true]),
            json!(["idle", "gauge", null, null, true]),
            json!([
                "queue_depth",
                "gauge",
                "2026-10-13T10:00:10Z",
                "2026-10-13T10:00:10Z",
                false
            ]),
            json!([
                "runs",
                "counter",
                "2026-10-13T10:00:10Z",
                "2026-10-13T10:05:10Z",
                true
            ]),
        ]
    );
    assert_eq!(metrics[0]["definition"]["rule"]["contains"], "boom");
    assert_eq!(metrics[3]["definition"]["title"], "Запуски");
    assert_eq!(metrics[3]["definition"]["unit"], "count");
    drop(router);
    core.shutdown().unwrap();
}

/// Метрика с указанным именем из ответа GET /v1/series.
fn listed<'a>(list: &'a Value, name: &str) -> &'a Value {
    list["metrics"]
        .as_array()
        .unwrap()
        .iter()
        .find(|metric| metric["name"] == name)
        .unwrap()
}

/// Серии ответа на запрос с токеном query; статус проверяется.
async fn series_of(router: &Router, body: Value) -> Value {
    let (status, reply) = send(router, "POST", QUERY, Some("query"), Some(body)).await;
    assert_eq!(status, StatusCode::OK);
    reply["series"].clone()
}

#[tokio::test]
async fn environment_filters_queries_and_lists_environments_across_sealing() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path(), 1000);
    let core = Core::open(cfg.clone()).unwrap();
    let router = api::router(core.clone());
    let staging = Source {
        environment: Some("staging".into()),
        ..source("api", Some("eu-2"))
    };
    // Группа без окружения попадает в суммы без фильтра, но не в список окружений.
    let unlabeled = Source {
        environment: None,
        ..source("api", Some("eu-3"))
    };
    core.journal
        .accept(&batch(vec![
            (
                source("api", Some("eu-1")),
                vec![report(
                    1,
                    "2026-10-13T10:00:10Z",
                    json!({"runs": 3}),
                    json!({}),
                )],
            ),
            (
                staging,
                vec![report(
                    2,
                    "2026-10-13T10:00:20Z",
                    json!({"runs": 5}),
                    json!({}),
                )],
            ),
            (
                unlabeled,
                vec![report(
                    3,
                    "2026-10-13T10:00:30Z",
                    json!({"runs": 1, "boots": 1}),
                    json!({}),
                )],
            ),
        ]))
        .unwrap();
    wait(|| core.snapshot().watermark >= 3);

    let mut body = query_body("runs", FROM, TO, 60);
    assert_eq!(
        series_of(&router, body.clone()).await,
        json!([{"label": "", "values": [9.0]}])
    );
    body["environment"] = json!("staging");
    assert_eq!(
        series_of(&router, body.clone()).await,
        json!([{"label": "", "values": [5.0]}])
    );
    body["environment"] = json!("production");
    assert_eq!(
        series_of(&router, body.clone()).await,
        json!([{"label": "", "values": [3.0]}])
    );
    body["environment"] = json!("qa");
    assert_eq!(series_of(&router, body.clone()).await, json!([]));

    // Окружения открытого сегмента.
    assert!(core.snapshot().active.is_some());
    let (status, list) = send(&router, "GET", LIST, Some("query"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        listed(&list, "runs")["environments"],
        json!(["production", "staging"])
    );
    assert_eq!(listed(&list, "boots")["environments"], json!([]));
    drop(router);
    core.shutdown().unwrap();
    drop(core);

    // После запечатывания окружения читаются из каталога.
    let core = Core::open(cfg).unwrap();
    let router = api::router(core.clone());
    let (_, list) = send(&router, "GET", LIST, Some("query"), None).await;
    assert_eq!(
        listed(&list, "runs")["environments"],
        json!(["production", "staging"])
    );
    assert_eq!(listed(&list, "boots")["environments"], json!([]));
    body["environment"] = json!("staging");
    assert_eq!(
        series_of(&router, body).await,
        json!([{"label": "", "values": [5.0]}])
    );
    drop(router);
    core.shutdown().unwrap();
}

#[tokio::test]
async fn invalid_queries_are_rejected_and_unknown_metrics_are_empty() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(config(dir.path(), 1000)).unwrap();
    let router = api::router(core.clone());
    let mut unknown_field = query_body("runs", FROM, TO, 60);
    unknown_field["limit"] = json!(10);
    let mut unknown_split = query_body("runs", FROM, TO, 60);
    unknown_split["split"] = json!("weekly");
    let mut unknown_origin = query_body("runs", FROM, TO, 60);
    unknown_origin["origin"] = json!("histogram");
    let cases = [
        ("from equals to", query_body("runs", FROM, FROM, 60)),
        ("from after to", query_body("runs", TO, FROM, 60)),
        (
            "more than 10000 buckets",
            query_body("runs", FROM, "2026-10-13T12:46:41Z", 1),
        ),
        ("unknown split", unknown_split),
        ("unknown origin", unknown_origin),
        ("zero step", query_body("runs", FROM, TO, 0)),
        ("step above a week", query_body("runs", FROM, TO, 604_801)),
        ("unknown field", unknown_field),
    ];
    for (case, body) in cases {
        let (status, reply) = send(&router, "POST", QUERY, Some("query"), Some(body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{case}");
        assert!(reply["error"].is_string(), "{case}: {reply}");
    }

    // 10000 корзин - предел, который ещё допустим.
    let (status, reply) = send(
        &router,
        "POST",
        QUERY,
        Some("query"),
        Some(query_body("runs", FROM, "2026-10-13T12:46:40Z", 1)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(reply["buckets"].as_array().unwrap().len(), 10_000);

    let (status, reply) = send(
        &router,
        "POST",
        QUERY,
        Some("query"),
        Some(query_body("ghost", FROM, TO, 60)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(reply["buckets"], json!([FROM]));
    assert_eq!(reply["series"], json!([]));
    assert_eq!(
        (reply["kind"].as_str(), reply["aggregation"].as_str()),
        (Some("delta"), Some("sum"))
    );
    core.shutdown().unwrap();
}

#[tokio::test]
async fn query_routes_need_the_query_token() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(config(dir.path(), 1000)).unwrap();
    let router = api::router(core.clone());
    for token in [None, Some("admin"), Some("ingest")] {
        let (status, _) = send(&router, "GET", LIST, token, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "GET {token:?}");
        let (status, _) = send(
            &router,
            "POST",
            QUERY,
            token,
            Some(query_body("runs", FROM, TO, 60)),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "POST {token:?}");
    }
    core.shutdown().unwrap();
}

/// Дата UTC, которая была days дней назад.
fn day_ago(days: i64) -> Date {
    (OffsetDateTime::now_utc() - time::Duration::days(days)).date()
}

/// Время RFC3339: полночь UTC даты плюс секунды.
fn stamp(day: Date, seconds: i64) -> String {
    (day.midnight().assume_utc() + time::Duration::seconds(seconds))
        .format(&Rfc3339)
        .unwrap()
}

/// Секунды эпохи в полночь UTC даты.
fn epoch(day: Date) -> i64 {
    day.midnight().assume_utc().unix_timestamp()
}

/// Конфигурация с агрегатами: сутки строятся сразу после запуска или запечатывания.
fn rollup_config(path: &Path, segment_rows: usize) -> Config {
    let mut cfg = config(path, segment_rows);
    cfg.series_rollup_pause = Duration::from_millis(1);
    cfg
}

/// Идентификатор опубликованного агрегата суток нужной корзины.
fn rollup_id(core: &Core, day: Date, resolution: u32) -> Option<String> {
    core.rollups
        .read()
        .unwrap()
        .iter()
        .find(|file| file.meta.day == day.to_string() && file.meta.resolution == resolution)
        .map(|file| file.meta.id.clone())
}

/// Значения корзин: null, кроме указанных позиций.
fn sparse(len: usize, points: &[(usize, f64)]) -> Value {
    let mut values = vec![Value::Null; len];
    for &(index, value) in points {
        values[index] = json!(value);
    }
    Value::Array(values)
}

/// Строка серии с подписью.
fn line(label: &str, values: Value) -> Value {
    json!({"label": label, "values": values})
}

/// Тело запроса с дополнительными полями (split, environment).
fn query_with(name: &str, from: &str, to: &str, step: u64, extra: Value) -> Value {
    let mut body = query_body(name, from, to, step);
    body.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    body
}

/// Создаёт определение; статус проверяется.
async fn define(router: &Router, body: Value) {
    let (status, _) = send(router, "POST", DEFINITIONS, Some("admin"), Some(body)).await;
    assert_eq!(status, StatusCode::CREATED);
}

/// Пачка счётчика runs одной группы: событие на точку.
fn runs_batch(first_id: u128, points: &[(String, u64)]) -> Vec<u8> {
    let events = points
        .iter()
        .enumerate()
        .map(|(index, (time, runs))| {
            report(
                first_id + index as u128,
                time,
                json!({"runs": runs}),
                json!({}),
            )
        })
        .collect();
    batch(vec![(source("api", Some("eu-1")), events)])
}

/// Точки двух суток: новый день (почасовые агрегаты) и старый (12-часовые). Очередь пишется четырьмя gauge.
fn rolled_batch(new: Date, old: Date) -> Vec<u8> {
    let queue = |depth: f64| json!({"queue_avg": depth, "queue_min": depth, "queue_max": depth, "queue_last": depth});
    let staging = Source {
        environment: Some("staging".into()),
        ..source("api", Some("eu-1"))
    };
    batch(vec![
        (
            source("api", Some("eu-1")),
            vec![
                report(1, &stamp(new, 4_200), json!({"runs": 1}), queue(4.0)),
                report(2, &stamp(new, 4_800), json!({"runs": 3}), queue(10.0)),
                report(3, &stamp(new, 19_800), json!({"runs": 2}), queue(6.0)),
                report(6, &stamp(old, 1_800), json!({"runs": 5}), queue(2.0)),
            ],
        ),
        (
            source("api", Some("eu-2")),
            vec![
                report(4, &stamp(new, 4_500), json!({"runs": 4}), queue(6.0)),
                report(7, &stamp(old, 46_800), json!({"runs": 7}), queue(3.0)),
            ],
        ),
        (
            staging,
            vec![
                report(5, &stamp(new, 5_400), json!({"runs": 10}), queue(8.0)),
                report(8, &stamp(old, 48_600), json!({"runs": 11}), queue(5.0)),
            ],
        ),
    ])
}

/// Запрос и ожидаемые серии; окна с одними сутками читаются агрегатами, mixed - частично сырые.
type Case = (&'static str, Value, Value);

fn cases(new: Date, old: Date) -> (Vec<Case>, Vec<Case>) {
    let (nf, nt) = (stamp(new, 0), stamp(new, 86_400));
    let (of, ot) = (stamp(old, 0), stamp(old, 86_400));
    let day = vec![
        (
            "runs hourly",
            query_with("runs", &nf, &nt, 3_600, json!({})),
            json!([line("", sparse(24, &[(1, 18.0), (5, 2.0)]))]),
        ),
        (
            "runs hourly staging",
            query_with("runs", &nf, &nt, 3_600, json!({"environment": "staging"})),
            json!([line("", sparse(24, &[(1, 10.0)]))]),
        ),
        (
            "runs hourly production",
            query_with(
                "runs",
                &nf,
                &nt,
                3_600,
                json!({"environment": "production"}),
            ),
            json!([line("", sparse(24, &[(1, 8.0), (5, 2.0)]))]),
        ),
        (
            "runs hourly by instance",
            query_with("runs", &nf, &nt, 3_600, json!({"split": "instance"})),
            json!([
                line("eu-1", sparse(24, &[(1, 14.0), (5, 2.0)])),
                line("eu-2", sparse(24, &[(1, 4.0)]))
            ]),
        ),
        (
            "runs 12-hour",
            query_with("runs", &of, &ot, 43_200, json!({})),
            json!([line("", sparse(2, &[(0, 5.0), (1, 18.0)]))]),
        ),
        (
            "runs 12-hour by instance",
            query_with("runs", &of, &ot, 43_200, json!({"split": "instance"})),
            json!([
                line("eu-1", sparse(2, &[(0, 5.0), (1, 11.0)])),
                line("eu-2", sparse(2, &[(1, 7.0)]))
            ]),
        ),
        (
            "queue avg hourly",
            query_with("queue_avg", &nf, &nt, 3_600, json!({})),
            json!([line("", sparse(24, &[(1, 7.0), (5, 6.0)]))]),
        ),
        (
            "queue min hourly",
            query_with("queue_min", &nf, &nt, 3_600, json!({})),
            json!([line("", sparse(24, &[(1, 4.0), (5, 6.0)]))]),
        ),
        (
            "queue max hourly",
            query_with("queue_max", &nf, &nt, 3_600, json!({})),
            json!([line("", sparse(24, &[(1, 10.0), (5, 6.0)]))]),
        ),
        // Последние значения складываются по сериям: 10 + 6 + 8 и 10 + 6.
        (
            "queue last hourly",
            query_with("queue_last", &nf, &nt, 3_600, json!({})),
            json!([line("", sparse(24, &[(1, 24.0), (5, 6.0)]))]),
        ),
        (
            "queue last hourly production",
            query_with(
                "queue_last",
                &nf,
                &nt,
                3_600,
                json!({"environment": "production"}),
            ),
            json!([line("", sparse(24, &[(1, 16.0), (5, 6.0)]))]),
        ),
        (
            "queue avg 12-hour",
            query_with("queue_avg", &of, &ot, 43_200, json!({})),
            json!([line("", sparse(2, &[(0, 2.0), (1, 4.0)]))]),
        ),
        (
            "queue min 12-hour",
            query_with("queue_min", &of, &ot, 43_200, json!({})),
            json!([line("", sparse(2, &[(0, 2.0), (1, 3.0)]))]),
        ),
        (
            "queue max 12-hour",
            query_with("queue_max", &of, &ot, 43_200, json!({})),
            json!([line("", sparse(2, &[(0, 2.0), (1, 5.0)]))]),
        ),
        (
            "queue last 12-hour",
            query_with("queue_last", &of, &ot, 43_200, json!({})),
            json!([line("", sparse(2, &[(0, 2.0), (1, 8.0)]))]),
        ),
    ];
    let mixed = vec![
        (
            "runs daily over rolled and empty days",
            query_with("runs", &of, &nt, 86_400, json!({})),
            json!([line("", sparse(31, &[(0, 23.0), (30, 20.0)]))]),
        ),
        (
            "runs 10-minute buckets on a rolled day",
            query_with("runs", &of, &stamp(old, 3_600), 600, json!({})),
            json!([line("", sparse(6, &[(3, 5.0)]))]),
        ),
        // Окно кончается в 13:00: сутки неполные, 13:00 и 13:30 в ответ не входят.
        (
            "runs 12-hour over a partial day",
            query_with("runs", &of, &stamp(old, 46_800), 43_200, json!({})),
            json!([line("", sparse(2, &[(0, 5.0)]))]),
        ),
    ];
    (day, mixed)
}

#[tokio::test]
async fn rollups_answer_day_queries_like_raw_points() {
    let (new, old) = (day_ago(10), day_ago(40));
    let dir = tempfile::tempdir().unwrap();
    // Точки запечатываются остановкой; агрегаты строятся на запуске по одному файлу.
    let core = Core::open(rollup_config(dir.path(), 1000)).unwrap();
    core.journal.accept(&rolled_batch(new, old)).unwrap();
    wait(|| core.snapshot().watermark >= 8);
    core.shutdown().unwrap();
    drop(core);
    let core = Core::open(rollup_config(dir.path(), 1000)).unwrap();
    wait(|| rollup_id(&core, new, HOURLY).is_some() && rollup_id(&core, old, HALF_DAY).is_some());
    assert_eq!(core.snapshot().series_files.len(), 1);
    let router = api::router(core.clone());

    // Те же точки без агрегатов: открытый сегмент, сырые точки.
    let open_dir = tempfile::tempdir().unwrap();
    let open = Core::open(config(open_dir.path(), 1000)).unwrap();
    open.journal.accept(&rolled_batch(new, old)).unwrap();
    wait(|| open.snapshot().watermark >= 8);
    assert!(open.snapshot().series_files.is_empty());
    let open_router = api::router(open.clone());
    for router in [&router, &open_router] {
        for (name, aggregation) in [
            ("queue_min", "min"),
            ("queue_max", "max"),
            ("queue_last", "last"),
        ] {
            define(
                router,
                json!({"service": "api", "name": name, "kind": "gauge", "aggregation": aggregation}),
            )
            .await;
        }
    }

    let (day, mixed) = cases(new, old);
    for (label, body, expected) in day.iter().chain(&mixed) {
        assert_eq!(series_of(&router, body.clone()).await, *expected, "{label}");
        assert_eq!(
            series_of(&open_router, body.clone()).await,
            *expected,
            "open {label}"
        );
    }
    // Суточные окна обходятся без точек: файл запечатанных точек удалён, ответы те же.
    fs::remove_file(&core.snapshot().series_files[0].path).unwrap();
    for (label, body, expected) in &day {
        assert_eq!(
            series_of(&router, body.clone()).await,
            *expected,
            "without points {label}"
        );
    }
    drop(router);
    drop(open_router);
    core.shutdown().unwrap();
    open.shutdown().unwrap();
}

#[tokio::test]
async fn cumulative_increases_cross_from_rolled_up_days_into_raw_days() {
    let ago: Vec<Date> = (0..=10).map(day_ago).collect();
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(rollup_config(dir.path(), 1000)).unwrap();
    // Сутки 9 и 8 уходят в агрегаты, сутки 7, 6 и 5 остаются сырыми.
    let points = [
        (9, 21_600, 90),
        (9, 43_200, 100),
        (8, 10_800, 120),
        (8, 82_800, 150),
        (7, 43_200, 160),
        (6, 43_200, 200),
        (5, 43_200, 205),
    ];
    let events = points
        .iter()
        .enumerate()
        .map(|(index, &(days, seconds, value))| {
            report(
                index as u128 + 1,
                &stamp(ago[days], seconds),
                json!({"bytes_sent": value}),
                json!({}),
            )
        })
        .collect();
    core.journal
        .accept(&batch(vec![(source("api", Some("eu-1")), events)]))
        .unwrap();
    wait(|| core.snapshot().watermark >= 7);
    core.shutdown().unwrap();
    drop(core);
    let core = Core::open(rollup_config(dir.path(), 1000)).unwrap();
    wait(|| {
        rollup_id(&core, ago[9], HOURLY).is_some() && rollup_id(&core, ago[8], HOURLY).is_some()
    });
    let router = api::router(core.clone());
    define(
        &router,
        json!({"service": "api", "name": "bytes_sent", "kind": "cumulative", "unit": "B"}),
    )
    .await;
    // Последние значения суток: 100, 150, 160, 205 (сутки 9, 8, 7, 5); первое приращение без предыдущих.
    let body = query_body("bytes_sent", &stamp(ago[9], 0), &stamp(ago[4], 0), 86_400);
    assert_eq!(
        series_of(&router, body).await,
        json!([line("", json!([null, 50.0, 10.0, 40.0, 5.0]))])
    );
    drop(router);
    core.shutdown().unwrap();
}

#[tokio::test]
async fn weekly_buckets_merge_rolled_up_days_with_raw_days() {
    let ago: Vec<Date> = (0..=10).map(day_ago).collect();
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(rollup_config(dir.path(), 1000)).unwrap();
    // Точка в полдень каждых суток с 10 по 4 назад; значение равно числу суток назад.
    let events = (4..=10)
        .map(|days| {
            report(
                days as u128,
                &stamp(ago[days], 43_200),
                json!({"runs": days}),
                json!({}),
            )
        })
        .collect();
    core.journal
        .accept(&batch(vec![(source("api", Some("eu-1")), events)]))
        .unwrap();
    wait(|| core.snapshot().watermark >= 7);
    core.shutdown().unwrap();
    drop(core);
    let core = Core::open(rollup_config(dir.path(), 1000)).unwrap();
    wait(|| (8..=10).all(|days| rollup_id(&core, ago[days], HOURLY).is_some()));
    let router = api::router(core.clone());

    let seen: Vec<(i64, f64)> = (4..=10)
        .map(|days| (epoch(ago[days]) + 43_200, days as f64))
        .collect();
    let first = epoch(ago[10]).div_euclid(WEEK) * WEEK;
    let last = (epoch(ago[3]) - 1).div_euclid(WEEK) * WEEK;
    let expected: Vec<Option<f64>> = (first..=last)
        .step_by(WEEK as usize)
        .map(|bucket| {
            let hits: Vec<f64> = seen
                .iter()
                .filter(|(time, _)| time.div_euclid(WEEK) * WEEK == bucket)
                .map(|(_, value)| *value)
                .collect();
            (!hits.is_empty()).then(|| hits.iter().sum::<f64>())
        })
        .collect();
    let body = query_body("runs", &stamp(ago[10], 0), &stamp(ago[3], 0), 604_800);
    assert_eq!(
        series_of(&router, body).await,
        json!([line("", json!(expected))])
    );
    drop(router);
    core.shutdown().unwrap();
}

#[tokio::test]
async fn stale_rollup_is_not_used_until_its_day_is_rebuilt() {
    let (new, old) = (day_ago(10), day_ago(40));
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(rollup_config(dir.path(), 1000)).unwrap();
    core.journal
        .accept(&runs_batch(
            1,
            &[(stamp(new, 3_600), 1), (stamp(old, 1_800), 5)],
        ))
        .unwrap();
    wait(|| core.snapshot().watermark >= 2);
    core.shutdown().unwrap();
    drop(core);
    let core = Core::open(rollup_config(dir.path(), 1000)).unwrap();
    wait(|| rollup_id(&core, new, HOURLY).is_some() && rollup_id(&core, old, HALF_DAY).is_some());
    let new_id = rollup_id(&core, new, HOURLY).unwrap();
    let old_id = rollup_id(&core, old, HALF_DAY).unwrap();
    core.shutdown().unwrap();
    drop(core);

    // Поздние точки обоих суток запечатываются остановкой; пауза час держит поток после первой пересборки.
    let core = Core::open(config(dir.path(), 1000)).unwrap();
    core.journal
        .accept(&runs_batch(
            3,
            &[(stamp(new, 7_200), 10), (stamp(old, 3_600), 7)],
        ))
        .unwrap();
    wait(|| {
        core.snapshot()
            .active
            .as_ref()
            .is_some_and(|active| active.meta.rows == 2)
    });
    core.shutdown().unwrap();
    drop(core);
    let mut cfg = config(dir.path(), 1000);
    cfg.series_rollup_pause = Duration::from_secs(3600);
    let core = Core::open(cfg).unwrap();
    wait(|| rollup_id(&core, old, HALF_DAY).is_some_and(|id| id != old_id));
    assert_eq!(rollup_id(&core, new, HOURLY), Some(new_id));
    let router = api::router(core.clone());
    // Устаревшие сутки читаются сырыми точками, поэтому видна и поздняя точка.
    let (nf, nt) = (stamp(new, 0), stamp(new, 86_400));
    let (of, ot) = (stamp(old, 0), stamp(old, 86_400));
    assert_eq!(
        series_of(&router, query_body("runs", &nf, &nt, 3600)).await,
        json!([line("", sparse(24, &[(1, 1.0), (2, 10.0)]))])
    );
    assert_eq!(
        series_of(&router, query_body("runs", &of, &ot, 43_200)).await,
        json!([line("", sparse(2, &[(0, 12.0)]))])
    );
    drop(router);
    core.shutdown().unwrap();
}

#[tokio::test]
async fn open_points_of_a_rolled_up_day_are_read_from_raw_points() {
    let new = day_ago(10);
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(rollup_config(dir.path(), 1000)).unwrap();
    core.journal
        .accept(&runs_batch(1, &[(stamp(new, 3_600), 1)]))
        .unwrap();
    wait(|| core.snapshot().watermark >= 1);
    core.shutdown().unwrap();
    drop(core);
    let core = Core::open(rollup_config(dir.path(), 1000)).unwrap();
    wait(|| rollup_id(&core, new, HOURLY).is_some());
    core.shutdown().unwrap();
    drop(core);

    // Агрегат суток актуален, но новая точка лежит в открытом сегменте и агрегатом не покрыта.
    let core = Core::open(config(dir.path(), 1000)).unwrap();
    wait(|| rollup_id(&core, new, HOURLY).is_some());
    core.journal
        .accept(&runs_batch(2, &[(stamp(new, 7_200), 10)]))
        .unwrap();
    wait(|| {
        core.snapshot()
            .active
            .as_ref()
            .is_some_and(|active| active.meta.rows == 1)
    });
    assert_eq!(core.snapshot().series_files.len(), 1);
    let router = api::router(core.clone());
    let (nf, nt) = (stamp(new, 0), stamp(new, 86_400));
    assert_eq!(
        series_of(&router, query_body("runs", &nf, &nt, 3600)).await,
        json!([line("", sparse(24, &[(1, 1.0), (2, 10.0)]))])
    );
    drop(router);
    core.shutdown().unwrap();
}

#[tokio::test]
async fn server_filter_applies_to_open_raw_and_rollup_points() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(rollup_config(dir.path(), 3)).unwrap();
    let router = api::router(core.clone());
    let day = day_ago(10);
    let current = day_ago(0);
    let mut a = source("api", None);
    a.server = "a".into();
    let mut b = a.clone();
    b.server = "b".into();
    core.journal
        .accept(&batch(vec![
            (
                a.clone(),
                vec![report(
                    31001,
                    &stamp(day, 100),
                    json!({"runs":1}),
                    json!({}),
                )],
            ),
            (
                b.clone(),
                vec![
                    report(31002, &stamp(day, 100), json!({"runs":9}), json!({})),
                    report(31003, &stamp(day, 200), json!({"runs":0}), json!({})),
                ],
            ),
        ]))
        .unwrap();
    wait(|| rollup_id(&core, day, HOURLY).is_some());
    core.journal
        .accept(&batch(vec![
            (
                a,
                vec![report(
                    31004,
                    &stamp(current, 100),
                    json!({"runs":3}),
                    json!({}),
                )],
            ),
            (
                b,
                vec![report(
                    31005,
                    &stamp(current, 100),
                    json!({"runs":7}),
                    json!({}),
                )],
            ),
        ]))
        .unwrap();
    wait(|| {
        core.snapshot()
            .active
            .as_ref()
            .is_some_and(|active| active.meta.rows == 2)
    });
    for (server, expected) in [("a", 4.0), ("b", 16.0)] {
        for step in [60, 3600] {
            let body = query_with(
                "runs",
                &stamp(day, 0),
                &stamp(current, 86400),
                step,
                json!({"server":server}),
            );
            if step == 60 {
                // Raw-only narrow day query stays below the bucket limit.
                let mut body = body;
                body["to"] = json!(stamp(day, 86400));
                let series = series_of(&router, body).await;
                let total: f64 = series[0]["values"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(Value::as_f64)
                    .sum();
                assert_eq!(total, if server == "a" { 1.0 } else { 9.0 });
            } else {
                let series = series_of(&router, body).await;
                let total: f64 = series[0]["values"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(Value::as_f64)
                    .sum();
                assert_eq!(total, expected);
            }
        }
    }
    core.shutdown().unwrap();
}

#[tokio::test]
async fn effective_settings_include_unconfigured_servers_and_scoped_origins() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(config(dir.path(), 2)).unwrap();
    let router = api::router(core.clone());
    let mut a = source("api", None);
    a.server = "a".into();
    let mut b = a.clone();
    b.server = "b".into();
    core.journal
        .accept(&batch(vec![
            (
                a,
                vec![report(41001, FROM, json!({"runs":10}), json!({"load":4}))],
            ),
            (
                b,
                vec![report(41002, FROM, json!({"runs":20,"load":9}), json!({}))],
            ),
        ]))
        .unwrap();
    wait(|| !core.snapshot().series_files.is_empty());
    define(
        &router,
        json!({"server":"a","service":"api","name":"runs","kind":"cumulative"}),
    )
    .await;
    let (status, error) = send(
        &router,
        "POST",
        QUERY,
        Some("query"),
        Some(query_body("runs", FROM, TO, 60)),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["error"], "server_required");
    let (status, reply) = send(
        &router,
        "POST",
        QUERY,
        Some("query"),
        Some(query_with("load", FROM, TO, 60, json!({"server":"a"}))),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(reply["kind"], "gauge");
    assert_eq!(reply["aggregation"], "avg");
    assert_eq!(reply["series"][0]["values"], json!([4.0]));
    define(&router,json!({"server":"other","service":"api","name":"load","kind":"delta","rule":{"contains":"done","pattern":"done"}})).await;
    let (status, reply) = send(
        &router,
        "POST",
        QUERY,
        Some("query"),
        Some(query_with("load", FROM, TO, 60, json!({"origin":"gauge"}))),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(reply["kind"], "gauge");
    core.shutdown().unwrap();
}
