use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use lognara_core::{
    api,
    config::Config,
    storage::Core,
    wire::{CoreBatch, Event, Group, LogId, LogLevel, Source},
};
use serde_json::{Value, json};
use std::{collections::HashMap, path::Path, time::Duration};
use tower::ServiceExt;
use uuid::Uuid;

fn config(path: &Path) -> Config {
    let mut config = Config::from_lookup(|key| match key {
        "LOGNARA_QUERY_TOKEN" => Some("query".into()),
        "LOGNARA_ADMIN_TOKEN" => Some("admin".into()),
        _ => None,
    })
    .unwrap();
    config.data_dir = path.into();
    config.disk_reserve_bytes = 1;
    config.segment_rows = 2;
    config.refresh_interval = Duration::from_millis(10);
    config
}

fn event(id: u128) -> Event {
    Event {
        id: LogId(Uuid::from_u128(id)),
        timestamp: 1_798_761_600_000_000_000 + id as i64,
        ingested_at: 1_798_761_600_000_000_000 + id as i64,
        action: None,
        level: LogLevel::Info,
        message: "source discovery fixture".into(),
        trace_id: None,
        span_id: None,
        parent_span_id: None,
        request_id: None,
        attributes: HashMap::new(),
    }
}

fn group_with_timestamp(
    id: u128,
    service: &str,
    server: &str,
    environment: Option<&str>,
    timestamp: i64,
) -> Group {
    let mut group = group(id, service, server, environment);
    group.events[0].timestamp = timestamp;
    group
}

fn group(id: u128, service: &str, server: &str, environment: Option<&str>) -> Group {
    Group {
        source: Source {
            environment: environment.map(str::to_owned),
            server: server.into(),
            backend: "test".into(),
            service: service.into(),
            service_instance: None,
        },
        dropped: 0,
        events: vec![event(id)],
    }
}

fn packed(groups: Vec<Group>) -> Vec<u8> {
    let body = rmp_serde::to_vec_named(&CoreBatch { dropped: 0, groups }).unwrap();
    zstd::encode_all(&body[..], 3).unwrap()
}

async fn send(router: &axum::Router, token: Option<&str>, body: Value) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method("POST")
        .uri("/v1/logs/sources")
        .header("content-type", "application/json");
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let response = router
        .clone()
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn source_lists_include_logs_only_sources_and_have_exact_filtered_pages() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(config(dir.path())).unwrap();
    core.journal
        .accept(&packed(vec![
            group(1, "app-a", "server-1", Some("prod")),
            group(2, "app-b", "server-2", Some("prod")),
            group(3, "app-a", "server-3", Some("test")),
            group_with_timestamp(4, "legacy-app", "old-server", None, 1),
        ]))
        .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while core.snapshot().watermark < 4 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "source rows did not publish"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let router = api::router(core.clone());
    let (status, first) = send(&router, Some("query"), json!({"field":"service","limit":1})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(first["values"], json!(["app-a"]));
    let cursor = first["next_cursor"].as_str().unwrap();

    let (status, second) = send(
        &router,
        Some("query"),
        json!({"field":"service","limit":1,"cursor":cursor}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(second["values"], json!(["app-b"]));
    assert!(second["next_cursor"].is_string());

    let (status, filtered) = send(
        &router,
        Some("query"),
        json!({"field":"server","filters":{"service":"app-a","environment":"prod"}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(filtered["values"], json!(["server-1"]));

    let (status, legacy) = send(
        &router,
        Some("query"),
        json!({"field":"service","filters":{"environment":"prod"},"limit":10}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(legacy["values"], json!(["app-a", "app-b"]));
    let (status, empty) = send(
        &router,
        Some("query"),
        json!({"field":"environment","filters":{"environment":"missing"}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(empty["values"], json!([]));
    assert!(empty["next_cursor"].is_null());

    let (status, _) = send(&router, None, json!({"field":"service"})).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    core.shutdown().unwrap();
}

#[tokio::test]
async fn source_lists_page_more_than_one_thousand_names_without_skipping() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(config(dir.path())).unwrap();
    let groups = (0..1005)
        .map(|index| {
            group(
                index + 1,
                &format!("service-{index:04}"),
                "server-1",
                Some("prod"),
            )
        })
        .collect();
    core.journal.accept(&packed(groups)).unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while core.snapshot().watermark < 1005 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "source rows did not publish"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let router = api::router(core.clone());
    let (status, first) = send(
        &router,
        Some("query"),
        json!({"field":"service","limit":1000}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(first["values"].as_array().unwrap().len(), 1000);
    assert_eq!(first["values"][0], "service-0000");
    assert_eq!(first["values"][999], "service-0999");
    let cursor = first["next_cursor"].as_str().unwrap();

    let (status, second) = send(
        &router,
        Some("query"),
        json!({"field":"service","limit":1000,"cursor":cursor}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(second["values"].as_array().unwrap().len(), 5);
    assert_eq!(second["values"][0], "service-1000");
    assert_eq!(second["values"][4], "service-1004");
    assert!(second["next_cursor"].is_null());
    core.shutdown().unwrap();
}
