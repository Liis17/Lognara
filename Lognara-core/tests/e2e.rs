use lognara_core::{api, config::Config, storage::Core};
use lognara_relay::{
    agent_wire::{Batch, Payload, Record},
    config::Config as RelayConfig,
    spool::Spool,
};
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn actual_pipeline_recovers_after_core_outage_and_public_routes_observe_logs() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::from_lookup(|key| match key {
        "LOGNARA_INGEST_TOKEN" => Some("ingest".into()),
        "LOGNARA_QUERY_TOKEN" => Some("query".into()),
        _ => None,
    })
    .unwrap();
    config.data_dir = dir.path().join("core");
    config.disk_reserve_bytes = 1;
    config.refresh_interval = Duration::from_millis(20);
    let core = Core::open(config).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let core_addr = listener.local_addr().unwrap();
    let url = format!("http://{core_addr}");
    let stop_core = CancellationToken::new();
    let stop = stop_core.clone();
    let router = api::router(core.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(stop.cancelled_owned())
            .await
            .unwrap()
    });
    let relay_config = RelayConfig::from_lookup(|key| match key {
        "LOGNARA_CORE_URL" => Some(format!("{url}/v1/batches")),
        "LOGNARA_CORE_TOKEN" => Some("ingest".into()),
        "LOGNARA_RELAY_TOKEN" => Some("relay-secret".into()),
        "LOGNARA_FLUSH_INTERVAL_MS" => Some("20".into()),
        "LOGNARA_BATCH_SIZE" => Some("1".into()),
        _ => None,
    })
    .unwrap();
    let spool = Spool::open(&dir.path().join("spool"), 1 << 20)
        .await
        .unwrap();
    let relay_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let relay_url = format!("http://{}/v1/batches", relay_listener.local_addr().unwrap());
    let stop_relay = CancellationToken::new();
    let stop = stop_relay.clone();
    let relay = tokio::spawn(lognara_relay::run(
        relay_config,
        relay_listener,
        spool,
        stop,
    ));
    let batch = Batch { service:"api".into(), server:"srv".into(), backend:"backend".into(), environment:Some("test".into()), service_instance:None,
        sent_at:1_790_678_511_894_000_001, dropped:0, records:vec![Record { received_at:1_790_678_511_894_000_001,
            payload:Payload::Json(r#"{"id":"019d3f8e-7c2a-7b3e-9f1d-2a4b6c8d0e1f","level":"error","message":"relay to core","trace_id":"8c21f0a4b6c8d0e12a4b6c8d0e1f3a5b"}"#.into()) }] };
    let body = zstd::encode_all(&rmp_serde::to_vec_named(&batch).unwrap()[..], 3).unwrap();
    let client = reqwest::Client::new();
    assert_eq!(
        client
            .post(&relay_url)
            .bearer_auth("relay-secret")
            .header("content-type", "application/msgpack")
            .header("content-encoding", "zstd")
            .body(body)
            .send()
            .await
            .unwrap()
            .status(),
        202
    );
    let range = serde_json::json!({"from":"2026-09-29T00:00:00Z","to":"2026-09-30T00:00:00Z"});
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let result: serde_json::Value = client
            .post(format!("{url}/v1/logs/search"))
            .bearer_auth("query")
            .json(&range)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        if result["events"].as_array().unwrap().len() == 1 {
            assert_eq!(result["events"][0]["message"], "relay to core");
            assert_eq!(
                result["events"][0]["ingested_at"],
                "2026-09-29T10:41:51.894000001Z"
            );
            break;
        }
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let trace: serde_json::Value = client
        .get(format!("{url}/v1/traces/8c21f0a4b6c8d0e12a4b6c8d0e1f3a5b"))
        .query(&[
            ("from", "2026-09-29T00:00:00Z"),
            ("to", "2026-09-30T00:00:00Z"),
        ])
        .bearer_auth("query")
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(trace["events"].as_array().unwrap().len(), 1);
    let mut stats = range.clone();
    stats["group_by"] = serde_json::json!(["service"]);
    let result: serde_json::Value = client
        .post(format!("{url}/v1/stats/group-by"))
        .bearer_auth("query")
        .json(&stats)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(result["groups"][0]["count"], 1);
    let mut histogram = range.clone();
    histogram["interval_seconds"] = serde_json::json!(3600);
    assert!(
        client
            .post(format!("{url}/v1/stats/histogram"))
            .bearer_auth("query")
            .json(&histogram)
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    let metrics = client
        .get(format!("{url}/metrics"))
        .bearer_auth("query")
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(metrics.contains("lognara_core_accepted_events_total 1\n"));
    assert_eq!(
        client
            .post(format!("{url}/v1/logs/search"))
            .bearer_auth("ingest")
            .json(&range)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let mut invalid = range.clone();
    invalid["text"] = serde_json::json!({"query":"must not be ignored"});
    assert_eq!(
        client
            .post(format!("{url}/v1/stats/histogram"))
            .bearer_auth("query")
            .json(&invalid)
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    // Настоящий agent принимает 202 при остановленном HTTP core.
    stop_core.cancel();
    server.await.unwrap();
    let mut agent_config = lognara_agent::config::Config::from_lookup(|key| match key {
        "LOGNARA_SERVICE" => Some("api".into()),
        "LOGNARA_SERVER" => Some("srv".into()),
        "LOGNARA_BACKEND" => Some("backend".into()),
        "LOGNARA_RELAY_TOKEN" => Some("relay-secret".into()),
        "LOGNARA_RELAY_URL" => Some(relay_url.clone()),
        "LOGNARA_BATCH_SIZE" => Some("1".into()),
        "LOGNARA_FLUSH_INTERVAL_MS" => Some("20".into()),
        _ => None,
    })
    .unwrap();
    agent_config.spool_dir = dir.path().join("agent-spool");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let agent_url = format!("http://{}/v1/logs", listener.local_addr().unwrap());
    let stop_agent = CancellationToken::new();
    let agent = tokio::spawn(lognara_agent::run(
        agent_config,
        listener,
        stop_agent.clone(),
    ));
    for number in 0..8 {
        let response = client
            .post(&agent_url)
            .header("content-type", "text/plain")
            .body(format!("agent delivery {number}"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 202);
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        std::fs::read_dir(dir.path().join("spool"))
            .unwrap()
            .any(|item| item
                .unwrap()
                .path()
                .extension()
                .is_some_and(|ext| ext == "group"))
    );
    let listener = TcpListener::bind(core_addr).await.unwrap();
    let stop_core = CancellationToken::new();
    let stop = stop_core.clone();
    let router = api::router(core.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(stop.cancelled_owned())
            .await
            .unwrap();
    });
    let query = serde_json::json!({"from":"2020-01-01T00:00:00Z","to":"2100-01-01T00:00:00Z",
        "text":{"query":"agent delivery","mode":"phrase"}});
    let expected: std::collections::HashSet<_> =
        (0..8).map(|n| format!("agent delivery {n}")).collect();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let result: serde_json::Value = client
            .post(format!("{url}/v1/logs/search"))
            .bearer_auth("query")
            .json(&query)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        let delivered: std::collections::HashSet<_> = result["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|event| event["message"].as_str().unwrap().to_owned())
            .collect();
        if expected.is_subset(&delivered) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "confirmed agent logs missing after core recovery"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    stop_agent.cancel();
    agent.await.unwrap().unwrap();
    stop_relay.cancel();
    relay.await.unwrap().unwrap();
    stop_core.cancel();
    server.await.unwrap();
    core.shutdown().unwrap();
}
