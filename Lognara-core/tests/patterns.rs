use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use lognara_core::{
    api,
    config::Config,
    issues::{self, IssueRequest, Status},
    model::format_timestamp,
    pattern_identity,
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

#[tokio::test]
async fn structured_logs_fit_capacity_and_rebuild_old_catalog_and_indexes() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config(dir.path());
    cfg.pattern_max_entries = 4;
    cfg.pattern_catalog_bytes = 1 << 20;
    let core = Core::open(cfg.clone()).unwrap();
    let messages: Vec<_> = (0..42).map(|i| match i % 3 {
        0 => format!(r#"10.0.0.1 - - [19/Oct/2026:22:28:{i:02} +0000] "POST /v1/batches HTTP/1.1" 204 {i} "-" "-""#),
        1 => format!(r#"ServiceMetrics {{ ServiceName: "api", Metrics: [("requests_total": {i})], Timestamp: 10/23/2026 12:14:{i:02} }}"#),
        _ => format!(r#"ServiceMetrics {{ SchemaVersion: 2, ServiceName: "api", Counters: [("requests_total": {i})], Gauges: [], Timestamp: 11/15/2026 20:37:{i:02} }}"#),
    }).collect();
    let events: Vec<_> = messages
        .iter()
        .enumerate()
        .map(|(i, message)| (LogLevel::Info, message.as_str(), i as i64))
        .collect();
    core.journal.accept(&batch(&events)).unwrap();
    processed(&core, 42).await;
    let response = patterns::search(&core, request(100, Sort::Count, &[]))
        .await
        .unwrap();
    assert_eq!((response.total_events, response.total_patterns), (14, 1));
    let id = response.patterns[0].pattern_id.clone();
    core.shutdown().unwrap();
    let closed = core.snapshot().closed.clone();
    drop(core);

    // Эмулируем каталог прежней ревизии, файл которого больше бюджета, и старый индекс.
    let path = dir.path().join("patterns.sqlite");
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("PRAGMA user_version=3;").unwrap();
    for i in 0..8 {
        db.execute(
            "INSERT INTO patterns VALUES(?1,?2)",
            rusqlite::params![format!("stale-{i}"), "x".repeat(256 << 10)],
        )
        .unwrap();
    }
    drop(db);
    assert!(std::fs::metadata(&path).unwrap().len() > 1 << 20);
    for segment in &closed {
        let path = segment.path.join("index/meta.json");
        let mut meta: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        meta.as_object_mut().unwrap().remove("payload");
        std::fs::write(path, serde_json::to_vec(&meta).unwrap()).unwrap();
    }
    let core = Core::open(cfg).unwrap();
    processed(&core, 42).await;
    let response = patterns::search(&core, request(100, Sort::Count, &[]))
        .await
        .unwrap();
    assert_eq!((response.total_events, response.total_patterns), (14, 1));
    assert!(response.patterns.iter().all(|p| p.count == 14));
    assert!(std::fs::metadata(&path).unwrap().len() <= 1 << 20);
    let found = query::search(
        &core,
        SearchRequest {
            from: format_timestamp(TS),
            to: format_timestamp(TS + 100 * SECOND),
            filters: BTreeMap::from([("pattern_id".into(), vec![id])]),
            text: None,
            limit: 100,
            cursor: None,
        },
        false,
        Instant::now() + Duration::from_secs(20),
    )
    .unwrap();
    assert_eq!(found.events.len(), 14);
    assert!(!closed.is_empty());
    for segment in &closed {
        assert_eq!(
            tantivy::Index::open_in_dir(segment.path.join("index"))
                .unwrap()
                .load_metas()
                .unwrap()
                .payload
                .as_deref(),
            Some("lognara-pattern-normalization-3")
        );
    }
    core.shutdown().unwrap();
}

#[tokio::test]
async fn service_metrics_are_searchable_but_not_patterns_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(config(dir.path())).unwrap();
    let metric_v1 = r#"ServiceMetrics { ServiceName: "api", Metrics: [("requests_total": 1)], Timestamp: 10/23/2026 12:14:47 }"#;
    let metric_v2 = r#"ServiceMetrics { SchemaVersion: 2, ServiceName: "api", Counters: [("requests_total": 2)], Gauges: [], Timestamp: 11/15/2026 20:37:51 }"#;
    let events = [
        (LogLevel::Warn, "payment id=1 failed", 1),
        (LogLevel::Info, metric_v1, 2),
        (LogLevel::Error, metric_v2, 3),
        (LogLevel::Warn, "payment id=2 failed", 4),
    ];
    let body = batch(&events);
    let source = wire::decode(&body, 1 << 20)
        .unwrap()
        .groups
        .remove(0)
        .source;
    let environment = source.environment.clone();
    core.journal.accept(&body).unwrap();
    processed(&core, 4).await;
    core.shutdown().unwrap();
    drop(core);

    // Старый каталог мог сохранить ServiceMetrics до обновления Dock/Core.
    let path = dir.path().join("patterns.sqlite");
    let db = rusqlite::Connection::open(path).unwrap();
    let mut metric_ids = Vec::new();
    for (level, message, offset) in [("info", metric_v1, 2), ("error", metric_v2, 3)] {
        let identity =
            pattern_identity::identify(environment.as_deref(), &source.service, level, message);
        metric_ids.push(identity.id.clone());
        let metadata = json!({
            "id": identity.id,
            "environment": environment,
            "service": source.service,
            "level": level,
            "template": identity.template,
            "parts": identity.parts,
            "exact": identity.exact,
            "first": TS + offset * SECOND,
            "last": TS + offset * SECOND,
        });
        db.execute(
            "INSERT OR REPLACE INTO patterns(id, metadata) VALUES(?1, ?2)",
            rusqlite::params![metric_ids.last().unwrap(), metadata.to_string()],
        )
        .unwrap();
    }
    drop(db);

    let core = Core::open(config(dir.path())).unwrap();
    processed(&core, 4).await;
    let response = patterns::search(&core, request(100, Sort::Count, &[]))
        .await
        .unwrap();
    assert_eq!((response.total_events, response.total_patterns), (2, 1));
    assert_eq!(response.patterns[0].count, 2);
    assert_eq!(response.patterns[0].share, 1.0);
    assert!(metric_ids.iter().all(|id| {
        response
            .patterns
            .iter()
            .all(|pattern| &pattern.pattern_id != id)
    }));

    let detail = patterns::details(
        &core,
        &response.patterns[0].pattern_id,
        details(BTreeMap::new()),
    )
    .await
    .unwrap();
    assert_eq!((detail.pattern.count, detail.pattern.share), (2, 1.0));
    for id in &metric_ids {
        assert!(matches!(
            patterns::details(&core, id, details(BTreeMap::new())).await,
            Err(QueryError::NotFound)
        ));
    }

    let logs = query::search(
        &core,
        SearchRequest {
            from: format_timestamp(TS),
            to: format_timestamp(TS + 100 * SECOND),
            filters: BTreeMap::new(),
            text: None,
            limit: 100,
            cursor: None,
        },
        false,
        Instant::now() + Duration::from_secs(20),
    )
    .unwrap();
    assert_eq!(logs.events.len(), 4);
    assert_eq!(
        logs.events
            .iter()
            .filter(|event| event.message.starts_with("ServiceMetrics {"))
            .count(),
        2
    );
    assert!(
        response
            .patterns
            .iter()
            .all(|pattern| !pattern.template.starts_with("ServiceMetrics {"))
    );
    let found = issues::search(
        &core,
        IssueRequest {
            from: format_timestamp(TS),
            to: format_timestamp(TS + 100 * SECOND),
            filters: BTreeMap::new(),
            interval_seconds: 60,
            limit: 100,
            status: Status::Open,
            resolved: vec![],
            regressed: vec![],
        },
    )
    .await
    .unwrap();
    assert!(
        found
            .issues
            .iter()
            .any(|issue| issue.template.starts_with("ServiceMetrics {"))
    );
    core.shutdown().unwrap();
}

#[tokio::test]
async fn hwagent_metrics_are_not_patterns_but_same_text_from_app_is() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(config(dir.path())).unwrap();
    let mut body = wire::decode(include_bytes!("fixtures/relay-batch.bin"), 1 << 20).unwrap();
    let mut host = body.groups[0].clone();
    host.source.server = "mail-01".into();
    host.source.backend = "host".into();
    host.source.service = "host".into();
    let mut host_event = host.events[0].clone();
    host_event.id = wire::LogId(uuid::Uuid::now_v7());
    host_event.timestamp = TS + SECOND;
    host_event.message = "host metrics".into();
    host_event.level = LogLevel::Debug;
    host_event.attributes.insert(
        "Metrics".into(),
        json!({
            "SchemaVersion": 2,
            "ServiceName": "host",
            "Counters": {"net.eth0.rx_bytes": 1024},
            "Gauges": {"cpu.usage_percent": 25.0},
        }),
    );
    host.events = vec![host_event];

    let mut application = body.groups[0].clone();
    application.source.server = "mail-01".into();
    application.source.backend = "payments".into();
    application.source.service = "billing".into();
    let mut application_event = application.events[0].clone();
    application_event.id = wire::LogId(uuid::Uuid::now_v7());
    application_event.timestamp = TS + 2 * SECOND;
    application_event.message = "host metrics".into();
    application_event.attributes.clear();
    application.events = vec![application_event];
    body.groups = vec![host, application];

    let encoded = rmp_serde::to_vec_named(&body).unwrap();
    let compressed = zstd::encode_all(&encoded[..], 3).unwrap();
    core.journal.accept(&compressed).unwrap();
    processed(&core, 2).await;

    let response = patterns::search(&core, request(100, Sort::Count, &[]))
        .await
        .unwrap();
    assert_eq!((response.total_events, response.total_patterns), (1, 1));
    assert_eq!(response.patterns[0].template, "host metrics");
    core.shutdown().unwrap();
}

#[tokio::test]
async fn pattern_search_and_details_apply_node_filters() {
    use sha2::{Digest, Sha256};

    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(config(dir.path())).unwrap();
    let relay_a = core.journal.access.create_relay("relay-a").unwrap();
    let relay_b = core.journal.access.create_relay("relay-b").unwrap();
    let agent_a = core
        .journal
        .access
        .create_agent("agent", relay_a.relay.id)
        .unwrap();
    let agent_b = core
        .journal
        .access
        .create_agent("agent", relay_b.relay.id)
        .unwrap();
    let mut batch = wire::decode(include_bytes!("fixtures/relay-batch.bin"), 1 << 20).unwrap();
    let mut group_a = batch.groups[0].clone();
    let mut group_b = group_a.clone();
    group_a.agent = Some(wire::AgentHash(
        Sha256::digest(agent_a.token.as_bytes()).into(),
    ));
    group_a.events[0].timestamp = TS + SECOND;
    group_a.events[0].message = "payment id=11 failed after 5ms".into();
    group_b.agent = Some(wire::AgentHash(
        Sha256::digest(agent_b.token.as_bytes()).into(),
    ));
    group_b.events[0].timestamp = TS + 2 * SECOND;
    group_b.events[0].message = "payment id=12 failed after 7ms".into();
    batch.groups = vec![group_a, group_b];
    let body = zstd::encode_all(&rmp_serde::to_vec_named(&batch).unwrap()[..], 3).unwrap();
    core.journal.accept(&body).unwrap();
    processed(&core, 2).await;

    let mut request = request(10, Sort::Count, &[]);
    request
        .filters
        .insert("agent_id".into(), vec![agent_a.agent.id.to_string()]);
    let response = patterns::search(&core, request).await.unwrap();
    assert_eq!(response.total_events, 1);
    assert_eq!(response.total_patterns, 1);
    let pattern = &response.patterns[0];
    assert_eq!(pattern.count, 1);

    let details = patterns::details(
        &core,
        &pattern.pattern_id,
        details(BTreeMap::from([(
            "relay_id".into(),
            vec![relay_b.relay.id.to_string()],
        )])),
    )
    .await
    .unwrap();
    assert_eq!(details.pattern.count, 1);
    assert_eq!(details.examples[0].relay_id, Some(relay_b.relay.id));
    assert_eq!(details.examples[0].agent_id, Some(agent_b.agent.id));
    core.shutdown().unwrap();
}

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

#[tokio::test]
async fn retention_frees_catalog_entries_of_deleted_events() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config(dir.path());
    cfg.pattern_max_entries = 3;
    cfg.retention = Duration::from_secs(2);
    cfg.segment_age = Duration::from_secs(1);
    let core = Core::open(cfg).unwrap();
    let count = || -> i64 {
        rusqlite::Connection::open(dir.path().join("patterns.sqlite"))
            .unwrap()
            .query_row("SELECT count(*) FROM patterns", [], |row| row.get(0))
            .unwrap()
    };
    core.journal
        .accept(&batch(&[
            (LogLevel::Info, "payment alpha accepted", 0),
            (LogLevel::Info, "payment beta accepted", 1),
        ]))
        .unwrap();
    processed(&core, 2).await;
    assert_eq!(count(), 2);
    let end = Instant::now() + Duration::from_secs(20);
    while {
        let snapshot = core.snapshot();
        !snapshot.closed.is_empty() || snapshot.active.is_some()
    } {
        assert!(Instant::now() < end, "retention timed out");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // Записи снимаются и без нового приёма.
    while count() != 0 {
        assert!(Instant::now() < end, "catalog cleanup timed out");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // Без очистки четыре разных паттерна превысили бы лимит в три записи.
    core.journal
        .accept(&batch(&[
            (LogLevel::Info, "refund gamma issued", 3600),
            (LogLevel::Info, "refund delta issued", 3601),
        ]))
        .unwrap();
    processed(&core, 4).await;
    assert_eq!(count(), 2);
    core.shutdown().unwrap();
}
