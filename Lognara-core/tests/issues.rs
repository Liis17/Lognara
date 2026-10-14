use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use lognara_core::{
    api,
    config::Config,
    issues::{
        self, IssueDetailsRequest, IssueRequest, IssueResponse, RecurrenceRequest, Status, Watch,
    },
    model::format_timestamp,
    query::{self, QueryError, SearchRequest},
    storage::Core,
    summary::{self, SummaryRequest},
    wire::{self, LogLevel},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
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

struct Case {
    server: &'static str,
    action: &'static str,
    level: LogLevel,
    message: String,
    attributes: Value,
    offset: i64,
}

fn stack(line: u32) -> String {
    format!(
        "System.NullReferenceException: Object reference not set\n   at InvoiceMapper.Map(Invoice src) in /src/InvoiceMapper.cs:line {line}\n   at InvoiceService.Sync(Guid id) in /src/InvoiceService.cs:line 41"
    )
}

fn null_reference(server: &'static str, offset: i64, line: u32) -> Case {
    Case {
        server,
        action: "invoice.sync",
        level: LogLevel::Error,
        message: format!("Mapping failed for invoice {offset}"),
        attributes: json!({"exception.type": "System.NullReferenceException", "exception.stacktrace": stack(line)}),
        offset,
    }
}

fn timeout(offset: i64) -> Case {
    Case {
        server: "s1",
        action: "charge",
        level: LogLevel::Error,
        message: format!("Timeout after 5ms id={offset}"),
        attributes: json!({}),
        offset,
    }
}

fn batch(cases: &[Case]) -> Vec<u8> {
    let mut batch = wire::decode(include_bytes!("fixtures/relay-batch.bin"), 1 << 20).unwrap();
    let base = batch.groups[0].clone();
    batch.groups = cases
        .iter()
        .map(|case| {
            let mut group = base.clone();
            group.source.service = "billing".into();
            group.source.server = case.server.into();
            let mut event = group.events[0].clone();
            event.id = wire::LogId(uuid::Uuid::now_v7());
            event.timestamp = TS + case.offset * SECOND;
            event.level = case.level;
            event.action = Some(case.action.into());
            event.message = case.message.clone();
            event.attributes =
                serde_json::from_value::<HashMap<String, Value>>(case.attributes.clone()).unwrap();
            group.events = vec![event];
            group
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
        assert!(Instant::now() < end, "issue catalog timed out");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn request(status: Status, resolved: &[&str], regressed: &[&str]) -> IssueRequest {
    IssueRequest {
        from: format_timestamp(TS),
        to: format_timestamp(TS + 100 * SECOND),
        filters: BTreeMap::new(),
        interval_seconds: 60,
        limit: 100,
        status,
        resolved: resolved.iter().map(|id| id.to_string()).collect(),
        regressed: regressed.iter().map(|id| id.to_string()).collect(),
    }
}

fn details(from: i64) -> IssueDetailsRequest {
    IssueDetailsRequest {
        from: format_timestamp(TS + from * SECOND),
        to: format_timestamp(TS + 100 * SECOND),
        filters: BTreeMap::new(),
        interval_seconds: 60,
    }
}

fn ids(response: &IssueResponse) -> Vec<&str> {
    response
        .issues
        .iter()
        .map(|issue| issue.fingerprint.as_str())
        .collect()
}

#[tokio::test]
async fn issues_group_errors_and_report_statuses() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(config(dir.path())).unwrap();
    let mut cases = vec![
        null_reference("s1", -50, 80),
        null_reference("s1", 1, 84),
        null_reference("s2", 2, 85),
        null_reference("s2", 3, 86),
        null_reference("s1", 4, 84),
        timeout(5),
        timeout(6),
        timeout(7),
        Case {
            server: "s3",
            action: "charge",
            level: LogLevel::Fatal,
            message: "Ledger crashed".into(),
            attributes: json!({"exception.type": "LedgerException"}),
            offset: 8,
        },
    ];
    for offset in 9..12 {
        cases.push(Case {
            level: LogLevel::Warn,
            ..timeout(offset)
        });
    }
    core.journal.accept(&batch(&cases)).unwrap();
    processed(&core, 12).await;

    let open = issues::search(&core, request(Status::Open, &[], &[]))
        .await
        .unwrap();
    assert!(open.history_complete && open.watermark == 12);
    assert_eq!(
        (
            open.counts.open,
            open.counts.new,
            open.counts.regressed,
            open.counts.resolved
        ),
        (3, Some(2), 0, 0)
    );
    assert_eq!(open.status_events, 8);
    let [null, slow, ledger] = &open.issues[..] else {
        panic!("expected three issues");
    };
    assert_eq!((null.count, null.servers, null.is_new), (4, 2, Some(false)));
    assert_eq!(
        null.exception_type.as_deref(),
        Some("System.NullReferenceException")
    );
    assert_eq!(null.frames.len(), 2);
    assert_eq!(null.first_seen, format_timestamp(TS - 50 * SECOND));
    assert_eq!(null.last_seen, format_timestamp(TS + 4 * SECOND));
    assert_eq!(null.action.as_deref(), Some("invoice.sync"));
    assert_eq!(
        (
            slow.count,
            slow.servers,
            slow.is_new,
            slow.exception_type.clone()
        ),
        (3, 1, Some(true), None)
    );
    assert_eq!(slow.template, "Timeout after <duration_1> id=<id_1>");
    assert_eq!((ledger.count, ledger.is_new), (1, Some(true)));

    // Статусы Dock: решённая не входит в открытые, вернувшаяся — подмножество открытых.
    let (a, b, c) = (
        null.fingerprint.as_str(),
        slow.fingerprint.as_str(),
        ledger.fingerprint.as_str(),
    );
    let statused = issues::search(&core, request(Status::Open, &[b], &[a]))
        .await
        .unwrap();
    assert_eq!(
        (
            statused.counts.open,
            statused.counts.new,
            statused.counts.regressed,
            statused.counts.resolved
        ),
        (2, Some(1), 1, 1)
    );
    assert_eq!(ids(&statused), [a, c]);
    let new = issues::search(&core, request(Status::New, &[b], &[a]))
        .await
        .unwrap();
    assert_eq!(ids(&new), [c]);
    let back = issues::search(&core, request(Status::Regressed, &[b], &[a]))
        .await
        .unwrap();
    assert_eq!((ids(&back), back.status_events), (vec![a], 4));
    let mut limited = request(Status::Open, &[], &[]);
    limited.limit = 1;
    let limited = issues::search(&core, limited).await.unwrap();
    assert_eq!((ids(&limited), limited.counts.open), (vec![a], 3));

    // Решённая без событий в периоде видна с нулевыми числами.
    let mut later = request(Status::Resolved, &[b], &[]);
    later.from = format_timestamp(TS + 50 * SECOND);
    let later = issues::search(&core, later).await.unwrap();
    assert_eq!(later.counts.open, 0);
    assert_eq!((ids(&later), later.issues[0].count), (vec![b], 0));
    let mut other = request(Status::Resolved, &[b], &[]);
    other.filters = BTreeMap::from([("service".into(), vec!["web".into()])]);
    assert_eq!(
        issues::search(&core, other).await.unwrap().counts.resolved,
        0
    );
    let mut level = request(Status::Open, &[], &[]);
    level.filters = BTreeMap::from([("level".into(), vec!["error".into()])]);
    assert!(matches!(
        issues::search(&core, level).await,
        Err(QueryError::Invalid(_))
    ));

    // fingerprint — точный фильтр поиска и агрегатов.
    let filters = BTreeMap::from([(
        "fingerprint".to_owned(),
        vec![a.to_uppercase().replace("F1_", "f1_")],
    )]);
    let found = query::search(
        &core,
        SearchRequest {
            from: format_timestamp(TS - 100 * SECOND),
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
    assert_eq!(found.events.len(), 5);
    let total = summary::summary(
        core.clone(),
        SummaryRequest {
            from: format_timestamp(TS),
            to: format_timestamp(TS + 100 * SECOND),
            filters,
            text: None,
            interval_seconds: 60,
            search_limit: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(total.total, 4);

    let detail = issues::details(&core, a, details(0)).await.unwrap();
    assert_eq!((detail.issue.count, detail.issue.servers), (4, 2));
    assert_eq!(
        detail
            .servers
            .iter()
            .map(|server| (server.value.as_str(), server.count))
            .collect::<Vec<_>>(),
        [("s1", 2), ("s2", 2)]
    );
    assert_eq!((detail.other_servers, detail.examples.len()), (0, 3));
    assert_eq!(
        detail.examples[0].timestamp,
        format_timestamp(TS + 4 * SECOND)
    );
    assert_eq!(
        detail.examples[0].attributes["exception.type"],
        "System.NullReferenceException"
    );
    let quiet = issues::details(&core, b, details(50)).await.unwrap();
    assert_eq!((quiet.issue.count, quiet.examples.len()), (0, 0));
    assert!(matches!(
        issues::details(&core, &format!("f1_{}", "0".repeat(64)), details(0)).await,
        Err(QueryError::NotFound)
    ));
    assert!(matches!(
        issues::details(&core, "f1_xyz", details(0)).await,
        Err(QueryError::Invalid(_))
    ));

    let response = api::router(core.clone())
        .oneshot(
            Request::post("/v1/issues/search")
                .header("authorization", "Bearer query")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"from": format_timestamp(TS), "to": format_timestamp(TS + 100 * SECOND), "status": "regressed", "regressed": [a]}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // Рестарт сохраняет fingerprint и lifetime-границы.
    core.shutdown().unwrap();
    drop(core);
    let core = Core::open(config(dir.path())).unwrap();
    processed(&core, 12).await;
    let again = issues::search(&core, request(Status::Open, &[], &[]))
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(&again.issues).unwrap(),
        serde_json::to_value(&open.issues).unwrap()
    );
    core.shutdown().unwrap();
}

/// Проходит сегменты до текущего watermark и складывает совпадения по fingerprint.
async fn drain(
    core: &Core,
    after: u64,
    watch: &[(&str, i64)],
) -> (u64, Vec<(String, String, u64)>) {
    let mut after = after;
    let mut found: BTreeMap<String, (String, u64)> = BTreeMap::new();
    loop {
        let response = issues::recurrences(
            core,
            RecurrenceRequest {
                after,
                issues: watch
                    .iter()
                    .map(|(fingerprint, since)| Watch {
                        fingerprint: fingerprint.to_string(),
                        since: format_timestamp(TS + since * SECOND),
                    })
                    .collect(),
            },
        )
        .await
        .unwrap();
        for item in response.recurrences {
            let entry = found
                .entry(item.fingerprint)
                .or_insert((item.first_seen.clone(), 0));
            entry.0 = entry.0.clone().min(item.first_seen);
            entry.1 += item.count;
        }
        after = response.watermark;
        if after >= response.core_watermark {
            let found = found
                .into_iter()
                .map(|(id, (first, count))| (id, first, count))
                .collect();
            return (after, found);
        }
    }
}

#[tokio::test]
async fn recurrences_need_new_publication_and_time() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(config(dir.path())).unwrap();
    let first: Vec<_> = (0..10).map(|n| null_reference("s1", n, 84)).collect();
    core.journal.accept(&batch(&first)).unwrap();
    processed(&core, 10).await;
    let a = issues::search(&core, request(Status::Open, &[], &[]))
        .await
        .unwrap()
        .issues[0]
        .fingerprint
        .clone();

    // Без наблюдаемых проблем ответ сразу даёт текущий watermark.
    assert_eq!(drain(&core, 0, &[]).await, (10, vec![]));
    let (watermark, found) = drain(&core, 0, &[(&a, 5)]).await;
    assert_eq!(watermark, 10);
    assert_eq!(found, [(a.clone(), format_timestamp(TS + 5 * SECOND), 5)]);

    // После checkpoint: позднее событие со старым временем не считается, новое — считается.
    core.journal
        .accept(&batch(&[
            null_reference("s1", 3, 84),
            timeout(30),
            null_reference("s2", 40, 99),
        ]))
        .unwrap();
    processed(&core, 13).await;
    let (watermark, found) = drain(&core, 10, &[(&a, 20)]).await;
    assert_eq!(watermark, 13);
    assert_eq!(found, [(a.clone(), format_timestamp(TS + 40 * SECOND), 1)]);
    // Хранилище начато заново: watermark меньше checkpoint Dock.
    assert_eq!(drain(&core, 50, &[(&a, 20)]).await, (13, vec![]));
    assert!(matches!(
        issues::recurrences(
            &core,
            RecurrenceRequest {
                after: 0,
                issues: vec![
                    Watch {
                        fingerprint: a.clone(),
                        since: format_timestamp(TS)
                    },
                    Watch {
                        fingerprint: a.clone(),
                        since: format_timestamp(TS)
                    },
                ],
            },
        )
        .await,
        Err(QueryError::Invalid(_))
    ));
    core.shutdown().unwrap();
}
