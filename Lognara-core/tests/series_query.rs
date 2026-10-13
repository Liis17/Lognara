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
    path::Path,
    time::{Duration, Instant},
};
use tower::ServiceExt;
use uuid::Uuid;

const FIXTURE: &[u8] = include_bytes!("fixtures/relay-batch.bin");
const DEFINITIONS: &str = "/v1/admin/series/definitions";
const LIST: &str = "/v1/series";
const QUERY: &str = "/v1/series/query";
const FROM: &str = "2026-10-13T10:00:00Z";
const TO: &str = "2026-10-13T10:01:00Z";

fn config(path: &Path, segment_rows: usize) -> Config {
    let mut cfg = Config::from_lookup(|key| match key {
        "LOGNARA_INGEST_TOKEN" => Some("ingest".into()),
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
    body: Option<Value>,
) -> (StatusCode, Value) {
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
