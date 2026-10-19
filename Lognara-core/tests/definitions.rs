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
    mut body: Option<Value>,
) -> (StatusCode, Value) {
    if uri.starts_with(DEFINITIONS)
        && matches!(method, "POST" | "PUT")
        && let Some(Value::Object(value)) = body.as_mut()
    {
        value.entry("server").or_insert(json!("eu-prod-01"));
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

#[tokio::test]
async fn server_scopes_isolate_rules_preview_listing_and_queries() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(config(dir.path(), 2)).unwrap();
    let router = api::router(core.clone());
    for (server, pattern) in [("a", "value=(?P<n>\\d+)"), ("b", "value=(?P<n>\\d+)")] {
        let (status,_)=send(&router,"POST",DEFINITIONS,Some("admin"),Some(json!({"server":server,"service":"api","name":"amount","kind":"gauge","rule":{"contains":"value=","pattern":pattern,"value_group":"n"}}))).await;
        assert_eq!(status, StatusCode::CREATED);
    }
    let now = now_nanos();
    let mut a = source("api", Some("prod"));
    a.server = "a".into();
    let mut b = a.clone();
    b.server = "b".into();
    core.journal
        .accept(&batch(vec![
            (a, vec![event(901, now, "value=2")]),
            (b, vec![event(902, now, "value=9")]),
        ]))
        .unwrap();
    wait(|| !core.snapshot().series_files.is_empty());
    let (status, list) = send(
        &router,
        "GET",
        "/v1/series?group_by=server",
        Some("query"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["metrics"].as_array().unwrap().len(), 2);
    for (server, value) in [("a", 2.0), ("b", 9.0)] {
        let (status,reply)=send(&router,"POST","/v1/series/query",Some("query"),Some(json!({"server":server,"service":"api","name":"amount","origin":"rule","from":lognara_core::model::format_timestamp(now-60_000_000_000),"to":lognara_core::model::format_timestamp(now+60_000_000_000),"step_seconds":60}))).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            reply["series"][0]["values"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v.as_f64() == Some(value)),
            "{reply}"
        );
        let (_,preview)=send(&router,"POST",PREVIEW,Some("admin"),Some(json!({"server":server,"service":"api","rule":{"contains":"value=","pattern":"value=(?P<n>\\d+)","value_group":"n"}}))).await;
        assert_eq!(preview["matched"], 1);
        assert_eq!(preview["samples"][0]["value"], value);
    }
    core.shutdown().unwrap();
    drop(router);
    drop(core);
    let core = Core::open(config(dir.path(), 100)).unwrap();
    assert_eq!(
        lognara_core::scoped_metrics::sources(core.clone())
            .await
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        lognara_core::scoped_metrics::list(core.clone())
            .await
            .unwrap()
            .metrics
            .len(),
        2
    );
    core.shutdown().unwrap();
}

#[tokio::test]
async fn bundle_import_is_atomic_and_supports_conflicts_and_stale_revisions() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(config(dir.path(), 100)).unwrap();
    let router = api::router(core.clone());
    let bundle = json!({"format":"lognara-metrics","version":1,"metrics":[{"name":"requests","kind":"delta","rule":{"contains":"done","pattern":"done"}}]});
    let request = json!({"server":"a","service":"api","bundle":bundle});
    let (status, check) = send(
        &router,
        "POST",
        "/v1/admin/series/import/check",
        Some("admin"),
        Some(request.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(check["errors"], json!([]));
    let mut apply = request.clone();
    apply["revision"] = check["revision"].clone();
    apply["actions"] = json!([{"action":"create"}]);
    let (status, result) = send(
        &router,
        "POST",
        "/v1/admin/series/import/apply",
        Some("admin"),
        Some(apply.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["created"], 1);
    assert_eq!(
        send(
            &router,
            "POST",
            "/v1/admin/series/import/apply",
            Some("admin"),
            Some(apply)
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let (_, check) = send(
        &router,
        "POST",
        "/v1/admin/series/import/check",
        Some("admin"),
        Some(request.clone()),
    )
    .await;
    assert_eq!(check["conflicts"].as_array().unwrap().len(), 1);
    let original = check["conflicts"][0]["existing"]["id"].clone();
    let mut apply = request.clone();
    apply["revision"] = check["revision"].clone();
    apply["actions"] = json!([{"action":"replace"}]);
    assert_eq!(
        send(
            &router,
            "POST",
            "/v1/admin/series/import/apply",
            Some("admin"),
            Some(apply)
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        lognara_core::definitions::list(&core).unwrap()[0]
            .id
            .to_string(),
        original.as_str().unwrap()
    );
    let (_, check) = send(
        &router,
        "POST",
        "/v1/admin/series/import/check",
        Some("admin"),
        Some(request.clone()),
    )
    .await;
    let mut apply = request.clone();
    apply["revision"] = check["revision"].clone();
    apply["actions"] = json!([{"action":"rename","name":"requests.copy"}]);
    assert_eq!(
        send(
            &router,
            "POST",
            "/v1/admin/series/import/apply",
            Some("admin"),
            Some(apply)
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, check) = send(
        &router,
        "POST",
        "/v1/admin/series/import/check",
        Some("admin"),
        Some(request.clone()),
    )
    .await;
    let mut apply = request.clone();
    apply["revision"] = check["revision"].clone();
    apply["actions"] = json!([{"action":"skip"}]);
    assert_eq!(
        send(
            &router,
            "POST",
            "/v1/admin/series/import/apply",
            Some("admin"),
            Some(apply)
        )
        .await
        .1["skipped"],
        1
    );
    let mut bad = request;
    bad["bundle"]["metrics"] = json!([{"name":"fresh","kind":"delta","rule":{"contains":"done","pattern":"done"}},{"name":"broken","kind":"delta","rule":{"contains":"done","pattern":"("}}]);
    let (_, check) = send(
        &router,
        "POST",
        "/v1/admin/series/import/check",
        Some("admin"),
        Some(bad.clone()),
    )
    .await;
    assert_eq!(check["errors"][0]["index"], 1);
    bad["revision"] = check["revision"].clone();
    bad["actions"] = json!([{"action":"create"},{"action":"create"}]);
    assert_eq!(
        send(
            &router,
            "POST",
            "/v1/admin/series/import/apply",
            Some("admin"),
            Some(bad)
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(lognara_core::definitions::list(&core).unwrap().len(), 2);
    let valid = json!({"server":"a","service":"api","bundle":{"format":"lognara-metrics","version":1,"metrics":[{"name":"new","kind":"delta","rule":{"contains":"done","pattern":"done"}}]}});
    for (field, value) in [
        ("version", json!(2)),
        ("format", json!("unknown")),
        ("metrics", json!([])),
    ] {
        let mut invalid = valid.clone();
        invalid["bundle"][field] = value;
        assert_eq!(
            send(
                &router,
                "POST",
                "/v1/admin/series/import/check",
                Some("admin"),
                Some(invalid)
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
    let mut duplicate = valid.clone();
    duplicate["bundle"]["metrics"] =
        json!([valid["bundle"]["metrics"][0], valid["bundle"]["metrics"][0]]);
    let (_, reply) = send(
        &router,
        "POST",
        "/v1/admin/series/import/check",
        Some("admin"),
        Some(duplicate),
    )
    .await;
    assert_eq!(reply["errors"][0]["field"], "name");
    assert_eq!(reply["errors"][0]["index"], 1);
    let mut over_quota = valid;
    over_quota["bundle"]["metrics"]=Value::Array((0..1023).map(|i|json!({"name":format!("extra_{i}"),"kind":"delta","rule":{"contains":"done","pattern":"done"}})).collect());
    let (_, check) = send(
        &router,
        "POST",
        "/v1/admin/series/import/check",
        Some("admin"),
        Some(over_quota.clone()),
    )
    .await;
    assert!(
        check["errors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["index"].is_null())
    );
    over_quota["revision"] = check["revision"].clone();
    over_quota["actions"] = Value::Array((0..1023).map(|_| json!({"action":"create"})).collect());
    assert_eq!(
        send(
            &router,
            "POST",
            "/v1/admin/series/import/apply",
            Some("admin"),
            Some(over_quota)
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(lognara_core::definitions::list(&core).unwrap().len(), 2);
    core.shutdown().unwrap();
}

#[tokio::test]
async fn imports_hundreds_of_rules_per_source_and_reopens_catalog() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path(), 100);
    let core = Core::open(cfg.clone()).unwrap();
    let router = api::router(core.clone());
    for (server, service) in [
        ("a", "api"),
        ("a", "worker"),
        ("a", "web"),
        ("a", "jobs"),
        ("b", "api"),
    ] {
        let metrics: Vec<_> = (0..220).map(|i| json!({"name":format!("metric_{i}"),"kind":"delta","rule":{"contains":"done","pattern":"done"}})).collect();
        let mut request = json!({"server":server,"service":service,"bundle":{"format":"lognara-metrics","version":1,"metrics":metrics}});
        let (status, check) = send(
            &router,
            "POST",
            "/v1/admin/series/import/check",
            Some("admin"),
            Some(request.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{check}");
        assert_eq!(check["errors"], json!([]));
        request["revision"] = check["revision"].clone();
        request["actions"] = json!(vec![json!({"action":"create"}); 220]);
        let (status, result) = send(
            &router,
            "POST",
            "/v1/admin/series/import/apply",
            Some("admin"),
            Some(request),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{result}");
        assert_eq!(result["created"], 220);
    }
    assert_eq!(lognara_core::definitions::list(&core).unwrap().len(), 1100);
    core.shutdown().unwrap();
    drop(router);
    drop(core);
    let reopened = Core::open(cfg).unwrap();
    let definitions = lognara_core::definitions::list(&reopened).unwrap();
    assert_eq!(definitions.len(), 1100);
    let rules = lognara_core::definitions::RuleSet::load(&definitions).unwrap();
    assert_eq!(rules.for_source("a", "api").len(), 220);
    assert_eq!(rules.for_source("b", "api").len(), 220);
    assert!(rules.for_source("unknown", "api").is_empty());
    let mut source = source("api", None);
    source.server = "a".into();
    let row = lognara_core::model::StoredEvent {
        sequence: 0,
        core_received_at: now_nanos(),
        source,
        event: event(1, now_nanos(), "done"),
    };
    let (points, failed) = lognara_core::extract::rules(&row, &rules);
    assert!(!failed);
    assert_eq!(points.len(), 220);
    reopened.shutdown().unwrap();
}

#[test]
fn nginx_templates_match_combined_and_error_logs_with_bounded_labels() {
    use lognara_core::definitions::{DefinitionInput, validate};
    let bundle: Value = serde_json::from_str(include_str!(
        "../../Lognara-dock/web/public/metrics-nginx.json"
    ))
    .unwrap();
    let lines = [
        "2001:db8::1 - - [19/Oct/2026:12:00:00 +0000] \"GET / HTTP/1.1\" 200 0 \"-\" \"curl/8.0\"",
        "127.0.0.1 - user [19/Oct/2026:12:00:00 +0000] \"POST /api HTTP/1.1\" 404 512 \"-\" \"test\\x22agent\"",
        "127.0.0.1 - - [19/Oct/2026:12:00:00 +0000] \"DELETE / HTTP/1.1\" 503 32 \"-\" \"curl\"",
        "2026/10/19 12:00:00 [error] 123#123: *42 upstream timed out",
        "not an nginx log",
    ];
    for (index, value) in bundle["metrics"].as_array().unwrap().iter().enumerate() {
        let mut value = value.clone();
        value["server"] = json!("a");
        value["service"] = json!("nginx");
        let input: DefinitionInput = serde_json::from_value(value).unwrap();
        validate(&input, &[], false).unwrap();
        let regex = lognara_core::definitions::validate_rule(input.rule.as_ref().unwrap()).unwrap();
        let expected = match index {
            0 | 1 | 4 => vec![true, true, true, false, false],
            2 => vec![false, true, false, false, false],
            3 => vec![false, false, true, false, false],
            _ => vec![false, false, false, true, false],
        };
        assert_eq!(
            lines
                .iter()
                .map(|line| regex.is_match(line))
                .collect::<Vec<_>>(),
            expected,
            "{}",
            input.name
        );
    }
}

#[tokio::test]
async fn legacy_migration_copies_known_servers_removes_orphans_and_preserves_points() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(config(dir.path(), 2)).unwrap();
    let now = now_nanos();
    let mut a = source("api", None);
    a.server = "a".into();
    let mut b = a.clone();
    b.server = "b".into();
    let mut native = event(10001, now, "ServiceMetrics");
    native.attributes.insert("Metrics".into(),json!({"SchemaVersion":2,"Counters":{"runs":3},"Timestamp":lognara_core::model::format_timestamp(now)}));
    core.journal
        .accept(&batch(vec![
            (a, vec![native]),
            (b, vec![event(10002, now, "done")]),
        ]))
        .unwrap();
    wait(|| !core.snapshot().series_files.is_empty());
    core.shutdown().unwrap();
    drop(core);
    let path = dir.path().join("catalog.sqlite");
    let original = json!({"id":Uuid::new_v4(),"service":"api","name":"requests","title":"Requests","description":"","unit":"requests","kind":"delta","aggregation":"sum","enabled":true,"rule":{"contains":"done","pattern":"done"},"updated_at":"2026-01-01T00:00:00Z"});
    {
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch("DROP TABLE series_definitions; DROP TABLE metric_sources; DROP TABLE scoped_series; DROP TABLE metrics_backfill; DROP TABLE metrics_migration; CREATE TABLE series_definitions(id TEXT PRIMARY KEY,service TEXT NOT NULL,name TEXT NOT NULL,body TEXT NOT NULL,UNIQUE(service,name)); PRAGMA user_version=2;").unwrap();
        db.execute(
            "INSERT INTO series_definitions VALUES(?1,'api','requests',?2)",
            rusqlite::params![original["id"].as_str().unwrap(), original.to_string()],
        )
        .unwrap();
        let mut orphan = original.clone();
        orphan["id"] = json!(Uuid::new_v4());
        orphan["service"] = json!("unknown");
        db.execute(
            "INSERT INTO series_definitions VALUES(?1,'unknown','requests',?2)",
            rusqlite::params![orphan["id"].as_str().unwrap(), orphan.to_string()],
        )
        .unwrap();
    }
    for _ in 0..2 {
        let core = Core::open(config(dir.path(), 100)).unwrap();
        let definitions = lognara_core::definitions::list(&core).unwrap();
        assert_eq!(definitions.len(), 2);
        assert_eq!(
            definitions
                .iter()
                .map(|d| d.server.as_str())
                .collect::<std::collections::BTreeSet<_>>(),
            std::collections::BTreeSet::from(["a", "b"])
        );
        assert_eq!(
            lognara_core::scoped_metrics::list(core.clone())
                .await
                .unwrap()
                .metrics
                .len(),
            3
        );
        assert_eq!(
            read_points(&core.snapshot().series_files[0].path)[0].value,
            3.0
        );
        core.shutdown().unwrap();
        drop(core);
    }
}

#[test]
fn migration_quota_failure_is_retryable_without_modifying_legacy_definitions() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(config(dir.path(), 1)).unwrap();
    let mut source = source("api", None);
    source.server = "a".into();
    core.journal
        .accept(&batch(vec![(
            source,
            vec![event(20001, now_nanos(), "done")],
        )]))
        .unwrap();
    wait(|| !core.snapshot().closed.is_empty());
    core.shutdown().unwrap();
    drop(core);
    let path = dir.path().join("catalog.sqlite");
    {
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch("DROP TABLE series_definitions; DROP TABLE metric_sources; DROP TABLE scoped_series; DROP TABLE metrics_backfill; DROP TABLE metrics_migration; CREATE TABLE series_definitions(id TEXT PRIMARY KEY,service TEXT NOT NULL,name TEXT NOT NULL,body TEXT NOT NULL,UNIQUE(service,name)); PRAGMA user_version=2;").unwrap();
        for i in 0..1025 {
            let definition = json!({"id":Uuid::new_v4(),"service":"api","name":format!("rule_{i}"),"title":"","description":"","unit":"","kind":"delta","aggregation":"sum","enabled":true,"rule":{"contains":"done","pattern":"done"},"updated_at":"2026-01-01T00:00:00Z"});
            db.execute(
                "INSERT INTO series_definitions VALUES(?1,'api',?2,?3)",
                rusqlite::params![
                    definition["id"].as_str().unwrap(),
                    format!("rule_{i}"),
                    definition.to_string()
                ],
            )
            .unwrap();
        }
    }
    assert!(Core::open(config(dir.path(), 100)).is_err());
    {
        let db = rusqlite::Connection::open(&path).unwrap();
        assert_eq!(
            db.query_row("SELECT count(*) FROM legacy_series_definitions", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap(),
            1025
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM series_definitions", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        db.execute(
            "DELETE FROM legacy_series_definitions WHERE name='rule_1024'",
            [],
        )
        .unwrap();
    }
    let core = Core::open(config(dir.path(), 100)).unwrap();
    assert_eq!(lognara_core::definitions::list(&core).unwrap().len(), 1024);
    core.shutdown().unwrap();
}

#[tokio::test]
async fn cumulative_rule_continues_across_legacy_migration() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(config(dir.path(), 1)).unwrap();
    let input=serde_json::from_value(json!({"server":"a","service":"api","name":"total","kind":"cumulative","rule":{"contains":"total=","pattern":"total=(?P<n>\\d+)","value_group":"n"}})).unwrap();
    let definition = lognara_core::definitions::create(&core, input).unwrap();
    let minute = 60_000_000_000;
    let time = (now_nanos() / minute - 2) * minute;
    let mut source = source("api", None);
    source.server = "a".into();
    core.journal
        .accept(&batch(vec![(
            source.clone(),
            vec![event(51001, time, "total=10")],
        )]))
        .unwrap();
    wait(|| !core.snapshot().series_files.is_empty());
    core.shutdown().unwrap();
    drop(core);
    {
        let db = rusqlite::Connection::open(dir.path().join("catalog.sqlite")).unwrap();
        db.execute_batch("DROP TABLE series_definitions; DROP TABLE metric_sources; DROP TABLE scoped_series; DROP TABLE metrics_backfill; DROP TABLE metrics_migration; CREATE TABLE series_definitions(id TEXT PRIMARY KEY,service TEXT NOT NULL,name TEXT NOT NULL,body TEXT NOT NULL,UNIQUE(service,name)); PRAGMA user_version=2;").unwrap();
        let mut legacy = serde_json::to_value(&definition).unwrap();
        legacy.as_object_mut().unwrap().remove("server");
        db.execute(
            "INSERT INTO series_definitions VALUES(?1,'api','total',?2)",
            rusqlite::params![definition.id.to_string(), legacy.to_string()],
        )
        .unwrap();
    }
    let core = Core::open(config(dir.path(), 1)).unwrap();
    let migrated = lognara_core::definitions::list(&core).unwrap().remove(0);
    assert_eq!(
        migrated.legacy_rule_id.as_deref(),
        Some(definition.id.to_string().as_str())
    );
    core.journal
        .accept(&batch(vec![(
            source,
            vec![event(51002, time + minute, "total=15")],
        )]))
        .unwrap();
    wait(|| core.snapshot().series_files.len() == 2);
    let router = api::router(core.clone());
    let (status,reply)=send(&router,"POST","/v1/series/query",Some("query"),Some(json!({"server":"a","service":"api","name":"total","from":lognara_core::model::format_timestamp(time+minute),"to":lognara_core::model::format_timestamp(time+2*minute),"step_seconds":60}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(reply["series"][0]["values"], json!([5.0]));
    core.shutdown().unwrap();
}

#[tokio::test]
async fn scoped_catalog_and_lookup_return_only_requested_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(config(dir.path(), 100)).unwrap();
    let router = api::router(core.clone());
    for (server, service) in [("a", "api"), ("b", "api"), ("a", "worker")] {
        let (status, _) = send(&router, "POST", DEFINITIONS, Some("admin"), Some(json!({
            "server":server,"service":service,"name":"requests","kind":"delta","rule":{"contains":"done","pattern":"done"}
        }))).await;
        assert_eq!(status, StatusCode::CREATED);
    }
    let (status, reply) = send(
        &router,
        "GET",
        "/v1/series?group_by=server&server=a&service=api",
        Some("query"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(reply["metrics"].as_array().unwrap().len(), 1);
    assert_eq!(reply["metrics"][0]["server"], "a");
    assert_eq!(reply["metrics"][0]["service"], "api");
    let key = json!({"server":"b","service":"api","name":"requests","origin":"rule"});
    let (_, reply) = send(&router,"POST","/v1/series/lookup",Some("query"),Some(json!({"keys":[key.clone(),key,{"server":"missing","service":"api","name":"requests","origin":"rule"}]}))).await;
    assert_eq!(reply["metrics"].as_array().unwrap().len(), 1);
    assert_eq!(reply["metrics"][0]["server"], "b");
    for keys in [
        json!([]),
        json!([{"server":"a","service":"api","name":"requests","origin":"counter"}]),
    ] {
        let (status, reply) = send(
            &router,
            "POST",
            "/v1/series/lookup",
            Some("query"),
            Some(json!({"keys":keys})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(reply["metrics"], json!([]));
    }
    let (status, _) = send(
        &router,
        "POST",
        "/v1/series/lookup",
        None,
        Some(json!({"keys":[]})),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (_, sources) = send(&router, "GET", "/v1/series/sources", Some("query"), None).await;
    assert_eq!(sources["sources"].as_array().unwrap().len(), 3);
    core.shutdown().unwrap();
}
