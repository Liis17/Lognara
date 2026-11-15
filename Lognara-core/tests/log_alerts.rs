use std::{
    collections::{BTreeMap, HashMap},
    time::{Duration, Instant},
};

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use lognara_core::{
    api,
    config::Config,
    log_alerts::{self, AlertQueryRequest, AlertRule},
    query::{TextMode, TextQuery},
    storage::Core,
    wire::{CoreBatch, Event, Group, LogId, LogLevel, Source},
};
use tower::ServiceExt;

fn config(path: &std::path::Path) -> Config {
    let mut config = Config::from_lookup(|key| match key {
        "LOGNARA_ADMIN_TOKEN" => Some("admin".into()),
        "LOGNARA_QUERY_TOKEN" => Some("query".into()),
        _ => None,
    })
    .unwrap();
    config.data_dir = path.into();
    config.disk_reserve_bytes = 1;
    // Маленькие сегменты: правило должно пропускать закрытые сегменты до after_sequence.
    config.segment_rows = 2;
    config.refresh_interval = Duration::from_millis(20);
    config
}

fn batch(service: &str, events: &[(i64, LogLevel, &str)]) -> Vec<u8> {
    let batch = CoreBatch {
        dropped: 0,
        groups: vec![Group {
            source: Source {
                environment: None,
                server: "mail".into(),
                backend: "host".into(),
                service: service.into(),
                service_instance: None,
            },
            dropped: 0,
            events: events
                .iter()
                .map(|(timestamp, level, message)| Event {
                    id: LogId(uuid::Uuid::now_v7()),
                    timestamp: *timestamp,
                    ingested_at: *timestamp,
                    action: None,
                    level: *level,
                    message: (*message).into(),
                    trace_id: None,
                    span_id: None,
                    parent_span_id: None,
                    request_id: None,
                    attributes: HashMap::new(),
                })
                .collect(),
            agent: None,
        }],
    };
    zstd::encode_all(&rmp_serde::to_vec_named(&batch).unwrap()[..], 3).unwrap()
}

async fn wait(core: &Core, sequence: u64) {
    let end = Instant::now() + Duration::from_secs(10);
    while core.snapshot().watermark < sequence {
        assert!(Instant::now() < end && core.journal.ready());
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn rule(after: Option<u64>) -> AlertRule {
    AlertRule {
        id: "sudo".into(),
        filters: BTreeMap::from([("service".into(), vec!["host".into()])]),
        text: Some(TextQuery {
            query: "sudo command".into(),
            mode: TextMode::All,
        }),
        after_sequence: after,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn counts_only_matches_materialized_after_previous_watermark() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(config(dir.path())).unwrap();
    let now = lognara_core::model::now_nanos();
    core.journal
        .accept(&batch(
            "host",
            &[
                (now, LogLevel::Info, "sudo: admin COMMAND=/bin/ls"),
                (now, LogLevel::Info, "session opened"),
                (now, LogLevel::Info, "sudo: root COMMAND=/bin/sh"),
            ],
        ))
        .unwrap();
    wait(&core, 3).await;

    // Без after_sequence правило получает только базовую линию.
    let baseline = log_alerts::alert_query(
        core.clone(),
        AlertQueryRequest {
            rules: vec![rule(None)],
        },
    )
    .await
    .unwrap();
    assert_eq!(baseline.watermark, 3);
    assert_eq!(baseline.results[0].count, 0);

    // Позднее событие со старым временем и событие другого сервиса.
    core.journal
        .accept(&batch(
            "host",
            &[
                (
                    now - 86_400_000_000_000,
                    LogLevel::Info,
                    "sudo: late COMMAND=/usr/bin/id",
                ),
                (now + 1, LogLevel::Warn, "sudo: eve COMMAND=/tmp/xmrig"),
            ],
        ))
        .unwrap();
    core.journal
        .accept(&batch(
            "api",
            &[(now, LogLevel::Info, "sudo: app COMMAND=/bin/true")],
        ))
        .unwrap();
    wait(&core, 6).await;
    let next = log_alerts::alert_query(
        core.clone(),
        AlertQueryRequest {
            rules: vec![rule(Some(baseline.watermark))],
        },
    )
    .await
    .unwrap();
    assert_eq!(next.watermark, 6);
    let result = &next.results[0];
    assert_eq!(result.count, 2);
    let samples: Vec<_> = result.samples.iter().map(|e| e.message.as_str()).collect();
    assert_eq!(
        samples,
        [
            "sudo: eve COMMAND=/tmp/xmrig",
            "sudo: late COMMAND=/usr/bin/id"
        ]
    );

    // Повторная оценка с новым watermark ничего не находит.
    let again = log_alerts::alert_query(
        core.clone(),
        AlertQueryRequest {
            rules: vec![rule(Some(next.watermark))],
        },
    )
    .await
    .unwrap();
    assert_eq!(again.results[0].count, 0);
    assert!(again.results[0].samples.is_empty());

    // Фильтр уровня без текста.
    let warn = log_alerts::alert_query(
        core.clone(),
        AlertQueryRequest {
            rules: vec![AlertRule {
                id: "warn".into(),
                filters: BTreeMap::from([("level".into(), vec!["warn".into()])]),
                text: None,
                after_sequence: Some(0),
            }],
        },
    )
    .await
    .unwrap();
    assert_eq!(warn.results[0].count, 1);

    let invalid = log_alerts::alert_query(core.clone(), AlertQueryRequest { rules: vec![] }).await;
    assert!(invalid.is_err());

    let router = api::router(core.clone());
    for (token, status) in [("", StatusCode::UNAUTHORIZED), ("query", StatusCode::OK)] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/logs/alerts/query")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"rules":[{"id":"r","filters":{"service":["host"]},"text":{"query":"sudo"},"after_sequence":0}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), status);
        if status == StatusCode::OK {
            let body = to_bytes(response.into_body(), 1 << 20).await.unwrap();
            let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(value["results"][0]["count"], 4);
            assert_eq!(value["watermark"], 6);
        }
    }
    core.shutdown().unwrap();
}
