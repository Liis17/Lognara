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
async fn actual_relay_delivers_to_core_and_all_public_routes_observe_the_log() {
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
    let url = format!("http://{}", listener.local_addr().unwrap());
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
            .post(relay_url)
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
    stop_relay.cancel();
    relay.await.unwrap().unwrap();
    stop_core.cancel();
    server.await.unwrap();
    core.shutdown().unwrap();
}
