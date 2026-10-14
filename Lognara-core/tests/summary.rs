use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use lognara_core::{
    api,
    config::Config,
    model::format_timestamp,
    query::{self, QueryError, SearchRequest, TextMode, TextQuery},
    storage::Core,
    summary::{self, SummaryRequest},
    wire,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};
use tower::ServiceExt;

const TS: i64 = 1_790_678_511_871_000_001;
fn config(path: &std::path::Path) -> Config {
    let mut cfg = Config::from_lookup(|key| match key {
        "LOGNARA_ADMIN_TOKEN" => Some("admin".into()),
        "LOGNARA_QUERY_TOKEN" => Some("query".into()),
        _ => None,
    })
    .unwrap();
    cfg.data_dir = path.into();
    cfg.disk_reserve_bytes = 1;
    cfg.segment_rows = 64;
    cfg.refresh_interval = Duration::from_millis(20);
    cfg
}
fn batch(messages: &[&str], durations: &[Value]) -> Vec<u8> {
    let mut batch = wire::decode(include_bytes!("fixtures/relay-batch.bin"), 1 << 20).unwrap();
    let base = batch.groups[0].events[0].clone();
    batch.groups[0].source.service = "billing".into();
    batch.groups[0].events = messages
        .iter()
        .enumerate()
        .map(|(i, message)| {
            let mut event = base.clone();
            event.timestamp = TS + i as i64 * 1_000_000_000;
            event.message = (*message).into();
            event.action = if i % 2 == 0 {
                Some("charge".into())
            } else {
                None
            };
            event.level = [
                wire::LogLevel::Trace,
                wire::LogLevel::Debug,
                wire::LogLevel::Info,
                wire::LogLevel::Warn,
                wire::LogLevel::Error,
                wire::LogLevel::Fatal,
                wire::LogLevel::Unknown,
            ][i % 7];
            event.attributes.clear();
            if let Some(duration) = durations.get(i) {
                event
                    .attributes
                    .insert("duration_ms".into(), duration.clone());
            }
            event
        })
        .collect();
    zstd::encode_all(&rmp_serde::to_vec_named(&batch).unwrap()[..], 3).unwrap()
}
async fn wait(core: &Core, sequence: u64) {
    let end = Instant::now() + Duration::from_secs(15);
    while core.snapshot().watermark < sequence {
        assert!(Instant::now() < end && core.journal.ready());
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
fn request() -> SummaryRequest {
    SummaryRequest {
        from: format_timestamp(TS),
        to: format_timestamp(TS + 200_000_000_000),
        filters: BTreeMap::new(),
        text: None,
        interval_seconds: 60,
        search_limit: Some(2),
    }
}
fn search(core: &Core, request: SearchRequest) -> query::SearchResponse {
    query::search(
        core,
        request,
        false,
        Instant::now() + Duration::from_secs(20),
    )
    .unwrap()
}

#[tokio::test]
async fn text_summary_matches_search_and_pins_page_cursor_across_publication_and_restart() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(config(dir.path())).unwrap();
    let messages: Vec<_> = (0..75)
        .map(|i| {
            [
                "Payment failure",
                "failure payment",
                "payment reset failure",
                "İstanbul ΟΣ",
                "payment payment",
            ][i % 5]
        })
        .collect();
    core.journal.accept(&batch(&messages, &[])).unwrap();
    wait(&core, 75).await;
    assert!(!core.snapshot().closed.is_empty());
    assert!(core.snapshot().active.is_some());
    for (text, mode) in [
        ("payment failure", TextMode::All),
        ("payment failure", TextMode::Phrase),
        ("payment payment", TextMode::Phrase),
        ("payment payment", TextMode::All),
        ("İstanbul οσ", TextMode::All),
        ("i", TextMode::All),
        ("ος", TextMode::All),
    ] {
        let mut req = request();
        req.text = Some(TextQuery {
            query: text.into(),
            mode,
        });
        let expected = search(
            &core,
            SearchRequest {
                from: req.from.clone(),
                to: req.to.clone(),
                filters: req.filters.clone(),
                text: req.text.clone(),
                limit: 1000,
                cursor: None,
            },
        );
        let response = summary::summary(core.clone(), req).await.unwrap();
        assert_eq!(
            response.total as usize,
            expected.events.len(),
            "{text} {mode:?}"
        );
        assert_eq!(
            response.total,
            response.buckets.iter().map(|b| b.count).sum::<u64>()
        );
        assert_eq!(response.total, response.levels.values().sum::<u64>());
        assert_eq!(response.levels.len(), 7);
        let page = response.page.unwrap();
        assert_eq!(page.as_of, response.as_of);
        assert_eq!(
            page.events.iter().map(|e| &e.message).collect::<Vec<_>>(),
            expected
                .events
                .iter()
                .take(2)
                .map(|e| &e.message)
                .collect::<Vec<_>>()
        );
    }
    let response = summary::summary(core.clone(), request()).await.unwrap();
    let page = response.page.unwrap();
    let as_of = page.as_of.clone();
    let mut cursor = page.next_cursor;
    core.journal.accept(&batch(&["new event"], &[])).unwrap();
    wait(&core, 76).await;
    let mut count = page.events.len();
    while let Some(next) = cursor {
        let next = search(
            &core,
            SearchRequest {
                from: request().from,
                to: request().to,
                filters: BTreeMap::new(),
                text: None,
                limit: 20,
                cursor: Some(next),
            },
        );
        assert_eq!(next.as_of, as_of);
        assert!(next.events.iter().all(|event| event.message != "new event"));
        count += next.events.len();
        cursor = next.next_cursor;
    }
    assert_eq!(count as u64, response.total);
    assert_eq!(
        summary::summary(core.clone(), request())
            .await
            .unwrap()
            .total,
        76
    );
    core.shutdown().unwrap();
    drop(core);
    let core = Core::open(config(dir.path())).unwrap();
    let result = summary::summary(core.clone(), request()).await.unwrap();
    assert_eq!(result.total, 76);
    assert_eq!(result.duration.measured, 0);
    assert_eq!(result.duration.p95_ms, None);
    core.shutdown().unwrap();
}

#[tokio::test]
async fn duration_uses_numeric_nonnegative_values_and_reports_coverage_and_action_events() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(config(dir.path())).unwrap();
    let mut values: Vec<_> = (0..=100).map(|value| json!(value)).collect();
    values.extend([
        json!(-1),
        json!("42"),
        json!(true),
        Value::Null,
        json!({"nested": 42}),
    ]);
    let messages = vec!["duration test"; values.len() + 1];
    core.journal.accept(&batch(&messages, &values)).unwrap();
    wait(&core, messages.len() as u64).await;
    let response = summary::summary(core.clone(), request()).await.unwrap();
    assert_eq!(response.total, 107);
    assert_eq!(response.duration.measured, 101);
    assert_eq!(response.duration.coverage, 101.0 / 107.0);
    assert_eq!(response.duration.method, "tdigest");
    let p95 = response.duration.p95_ms.unwrap();
    assert!((93.0..=97.0).contains(&p95), "p95={p95}");
    assert_eq!(
        response.actions.iter().map(|a| a.count).sum::<u64>(),
        response.total
    );
    assert_eq!(
        response
            .actions
            .iter()
            .map(|a| a.duration.measured)
            .sum::<u64>(),
        101
    );
    assert_eq!(
        response.error_services[0].service.as_deref(),
        Some("billing")
    );
    assert_eq!(response.error_services[0].count, response.levels["error"]);
    for bucket in &response.buckets {
        assert_eq!(bucket.count, bucket.levels.values().sum::<u64>());
        assert_eq!(bucket.levels.len(), 7);
    }
    let mut req = request();
    req.from = format_timestamp(TS + 100_000_000_000);
    req.to = format_timestamp(TS + 101_000_000_000);
    let single = summary::summary(core.clone(), req).await.unwrap();
    assert_eq!(single.total, 1);
    assert_eq!(single.duration.p95_ms, Some(100.0));
    let mut req = request();
    req.filters.insert("service".into(), vec!["missing".into()]);
    req.search_limit = None;
    let empty = summary::summary(core.clone(), req).await.unwrap();
    assert_eq!(empty.total, 0);
    assert_eq!(empty.duration.measured, 0);
    assert_eq!(empty.duration.p95_ms, None);
    assert_eq!(empty.duration.coverage, 0.0);
    assert!(empty.actions.is_empty());
    assert!(empty.page.is_none());
    core.shutdown().unwrap();
}

async fn call(core: &std::sync::Arc<Core>, token: &str, body: Value) -> (StatusCode, Value) {
    let response = api::router(core.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/stats/summary")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}
#[tokio::test]
async fn summary_http_validates_query_token_limits_text_and_analytics_capacity() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(config(dir.path())).unwrap();
    let body = json!({"from":format_timestamp(-1),"to":format_timestamp(2_000_000_000),"interval_seconds":1,"search_limit":1});
    for token in ["", "admin", "wrong"] {
        assert_eq!(call(&core, token, body.clone()).await.0, 401);
    }
    let (status, response) = call(&core, "query", body.clone()).await;
    assert_eq!(status, 200);
    assert_eq!(response["total"], 0);
    assert_eq!(response["buckets"].as_array().unwrap().len(), 3);
    assert_eq!(response["buckets"][0]["timestamp"], "1969-12-31T23:59:59Z");
    for (key, value) in [
        ("interval_seconds", json!(0)),
        ("interval_seconds", json!(86401)),
        ("search_limit", json!(0)),
        ("search_limit", json!(1001)),
        ("sql", json!("SELECT")),
        ("text", json!({"query":"!!!"})),
        ("text", json!({"query":"x".repeat(8193)})),
        ("text", json!({"query":vec!["x";65].join(" ")})),
        ("filters", json!({"level":["ERROR"]})),
        ("to", json!(format_timestamp(-1))),
    ] {
        let mut invalid = body.clone();
        invalid[key] = value;
        assert_eq!(call(&core, "query", invalid).await.0, 400, "key={key}");
    }
    let mut invalid = body.clone();
    invalid["to"] = json!(format_timestamp(20_000_000_000_000));
    assert_eq!(call(&core, "query", invalid).await.0, 400);
    let permit = core
        .analytics_slots
        .clone()
        .acquire_many_owned(2)
        .await
        .unwrap();
    assert!(matches!(
        summary::summary(core.clone(), request()).await,
        Err(QueryError::Busy)
    ));
    assert_eq!(call(&core, "query", body).await.0, 503);
    drop(permit);
    core.shutdown().unwrap();
}

#[tokio::test]
async fn finite_extreme_durations_remain_serializable() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(config(dir.path())).unwrap();
    core.journal
        .accept(&batch(&vec!["extreme"; 1000], &vec![json!(1.0e308); 1000]))
        .unwrap();
    wait(&core, 1000).await;
    let mut req = request();
    req.to = format_timestamp(TS + 1_001_000_000_000);
    let response = summary::summary(core.clone(), req).await.unwrap();
    assert!(response.duration.p95_ms.unwrap().is_finite());
    assert!(
        response
            .actions
            .iter()
            .all(|action| action.duration.p95_ms.unwrap().is_finite())
    );
    core.shutdown().unwrap();
}
