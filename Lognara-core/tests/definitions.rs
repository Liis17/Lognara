use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use datafusion::arrow::array::{Array, FixedSizeBinaryArray, Float64Array, StringArray};
use lognara_core::{
    api,
    catalog::Catalog,
    columns,
    config::Config,
    model::now_nanos,
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
const PREVIEW: &str = "/v1/admin/series/preview";

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

fn source(service: &str, environment: Option<&str>) -> Source {
    Source {
        environment: environment.map(Into::into),
        server: "eu-prod-01".into(),
        backend: "barkcloud".into(),
        service: service.into(),
        service_instance: None,
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
        assert!(Instant::now() < deadline, "definitions timed out");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Строка серии, которую проверяют тесты.
#[derive(Debug, PartialEq)]
struct Row {
    service: String,
    name: String,
    origin: String,
    rule_id: Option<String>,
    labels: String,
    value: f64,
    event_id: Option<[u8; 16]>,
}

/// Строки серии, отсортированные по имени и меткам.
fn read_points(path: &Path) -> Vec<Row> {
    let mut rows = vec![];
    for batch in columns::read_all(path).unwrap() {
        let column = |name: &str| batch.column_by_name(name).unwrap().clone();
        let services = column("service");
        let services = services.as_any().downcast_ref::<StringArray>().unwrap();
        let names = column("name");
        let names = names.as_any().downcast_ref::<StringArray>().unwrap();
        let origins = column("origin");
        let origins = origins.as_any().downcast_ref::<StringArray>().unwrap();
        let rule_ids = column("rule_id");
        let rule_ids = rule_ids.as_any().downcast_ref::<StringArray>().unwrap();
        let labels = column("labels");
        let labels = labels.as_any().downcast_ref::<StringArray>().unwrap();
        let values = column("value");
        let values = values.as_any().downcast_ref::<Float64Array>().unwrap();
        let event_ids = column("event_id");
        let event_ids = event_ids
            .as_any()
            .downcast_ref::<FixedSizeBinaryArray>()
            .unwrap();
        for i in 0..batch.num_rows() {
            rows.push(Row {
                service: services.value(i).into(),
                name: names.value(i).into(),
                origin: origins.value(i).into(),
                rule_id: (!rule_ids.is_null(i)).then(|| rule_ids.value(i).into()),
                labels: labels.value(i).into(),
                value: values.value(i),
                event_id: (!event_ids.is_null(i)).then(|| event_ids.value(i).try_into().unwrap()),
            });
        }
    }
    rows.sort_by(|a, b| (&a.name, &a.labels).cmp(&(&b.name, &b.labels)));
    rows
}

/// Имена "service/name" из ответа списка.
fn names(list: &Value) -> Vec<String> {
    list["definitions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|definition| {
            format!(
                "{}/{}",
                definition["service"].as_str().unwrap(),
                definition["name"].as_str().unwrap()
            )
        })
        .collect()
}

#[tokio::test]
async fn definitions_round_trip_with_admin_token() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(config(dir.path(), 1000)).unwrap();
    let router = api::router(core.clone());
    let (status, gauge) = send(
        &router,
        "POST",
        DEFINITIONS,
        Some("admin"),
        Some(json!({"service": "api", "name": "request_ms", "title": "Время ответа", "unit": "ms", "kind": "gauge"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(gauge["aggregation"], "avg");
    assert_eq!(gauge["enabled"], true);
    assert_eq!(gauge["description"], "");
    assert_eq!(gauge["rule"], Value::Null);
    let id = gauge["id"].as_str().unwrap().to_owned();
    assert_eq!(Uuid::parse_str(&id).unwrap().get_version_num(), 4);
    for body in [
        json!({"service": "zeta", "name": "a", "kind": "cumulative"}),
        json!({"service": "api", "name": "errors", "kind": "delta"}),
    ] {
        let (status, _) = send(&router, "POST", DEFINITIONS, Some("admin"), Some(body)).await;
        assert_eq!(status, StatusCode::CREATED);
    }
    let (status, body) = send(&router, "GET", DEFINITIONS, Some("admin"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(names(&body), ["api/errors", "api/request_ms", "zeta/a"]);
    assert_eq!(body["definitions"][2]["aggregation"], "sum");

    let (status, updated) = send(
        &router,
        "PUT",
        &format!("{DEFINITIONS}/{id}"),
        Some("admin"),
        Some(json!({"service": "api", "name": "request_ms", "title": "Ответ", "kind": "gauge", "aggregation": "max", "enabled": false})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(updated["id"], gauge["id"]);
    assert_eq!(updated["title"], "Ответ");
    assert_eq!(updated["aggregation"], "max");
    assert_eq!(updated["enabled"], false);

    let missing = format!("{DEFINITIONS}/{}", Uuid::new_v4());
    let (status, body) = send(
        &router,
        "PUT",
        &missing,
        Some("admin"),
        Some(json!({"service": "api", "name": "x", "kind": "delta"})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, json!({"error": "definition not found"}));

    let uri = format!("{DEFINITIONS}/{id}");
    let (status, _) = send(&router, "DELETE", &uri, Some("admin"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = send(&router, "DELETE", &uri, Some("admin"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, body) = send(&router, "GET", DEFINITIONS, Some("admin"), None).await;
    assert_eq!(names(&body), ["api/errors", "zeta/a"]);
    core.shutdown().unwrap();
}

#[tokio::test]
async fn admin_routes_need_the_admin_token() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(config(dir.path(), 1000)).unwrap();
    let router = api::router(core.clone());
    for token in [None, Some("query"), Some("ingest")] {
        let (status, _) = send(&router, "GET", DEFINITIONS, token, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    let (status, _) = send(
        &router,
        "POST",
        PREVIEW,
        Some("query"),
        Some(json!({"service": "api", "rule": {"contains": "x", "pattern": "x"}})),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    core.shutdown().unwrap();
}

#[tokio::test]
async fn invalid_definitions_are_rejected_with_400() {
    let dir = tempfile::tempdir().unwrap();
    // segment_rows = 1: точка native-метрики сразу попадает в каталог через seal.
    let core = Core::open(config(dir.path(), 1)).unwrap();
    let router = api::router(core.clone());
    let mut report = event(1, now_nanos(), "report");
    report.attributes.insert(
        "Metrics".into(),
        json!({"SchemaVersion": 2, "Counters": {"runs": 1}}),
    );
    core.journal
        .accept(&batch(vec![(
            source("api", Some("production")),
            vec![report],
        )]))
        .unwrap();
    wait(|| !core.snapshot().series_files.is_empty());

    let rule = |pattern: &str| json!({"contains": "x", "pattern": pattern});
    let cases = [
        (
            json!({"service": "api", "name": "bad_regex", "kind": "gauge", "rule": rule("(")}),
            "pattern is invalid",
        ),
        (
            json!({"service": "api", "name": "unknown_group", "kind": "gauge", "rule": {"contains": "x", "pattern": r"(?P<value>\d+)", "value_group": "missing"}}),
            "value_group missing is not a named group",
        ),
        (
            json!({"service": "api", "name": "same_group", "kind": "gauge", "rule": {"contains": "x", "pattern": r"(?P<value>\d+)", "value_group": "value", "label_groups": ["value"]}}),
            "must differ from value_group",
        ),
        (
            json!({"service": "api", "name": "many_labels", "kind": "gauge", "rule": {"contains": "x", "pattern": "(?P<a>x)(?P<b>x)(?P<c>x)(?P<d>x)(?P<e>x)", "label_groups": ["a", "b", "c", "d", "e"]}}),
            "at most 4 label_groups",
        ),
        (
            json!({"service": "api", "name": "bad name", "kind": "gauge"}),
            "name must be",
        ),
        (
            json!({"service": "api", "name": "runs", "kind": "delta", "rule": rule("x")}),
            "collides with a native counter or gauge",
        ),
        (
            json!({"service": "api", "name": "typo", "kind": "gauge", "unknown": 1}),
            "unknown field",
        ),
    ];
    for (input, expected) in cases {
        let (status, body) = send(
            &router,
            "POST",
            DEFINITIONS,
            Some("admin"),
            Some(input.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{input}");
        assert!(
            body["error"].as_str().unwrap().contains(expected),
            "{body} should mention {expected}"
        );
    }

    let errors = json!({"service": "api", "name": "errors", "kind": "delta", "rule": rule("x")});
    let (status, _) = send(
        &router,
        "POST",
        DEFINITIONS,
        Some("admin"),
        Some(errors.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, body) = send(&router, "POST", DEFINITIONS, Some("admin"), Some(errors)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "definition already exists");
    // Определение без правила описывает native-метрику с тем же именем.
    let (status, _) = send(
        &router,
        "POST",
        DEFINITIONS,
        Some("admin"),
        Some(json!({"service": "api", "name": "runs", "kind": "delta", "unit": "count"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    for index in 0..63 {
        let body = json!({"service": "api", "name": format!("rule_{index}"), "kind": "delta", "rule": rule("x")});
        let (status, _) = send(&router, "POST", DEFINITIONS, Some("admin"), Some(body)).await;
        assert_eq!(status, StatusCode::CREATED);
    }
    let (status, body) = send(
        &router,
        "POST",
        DEFINITIONS,
        Some("admin"),
        Some(json!({"service": "api", "name": "overflow", "kind": "delta", "rule": rule("x")})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "at most 64 definitions may have a rule");
    core.shutdown().unwrap();
}

#[tokio::test]
async fn rules_extract_points_from_message_text_of_their_service_only() {
    let dir = tempfile::tempdir().unwrap();
    // Три строки пачки закрывают сегмент и серию.
    let core = Core::open(config(dir.path(), 3)).unwrap();
    let router = api::router(core.clone());
    let (status, timing) = send(
        &router,
        "POST",
        DEFINITIONS,
        Some("admin"),
        Some(json!({
            "service": "api",
            "name": "request_ms",
            "kind": "gauge",
            "unit": "ms",
            "rule": {
                "environment": null,
                "contains": "completed in",
                "pattern": r"completed in (?P<value>\d+) ms, status (?P<status>\d+)",
                "value_group": "value",
                "label_groups": ["status"]
            }
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, count) = send(
        &router,
        "POST",
        DEFINITIONS,
        Some("admin"),
        Some(json!({
            "service": "api",
            "name": "requests",
            "kind": "delta",
            "rule": {"contains": "completed in", "pattern": r"completed in \d+ ms"}
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let now = now_nanos();
    core.journal
        .accept(&batch(vec![
            (
                source("api", Some("production")),
                vec![
                    event(1, now, "Request /api completed in 123 ms, status 200"),
                    event(2, now, "Health check ok"),
                ],
            ),
            (
                source("worker", Some("production")),
                vec![event(
                    3,
                    now,
                    "Request /api completed in 999 ms, status 500",
                )],
            ),
        ]))
        .unwrap();
    wait(|| !core.snapshot().series_files.is_empty());
    let snapshot = core.snapshot();
    assert_eq!(snapshot.series_files.len(), 1);
    let rows = read_points(&snapshot.series_files[0].path);
    let event_id = Uuid::from_u128(1).into_bytes();
    assert_eq!(
        rows,
        vec![
            Row {
                service: "api".into(),
                name: "request_ms".into(),
                origin: "rule".into(),
                rule_id: Some(timing["id"].as_str().unwrap().into()),
                labels: "status=200".into(),
                value: 123.0,
                event_id: Some(event_id),
            },
            Row {
                service: "api".into(),
                name: "requests".into(),
                origin: "rule".into(),
                rule_id: Some(count["id"].as_str().unwrap().into()),
                labels: String::new(),
                value: 1.0,
                event_id: Some(event_id),
            },
        ]
    );
    drop(snapshot);
    core.shutdown().unwrap();
    let names = Catalog::open(&dir.path().join("catalog.sqlite"))
        .unwrap()
        .series_names()
        .unwrap();
    assert_eq!(
        names
            .iter()
            .map(|name| (name.name.as_str(), name.origin.as_str()))
            .collect::<Vec<_>>(),
        [("request_ms", "rule"), ("requests", "rule")]
    );
}

#[tokio::test]
async fn rule_changes_apply_to_new_events_without_restart() {
    let dir = tempfile::tempdir().unwrap();
    // segment_rows = 1: каждое событие сразу закрывает сегмент.
    let core = Core::open(config(dir.path(), 1)).unwrap();
    let router = api::router(core.clone());
    let (status, created) = send(
        &router,
        "POST",
        DEFINITIONS,
        Some("admin"),
        Some(json!({
            "service": "api",
            "name": "request_ms",
            "kind": "gauge",
            "rule": {"contains": "completed in", "pattern": r"completed in (?P<value>\d+) ms", "value_group": "value"}
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let uri = format!("{DEFINITIONS}/{}", created["id"].as_str().unwrap());
    let one_event = |id: u128, message: &str| {
        batch(vec![(
            source("api", Some("production")),
            vec![event(id, now_nanos(), message)],
        )])
    };

    core.journal
        .accept(&one_event(1, "completed in 123 ms"))
        .unwrap();
    wait(|| core.snapshot().series_files.len() == 1);
    let first = read_points(&core.snapshot().series_files[0].path);
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].value, 123.0);

    // Без value_group правило считает совпадения: значение 1.0.
    let (status, _) = send(
        &router,
        "PUT",
        &uri,
        Some("admin"),
        Some(json!({"service": "api", "name": "request_ms", "kind": "delta", "rule": {"contains": "completed in", "pattern": r"completed in \d+ ms"}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    core.journal
        .accept(&one_event(2, "completed in 456 ms"))
        .unwrap();
    wait(|| core.snapshot().series_files.len() == 2);
    let second = read_points(&core.snapshot().series_files[1].path);
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].value, 1.0);
    assert_eq!(second[0].labels, "");

    // Выключенное правило не даёт точек, а событие всё равно материализуется.
    let (status, _) = send(
        &router,
        "PUT",
        &uri,
        Some("admin"),
        Some(json!({"service": "api", "name": "request_ms", "kind": "delta", "enabled": false, "rule": {"contains": "completed in", "pattern": r"completed in \d+ ms"}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    core.journal
        .accept(&one_event(3, "completed in 789 ms"))
        .unwrap();
    wait(|| core.snapshot().closed.len() == 3);
    assert_eq!(core.snapshot().series_files.len(), 2);
    core.shutdown().unwrap();
}

#[tokio::test]
async fn preview_scans_recent_logs_of_the_service_and_environment() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(config(dir.path(), 1000)).unwrap();
    let router = api::router(core.clone());
    let now = now_nanos();
    core.journal
        .accept(&batch(vec![
            (
                source("api", Some("production")),
                vec![
                    event(
                        1,
                        now - 1_000_000_000,
                        "Request /api completed in 123 ms, status 200",
                    ),
                    event(
                        2,
                        now - 2_000_000_000,
                        "Request /api completed in 45 ms, status 500",
                    ),
                    event(3, now - 3_000_000_000, "req_id logged"),
                    event(4, now - 4_000_000_000, "reqXid logged"),
                ],
            ),
            (
                source("api", Some("staging")),
                vec![event(5, now, "Request /api completed in 9 ms, status 200")],
            ),
            (
                source("worker", Some("production")),
                vec![event(6, now, "Request /api completed in 9 ms, status 200")],
            ),
        ]))
        .unwrap();
    wait(|| core.snapshot().watermark >= 6);

    let completed = json!({
        "contains": "completed in",
        "pattern": r"completed in (?P<value>\d+) ms, status (?P<status>\d+)",
        "value_group": "value",
        "label_groups": ["status"]
    });
    let (status, body) = send(
        &router,
        "POST",
        PREVIEW,
        Some("admin"),
        Some(json!({"service": "api", "environment": "production", "rule": completed})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["scanned"], 2);
    assert_eq!(body["matched"], 2);
    // Образцы новее первыми.
    assert_eq!(body["samples"][0]["value"], 123.0);
    assert_eq!(body["samples"][0]["labels"], "status=200");
    assert_eq!(
        body["samples"][0]["message"],
        "Request /api completed in 123 ms, status 200"
    );
    assert!(body["samples"][0]["timestamp"].is_string());
    assert_eq!(body["samples"][1]["labels"], "status=500");

    let (_, body) = send(
        &router,
        "POST",
        PREVIEW,
        Some("admin"),
        Some(json!({"service": "api", "rule": completed})),
    )
    .await;
    assert_eq!(
        (body["scanned"].as_u64(), body["matched"].as_u64()),
        (Some(3), Some(3))
    );

    // Окружение внутри rule игнорируется; фильтр задаётся на верхнем уровне.
    let mut other_env = completed.clone();
    other_env["environment"] = json!("nonexistent");
    let (_, body) = send(
        &router,
        "POST",
        PREVIEW,
        Some("admin"),
        Some(json!({"service": "api", "environment": "production", "rule": other_env})),
    )
    .await;
    assert_eq!(
        (body["scanned"].as_u64(), body["matched"].as_u64()),
        (Some(2), Some(2))
    );

    // Подстрока экранируется: "_" не работает как подстановочный символ.
    let (_, body) = send(
        &router,
        "POST",
        PREVIEW,
        Some("admin"),
        Some(json!({"service": "api", "environment": "production", "rule": {"contains": "req_id", "pattern": "req_id"}})),
    )
    .await;
    assert_eq!(
        (body["scanned"].as_u64(), body["matched"].as_u64()),
        (Some(1), Some(1))
    );

    for minutes in [0, 61] {
        let (status, body) = send(
            &router,
            "POST",
            PREVIEW,
            Some("admin"),
            Some(json!({"service": "api", "minutes": minutes, "rule": completed})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "minutes must be 1..60");
    }
    core.shutdown().unwrap();
}
