use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use lognara_core::{api, config::Config, storage::Core};
use lognara_selflog::Record;
use serde_json::{Value, json};
use tower::ServiceExt;

async fn call(
    router: &Router,
    method: &str,
    path: &str,
    token: &str,
    body: Value,
) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

#[tokio::test]
async fn diagnostics_are_authorized_durable_searchable_and_deduplicated() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::from_lookup(|name| match name {
        "LOGNARA_ADMIN_TOKEN" => Some("admin".into()),
        "LOGNARA_QUERY_TOKEN" => Some("query".into()),
        _ => None,
    })
    .unwrap();
    config.data_dir = dir.path().into();
    config.disk_reserve_bytes = 1;
    config.refresh_interval = std::time::Duration::from_millis(20);
    let core = Core::open(config.clone()).unwrap();
    let relay = core.journal.access.create_relay("node").unwrap();
    let router = api::router(core.clone());
    let settings = "/v1/admin/self-logs/settings";
    assert_eq!(
        call(&router, "GET", settings, "query", Value::Null).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(
            &router,
            "PUT",
            settings,
            &relay.token,
            json!({"core":false,"dock":false,"relay":false})
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(
            &router,
            "PUT",
            settings,
            "admin",
            json!({"core":true,"dock":false,"relay":true})
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(
            &router,
            "GET",
            "/v1/self-logs/settings",
            &relay.token,
            Value::Null
        )
        .await
        .1["dock"],
        false
    );
    let timestamp = lognara_core::model::now_nanos();
    let record = Record {
        id: uuid::Uuid::now_v7(),
        timestamp,
        service: "lognara-relay".into(),
        server: "node".into(),
        level: "ERROR".into(),
        target: "lognara_relay::sender".into(),
        message: "core unavailable".into(),
        fields: [("error".into(), "connection refused".into())].into(),
    };
    let body = json!([record]);
    assert_eq!(
        call(&router, "POST", "/v1/self-logs", "query", body.clone())
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    for _ in 0..2 {
        assert_eq!(
            call(&router, "POST", "/v1/self-logs", &relay.token, body.clone())
                .await
                .0,
            StatusCode::NO_CONTENT
        );
    }
    assert_eq!(
        call(&router, "POST", "/v1/admin/self-logs", "admin", body)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while core.snapshot().watermark < 1 {
        assert!(std::time::Instant::now() < deadline);
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(core.snapshot().watermark, 1);
    let (_, found) = call(&router, "POST", "/v1/logs/search", "query", json!({
        "from":lognara_core::model::format_timestamp(timestamp - 1_000_000_000),
        "to":lognara_core::model::format_timestamp(timestamp + 1_000_000_000),
        "filters":{"service":["lognara-relay"]},"limit":10
    })).await;
    assert_eq!(found["events"].as_array().unwrap().len(), 1);
    assert_eq!(found["events"][0]["message"], "core unavailable");
    assert_eq!(
        found["events"][0]["attributes"]["selflog"]["fields"]["error"],
        "connection refused"
    );
    core.shutdown().unwrap();
    drop(router);
    drop(core);
    let reopened = Core::open(config).unwrap();
    assert!(!reopened.self_logs.get().dock);
    reopened.shutdown().unwrap();
}
