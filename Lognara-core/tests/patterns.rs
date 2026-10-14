use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use lognara_core::{
    api,
    config::Config,
    model::format_timestamp,
    patterns::{self, DetailsRequest, PatternRequest, PatternResponse, Sort},
    query::{self, QueryError, SearchRequest},
    storage::Core,
    summary::{self, SummaryRequest},
    wire::{self, LogLevel},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};
use tower::ServiceExt;

const TS: i64 = 1_790_678_400_000_000_000;
const SECOND: i64 = 1_000_000_000;

fn config(path: &std::path::Path) -> Config {
    let mut cfg = Config::from_lookup(|key| match key {
        "LOGNARA_ADMIN_TOKEN" => Some("admin".into()),
        "LOGNARA_QUERY_TOKEN" => Some("query".into()),
        _ => None,
    })
    .unwrap();
    cfg.data_dir = path.into();
    cfg.disk_reserve_bytes = 1;
    cfg.segment_rows = 8;
    cfg.refresh_interval = Duration::from_millis(20);
    cfg
}

fn batch(events: &[(LogLevel, &str, i64)]) -> Vec<u8> {
    let mut batch = wire::decode(include_bytes!("fixtures/relay-batch.bin"), 1 << 20).unwrap();
    let base = batch.groups[0].events[0].clone();
    batch.groups[0].source.service = "billing".into();
    batch.groups[0].events = events
        .iter()
        .map(|(level, message, offset)| {
            let mut event = base.clone();
            event.id = wire::LogId(uuid::Uuid::now_v7());
            event.timestamp = TS + offset * SECOND;
            event.level = *level;
            event.message = (*message).into();
            event
        })
        .collect();
    zstd::encode_all(&rmp_serde::to_vec_named(&batch).unwrap()[..], 3).unwrap()
}

async fn processed(core: &Core, sequence: u64) {
    let end = Instant::now() + Duration::from_secs(20);
    while core
        .patterns
        .checkpoint()
        .is_none_or(|checkpoint| checkpoint < sequence)
    {
        assert!(
            Instant::now() < end && core.journal.ready(),
            "pattern catalog timed out"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn request(limit: usize, sort: Sort, exclude: &[&str]) -> PatternRequest {
    PatternRequest {
        from: format_timestamp(TS),
        to: format_timestamp(TS + 100 * SECOND),
        filters: BTreeMap::new(),
        text: None,
        interval_seconds: 60,
        limit,
        sort,
        exclude_patterns: exclude.iter().map(|id| id.to_string()).collect(),
    }
}

fn details(filters: BTreeMap<String, Vec<String>>) -> DetailsRequest {
    DetailsRequest {
        from: format_timestamp(TS),
        to: format_timestamp(TS + 100 * SECOND),
        filters,
        text: None,
        interval_seconds: 60,
    }
}

fn find<'a>(response: &'a PatternResponse, template: &str) -> &'a patterns::Pattern {
    response
        .patterns
        .iter()
        .find(|pattern| pattern.template == template)
        .unwrap_or_else(|| panic!("missing {template}"))
}

fn fixture() -> Vec<(LogLevel, String, i64)> {
    let mut events = vec![
        (
            LogLevel::Error,
            "payment id=90 failed after 5ms".to_owned(),
            -50,
        ),
        (
            LogLevel::Error,
            "payment id=91 failed after 7ms".to_owned(),
            -40,
        ),
    ];
    for i in 0..6 {
        events.push((
            LogLevel::Error,
            format!("payment id={} failed after {}ms", i % 6, 10 + i),
            1 + i,
        ));
    }
    events.push((LogLevel::Error, "payment id=1 failed after 3ms".into(), 20));
    for i in 0..3 {
        events.push((LogLevel::Info, "cache miss key=a".into(), 30 + i));
    }
    events.push((LogLevel::Warn, "new thing happened".into(), 50));
    events
}

#[tokio::test]
async fn counts_come_from_raw_events_and_survive_restart() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(config(dir.path())).unwrap();
    let events = fixture();
    let borrowed: Vec<_> = events
        .iter()
        .map(|(l, m, o)| (*l, m.as_str(), *o))
        .collect();
    core.journal.accept(&batch(&borrowed)).unwrap();
    processed(&core, 13).await;

    let full = patterns::search(&core, request(100, Sort::Count, &[]))
        .await
        .unwrap();
    assert_eq!(
        (
            full.total_events,
            full.total_patterns,
            full.visible_patterns
        ),
        (11, 3, 3)
    );
    assert!(full.history_complete && full.watermark == 13 && full.core_watermark >= 13);
    assert_eq!(full.new_patterns, Some(2));
    let payment = find(&full, "payment id=<id_1> failed after <duration_1>");
    assert_eq!((payment.count, payment.previous_count), (7, 2));
    assert_eq!(payment.growth_percent, Some(250.0));
    assert_eq!(payment.is_new, Some(false));
    assert_eq!(payment.first_seen, format_timestamp(TS - 50 * SECOND));
    assert_eq!(payment.last_seen, format_timestamp(TS + 20 * SECOND));
    assert_eq!(
        payment
            .buckets
            .iter()
            .map(|bucket| bucket.count)
            .sum::<u64>(),
        7
    );
    let cache = find(&full, "cache miss key=a");
    assert_eq!(
        (
            cache.count,
            cache.previous_count,
            cache.growth_percent,
            cache.is_new
        ),
        (3, 0, None, Some(true))
    );
    assert!((cache.share - 3.0 / 11.0).abs() < 1e-12);
    assert_eq!(full.patterns[0].pattern_id, payment.pattern_id);

    // Размер страницы и скрытие не меняют знаменатель и счётчики.
    let top = patterns::search(&core, request(1, Sort::Count, &[]))
        .await
        .unwrap();
    assert_eq!(
        (top.patterns.len(), top.total_events, top.total_patterns),
        (1, 11, 3)
    );
    assert_eq!(top.patterns[0].count, 7);
    let growth = patterns::search(&core, request(100, Sort::Growth, &[]))
        .await
        .unwrap();
    assert_eq!(
        growth.patterns.last().unwrap().pattern_id,
        payment.pattern_id
    );
    let hidden = patterns::search(&core, request(100, Sort::Count, &[&payment.pattern_id]))
        .await
        .unwrap();
    assert_eq!(
        (
            hidden.total_events,
            hidden.total_patterns,
            hidden.visible_patterns
        ),
        (11, 3, 2)
    );
    assert_eq!(hidden.new_patterns, Some(2));
    assert!((find(&hidden, "cache miss key=a").share - cache.share).abs() < 1e-12);

    // Точный фильтр pattern_id согласован в поиске и агрегатах.
    let filters = BTreeMap::from([(
        "pattern_id".to_owned(),
        vec![payment.pattern_id.to_uppercase().replace("P1_", "p1_")],
    )]);
    let found = query::search(
        &core,
        SearchRequest {
            from: format_timestamp(TS),
            to: format_timestamp(TS + 100 * SECOND),
            filters: filters.clone(),
            text: None,
            limit: 1000,
            cursor: None,
        },
        false,
        Instant::now() + Duration::from_secs(20),
    )
    .unwrap();
    assert_eq!(found.events.len(), 7);
    let total = summary::summary(
        core.clone(),
        SummaryRequest {
            from: format_timestamp(TS),
            to: format_timestamp(TS + 100 * SECOND),
            filters: filters.clone(),
            text: None,
            interval_seconds: 60,
            search_limit: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(total.total, 7);

    let detail = patterns::details(&core, &payment.pattern_id, details(BTreeMap::new()))
        .await
        .unwrap();
    assert_eq!(detail.pattern.count, 7);
    assert_eq!(detail.examples.len(), 3);
    assert_eq!(
        detail.examples[0].timestamp,
        format_timestamp(TS + 20 * SECOND)
    );
    let id = &detail.variables[0];
    assert_eq!(
        (id.name.as_str(), id.total, id.values.len(), id.other_count),
        ("id_1", 7, 5, 1)
    );
    assert_eq!((id.values[0].value.as_str(), id.values[0].count), ("1", 2));
    assert_eq!(detail.variables[1].name, "duration_1");
    assert!(matches!(
        patterns::details(
            &core,
            &format!("p1_{}", "0".repeat(64)),
            details(BTreeMap::new())
        )
        .await,
        Err(QueryError::NotFound)
    ));
    assert!(matches!(
        patterns::details(&core, "p1_xyz", details(BTreeMap::new())).await,
        Err(QueryError::Invalid(_))
    ));

    // Рестарт сохраняет ID и lifetime first_seen; позднее событие сдвигает first_seen.
    core.shutdown().unwrap();
    drop(core);
    let core = Core::open(config(dir.path())).unwrap();
    processed(&core, 13).await;
    let again = patterns::search(&core, request(100, Sort::Count, &[]))
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(&again.patterns).unwrap(),
        serde_json::to_value(&full.patterns).unwrap()
    );
    core.journal
        .accept(&batch(&[(LogLevel::Info, "cache miss key=a", -30)]))
        .unwrap();
    processed(&core, 14).await;
    let late = patterns::search(&core, request(100, Sort::Count, &[]))
        .await
        .unwrap();
    let cache = find(&late, "cache miss key=a");
    assert_eq!(
        (cache.count, cache.previous_count, cache.is_new),
        (3, 1, Some(false))
    );
    assert_eq!(late.new_patterns, Some(1));

    let response = api::router(core.clone())
        .oneshot(
            Request::post("/v1/patterns/search")
                .header("authorization", "Bearer query")
                .header("content-type", "application/json")
                .body(Body::from(json!({"from": format_timestamp(TS), "to": format_timestamp(TS + 100 * SECOND), "limit": 101}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let response = api::router(core.clone())
        .oneshot(
            Request::post(format!("/v1/patterns/{}/details", payment.pattern_id))
                .header("authorization", "Bearer query")
                .header("content-type", "application/json")
                .body(Body::from(json!({"from": format_timestamp(TS), "to": format_timestamp(TS + 100 * SECOND)}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 1 << 20).await.unwrap()).unwrap();
    assert_eq!(
        body["pattern"]["parts"][1],
        json!({"kind": "variable", "name": "id_1", "variable_type": "id"})
    );
    core.shutdown().unwrap();
}

#[tokio::test]
async fn deleted_history_is_reported_as_incomplete() {
    let dir = tempfile::tempdir().unwrap();
    let mut short = config(dir.path());
    short.retention = Duration::from_millis(300);
    let core = Core::open(short.clone()).unwrap();
    let old: Vec<_> = (0..8).map(|i| (LogLevel::Info, "old message", i)).collect();
    core.journal.accept(&batch(&old)).unwrap();
    let end = Instant::now() + Duration::from_secs(20);
    // Каталог мог успеть обработать сегмент: удаляем его, чтобы rebuild видел только остаток.
    while core.snapshot().watermark < 8
        || !core.snapshot().closed.is_empty()
        || core.snapshot().active.is_some()
    {
        assert!(Instant::now() < end, "retention timed out");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    core.shutdown().unwrap();
    drop(core);
    std::fs::remove_file(dir.path().join("patterns.sqlite")).unwrap();
    let core = Core::open(config(dir.path())).unwrap();
    core.journal
        .accept(&batch(&[(LogLevel::Info, "fresh message", 10)]))
        .unwrap();
    processed(&core, 9).await;
    let response = patterns::search(&core, request(100, Sort::Count, &[]))
        .await
        .unwrap();
    assert!(!response.history_complete);
    assert_eq!((response.new_patterns, response.total_events), (None, 1));
    assert_eq!(response.patterns[0].is_new, None);
    assert_eq!(
        response.history_from,
        Some(format_timestamp(TS + 10 * SECOND))
    );
    core.shutdown().unwrap();
}
