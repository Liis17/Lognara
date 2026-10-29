use lognara_core::{
    analytics::{self, HistogramRequest},
    config::Config,
    model::format_timestamp,
    query::{self, SearchRequest, TextMode, TextQuery},
    storage::Core,
    wire,
};
use std::{
    collections::BTreeMap,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

const TS: i64 = 1_790_678_511_871_000_001;
const MESSAGES: [&str; 6] = [
    "alpha one",
    "beta two",
    "alpha three",
    "gamma four",
    "alpha five",
    "delta six",
];

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
    // Окно 3 с и тишина 2 с: после приёма есть не меньше 2 с до слияния.
    cfg.segment_age = Duration::from_secs(1);
    cfg.segment_compact = Duration::from_secs(3);
    cfg
}

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

fn wait(what: &str, mut condition: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(30);
    while !condition() {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn request(limit: usize, text: Option<&str>) -> SearchRequest {
    SearchRequest {
        from: format_timestamp(TS),
        to: format_timestamp(TS + 1),
        filters: BTreeMap::new(),
        text: text.map(|query| TextQuery {
            query: query.into(),
            mode: TextMode::All,
        }),
        limit,
        cursor: None,
    }
}

fn messages(core: &Core, request: SearchRequest, ascending: bool) -> (Vec<String>, Option<String>) {
    let response = query::search(
        core,
        request,
        ascending,
        Instant::now() + Duration::from_secs(10),
    )
    .unwrap();
    (
        response
            .events
            .into_iter()
            .map(|event| event.message)
            .collect(),
        response.next_cursor,
    )
}

fn histogram_total(core: &Core) -> u64 {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime
        .block_on(analytics::histogram(
            core,
            HistogramRequest {
                from: format_timestamp(TS - 60_000_000_000),
                to: format_timestamp(TS + 60_000_000_000),
                filters: BTreeMap::new(),
                interval_seconds: 60,
            },
        ))
        .unwrap()
        .buckets
        .iter()
        .map(|bucket| bucket.count)
        .sum()
}

#[test]
fn merged_window_keeps_results_cursor_and_readers_of_old_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(config(dir.path())).unwrap();
    let body = batch(&MESSAGES);
    core.journal.accept(&body).unwrap();
    wait("three sealed segments", || {
        core.snapshot().closed.len() == 3
    });
    let before = core.snapshot();
    assert_eq!(before.closed.len(), 3);
    let sources: Vec<_> = before.closed.iter().map(|s| s.path.clone()).collect();
    let (asc, _) = messages(&core, request(10, None), true);
    assert_eq!(asc, MESSAGES);
    let (desc, _) = messages(&core, request(10, None), false);
    let (alpha, _) = messages(&core, request(10, Some("alpha")), true);
    assert_eq!(alpha, ["alpha one", "alpha three", "alpha five"]);
    let (first_page, cursor) = messages(&core, request(2, None), true);
    assert_eq!(first_page, ["alpha one", "beta two"]);
    assert_eq!(histogram_total(&core), 6);

    wait("compaction", || core.snapshot().closed.len() == 1);
    let merged = core.snapshot().closed[0].clone();
    assert_eq!(merged.meta.rows, 6);
    assert_eq!(merged.meta.first_sequence, 1);
    assert_eq!(merged.meta.last_sequence, 6);
    assert_eq!(
        core.journal
            .metrics
            .segment_compactions
            .load(Ordering::Relaxed),
        1
    );
    // Старый снимок держит исходные каталоги и продолжает читать их.
    assert!(sources.iter().all(|path| path.exists()));
    assert_eq!(before.closed.len(), 3);
    drop(before);
    wait("source deletion", || {
        sources.iter().all(|path| !path.exists())
    });

    // Курсор, выданный до слияния, продолжает выдачу без пропусков и повторов.
    let mut next = request(2, None);
    next.cursor = cursor;
    let (second_page, cursor) = messages(&core, next, true);
    assert_eq!(second_page, ["alpha three", "gamma four"]);
    let mut next = request(2, None);
    next.cursor = cursor;
    let (third_page, cursor) = messages(&core, next, true);
    assert_eq!(third_page, ["alpha five", "delta six"]);
    assert!(cursor.is_none());

    assert_eq!(messages(&core, request(10, None), true).0, asc);
    assert_eq!(messages(&core, request(10, None), false).0, desc);
    assert_eq!(messages(&core, request(10, Some("alpha")), true).0, alpha);
    assert_eq!(histogram_total(&core), 6);
    let status = lognara_core::status::status(&core).unwrap();
    assert_eq!(status.storage.segments, 1);
    core.shutdown().unwrap();
    drop(merged);
    drop(core);

    let core = Core::open(config(dir.path())).unwrap();
    let snapshot = core.snapshot();
    assert_eq!(snapshot.closed.len(), 1);
    assert_eq!(snapshot.closed[0].meta.rows, 6);
    drop(snapshot);
    assert_eq!(messages(&core, request(10, None), true).0, asc);
    // Повтор той же пачки остаётся дубликатом после слияния.
    let receipt = core.journal.accept(&body).unwrap();
    assert_eq!(receipt.first_sequence, 1);
    assert_eq!(core.snapshot().watermark, 6);
    core.shutdown().unwrap();
}

#[test]
fn window_is_not_merged_before_it_ends() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config(dir.path());
    cfg.segment_compact = Duration::from_secs(86400);
    let core = Core::open(cfg).unwrap();
    core.journal.accept(&batch(&MESSAGES[..4])).unwrap();
    wait("visibility", || core.snapshot().watermark >= 4);
    // Текущие сутки не закончились: сегменты остаются раздельными.
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(core.snapshot().closed.len(), 2);
    core.shutdown().unwrap();
    drop(core);
    let core = Core::open(config(dir.path())).unwrap();
    wait("compaction", || core.snapshot().closed.len() == 1);
    assert_eq!(messages(&core, request(10, None), true).0, MESSAGES[..4]);
    core.shutdown().unwrap();
}
