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
    wire,
};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};
use tower::ServiceExt;

fn config(path: &std::path::Path) -> Config {
    let mut cfg = Config::from_lookup(|key| match key {
        "LOGNARA_ADMIN_TOKEN" => Some("ingest".into()),
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

const TS: i64 = 1_790_678_511_871_000_001;

fn batch(messages: &[&str]) -> Vec<u8> {
    let mut batch = wire::decode(include_bytes!("fixtures/relay-batch.bin"), 1 << 20).unwrap();
    let base = batch.groups[0].events[0].clone();
    batch.groups[0].events = messages
        .iter()
        .map(|message| {
            let mut event = base.clone();
            event.message = (*message).into();
            event
        })
        .collect();
    zstd::encode_all(&rmp_serde::to_vec_named(&batch).unwrap()[..], 3).unwrap()
}

fn wait(core: &Core, sequence: u64) {
    let end = Instant::now() + Duration::from_secs(10);
    while core.snapshot().watermark < sequence {
        assert!(Instant::now() < end && core.journal.ready());
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn request() -> SearchRequest {
    SearchRequest {
        from: format_timestamp(TS),
        to: format_timestamp(TS + 1),
        filters: BTreeMap::new(),
        text: None,
        limit: 2,
        cursor: None,
    }
}

fn search(
    core: &Core,
    request: SearchRequest,
    ascending: bool,
) -> Result<query::SearchResponse, QueryError> {
    query::search(
        core,
        request,
        ascending,
        Instant::now() + Duration::from_secs(10),
    )
}

#[test]
fn cursor_preserves_snapshot_across_new_events_and_sealing_with_identical_timestamps() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let core = Core::open(cfg.clone()).unwrap();
    core.journal
        .accept(&batch(&["one", "two", "three", "four", "five"]))
        .unwrap();
    wait(&core, 5);
    let first = search(&core, request(), false).unwrap();
    assert_eq!(
        first
            .events
            .iter()
            .map(|event| event.message.as_str())
            .collect::<Vec<_>>(),
        ["five", "four"]
    );
    core.journal.accept(&batch(&["six"])).unwrap();
    wait(&core, 6);
    let mut next = request();
    next.cursor = first.next_cursor.clone();
    let second = search(&core, next.clone(), false).unwrap();
    assert_eq!(
        second
            .events
            .iter()
            .map(|event| event.message.as_str())
            .collect::<Vec<_>>(),
        ["three", "two"]
    );
    next.cursor = second.next_cursor;
    let third = search(&core, next, false).unwrap();
    assert_eq!(third.events.len(), 1);
    assert_eq!(third.events[0].message, "one");
    assert!(third.next_cursor.is_none());
    let mut changed = request();
    changed.cursor = first.next_cursor.clone();
    changed.filters.insert("level".into(), vec!["info".into()]);
    assert!(matches!(
        search(&core, changed, false),
        Err(QueryError::Invalid(_))
    ));
    core.shutdown().unwrap();
    drop(core);
    let core = Core::open(cfg).unwrap();
    let mut old = request();
    old.cursor = first.next_cursor;
    assert!(matches!(search(&core, old, false), Err(QueryError::Gone)));
    core.shutdown().unwrap();
}

#[test]
fn text_filters_trace_sort_and_exclusive_time_bound_work_in_memory_and_parquet() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(config(dir.path())).unwrap();
    core.journal
        .accept(&batch(&[
            "CONNECTION refused",
            "connection was refused",
            "доступ запрещён",
        ]))
        .unwrap();
    wait(&core, 3);
    let mut req = request();
    req.limit = 10;
    req.text = Some(TextQuery {
        query: "connection refused".into(),
        mode: TextMode::Phrase,
    });
    assert_eq!(search(&core, req.clone(), false).unwrap().events.len(), 1);
    req.text.as_mut().unwrap().mode = TextMode::All;
    assert_eq!(search(&core, req.clone(), false).unwrap().events.len(), 2);
    req.text = Some(TextQuery {
        query: "ДОСТУП".into(),
        mode: TextMode::All,
    });
    assert_eq!(
        search(&core, req.clone(), false).unwrap().events[0].message,
        "доступ запрещён"
    );
    req.text = None;
    req.filters
        .insert("service".into(), vec!["worker".into(), "api".into()]);
    req.filters.insert("level".into(), vec!["error".into()]);
    req.filters.insert(
        "trace_id".into(),
        vec!["8C21F0A4B6C8D0E12A4B6C8D0E1F3A5B".into()],
    );
    let events = search(&core, req.clone(), true).unwrap().events;
    assert_eq!(events[0].message, "CONNECTION refused");
    assert_eq!(events[2].message, "доступ запрещён");
    req.from = format_timestamp(TS - 1);
    req.to = format_timestamp(TS);
    assert!(search(&core, req.clone(), false).unwrap().events.is_empty());
    req.filters
        .insert("attributes.user_id".into(), vec!["18271".into()]);
    assert!(matches!(
        search(&core, req, false),
        Err(QueryError::Invalid(_))
    ));
    core.shutdown().unwrap();
}

#[test]
fn search_memory_budget_rejects_large_pages_in_arrow_and_parquet() {
    for segment_rows in [2, 100] {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = config(dir.path());
        cfg.segment_rows = segment_rows;
        cfg.search_memory_bytes = 100_000;
        let core = Core::open(cfg).unwrap();
        let message = "word ".repeat(1000);
        core.journal
            .accept(&batch(&[&message, &message, &message]))
            .unwrap();
        wait(&core, 3);
        let mut req = request();
        req.limit = 3;
        assert!(matches!(
            search(&core, req.clone(), false),
            Err(QueryError::Storage(_))
        ));
        req.limit = 1;
        assert_eq!(
            search(&core, req, false).unwrap().events[0].message,
            message
        );
        core.shutdown().unwrap();
    }
}

#[tokio::test]
async fn search_filters_and_json_events_include_resolved_node_identity() {
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
    let hash_a = Sha256::digest(agent_a.token.as_bytes()).into();
    let agent_a = core
        .journal
        .access
        .rotate_agent_token(agent_a.agent.id)
        .unwrap();
    let hash_b = Sha256::digest(agent_b.token.as_bytes()).into();
    let mut batch = wire::decode(include_bytes!("fixtures/relay-batch.bin"), 1 << 20).unwrap();
    let mut group_a = batch.groups[0].clone();
    let mut group_b = group_a.clone();
    group_a.agent = Some(wire::AgentHash(hash_a));
    group_a.events[0].message = "relay alpha".into();
    group_b.agent = Some(wire::AgentHash(hash_b));
    group_b.events[0].message = "relay beta".into();
    let mut unknown_group = group_a.clone();
    unknown_group.agent = Some(wire::AgentHash([0x77; 32]));
    unknown_group.events[0].message = "unknown agent".into();
    batch.groups = vec![group_a, group_b, unknown_group];
    let body = zstd::encode_all(&rmp_serde::to_vec_named(&batch).unwrap()[..], 3).unwrap();
    core.journal.accept(&body).unwrap();
    wait(&core, 3);

    let mut by_agent = request();
    by_agent.limit = 10;
    by_agent
        .filters
        .insert("agent_id".into(), vec![agent_a.agent.id.to_string()]);
    let events = search(&core, by_agent, false).unwrap().events;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].message, "relay alpha");
    assert_eq!(events[0].relay_id, Some(relay_a.relay.id));
    assert_eq!(events[0].agent_id, Some(agent_a.agent.id));

    let mut by_relay = request();
    by_relay.limit = 10;
    by_relay
        .filters
        .insert("relay_id".into(), vec![relay_b.relay.id.to_string()]);
    let events = search(&core, by_relay, false).unwrap().events;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].message, "relay beta");

    let mut unfiltered = request();
    unfiltered.limit = 10;
    let unknown = search(&core, unfiltered, false)
        .unwrap()
        .events
        .into_iter()
        .find(|event| event.message == "unknown agent")
        .unwrap();
    assert_eq!(unknown.relay_id, None);
    assert_eq!(unknown.agent_id, None);
    let unknown = serde_json::to_value(unknown).unwrap();
    assert!(unknown.get("relay_id").is_none());
    assert!(unknown.get("agent_id").is_none());

    let response = api::router(core.clone())
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/v1/traces/8c21f0a4b6c8d0e12a4b6c8d0e1f3a5b?from={}&to={}&agent_id={}",
                    format_timestamp(TS - 1),
                    format_timestamp(TS + 1),
                    agent_a.agent.id,
                ))
                .header("authorization", "Bearer query")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    let response: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(response["events"].as_array().unwrap().len(), 1);
    assert_eq!(response["events"][0]["agent_id"], agent_a.agent.id);
    core.shutdown().unwrap();
}
