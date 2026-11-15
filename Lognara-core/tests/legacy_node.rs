use datafusion::arrow::{array::RecordBatch, datatypes::Schema};
use lognara_core::{
    analytics::{self, HistogramRequest},
    columns,
    config::Config,
    query::{self, SearchRequest},
    storage::Core,
};
use std::{
    fs,
    sync::Arc,
    time::{Duration, Instant},
};
use tantivy::{
    Index,
    schema::{
        FAST, INDEXED, IndexRecordOption, STRING, Schema as TantivySchema, TextFieldIndexing,
        TextOptions,
    },
};

fn config(path: &std::path::Path) -> Config {
    let mut config = Config::from_lookup(|key| match key {
        "LOGNARA_ADMIN_TOKEN" => Some("admin".into()),
        "LOGNARA_QUERY_TOKEN" => Some("query".into()),
        _ => None,
    })
    .unwrap();
    config.data_dir = path.into();
    config.disk_reserve_bytes = 1;
    config.segment_rows = 1;
    config.refresh_interval = Duration::from_millis(20);
    config
}

fn wait(core: &Core) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while core.snapshot().watermark < 1 || core.snapshot().closed.is_empty() {
        assert!(Instant::now() < deadline && core.journal.ready());
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn legacy_batch(current: &RecordBatch) -> RecordBatch {
    let fields = current
        .schema()
        .fields()
        .iter()
        .filter(|field| field.name() != "relay_id" && field.name() != "agent_id")
        .map(|field| field.as_ref().clone())
        .collect::<Vec<_>>();
    let arrays = current
        .schema()
        .fields()
        .iter()
        .zip(current.columns())
        .filter(|(field, _)| field.name() != "relay_id" && field.name() != "agent_id")
        .map(|(_, array)| array.clone())
        .collect::<Vec<_>>();
    RecordBatch::try_new(Arc::new(Schema::new(fields)), arrays).unwrap()
}

fn legacy_index_schema() -> TantivySchema {
    let mut builder = TantivySchema::builder();
    builder.add_i64_field("timestamp", INDEXED | FAST);
    builder.add_u64_field("sequence", INDEXED | FAST);
    builder.add_u64_field("row_id", FAST);
    builder.add_u64_field("level", INDEXED | FAST);
    for field in lognara_core::index::EXACT_FIELDS
        .iter()
        .filter(|field| **field != "relay_id" && **field != "agent_id")
    {
        builder.add_text_field(field, STRING);
    }
    builder.add_text_field(
        "message",
        TextOptions::default().set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer("log_text")
                .set_index_option(IndexRecordOption::WithFreqsAndPositions),
        ),
    );
    builder.build()
}

#[tokio::test]
async fn old_parquet_and_tantivy_schema_load_with_node_fields_unset() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    let core = Core::open(config.clone()).unwrap();
    let body = include_bytes!("fixtures/relay-batch.bin");
    core.journal.accept(body).unwrap();
    wait(&core);
    let segment = core.snapshot().closed[0].path.clone();
    core.shutdown().unwrap();
    drop(core);

    let parquet = segment.join("logs.parquet");
    let mut batches = columns::read_all(&parquet).unwrap();
    let batch = batches.remove(0);
    let legacy = legacy_batch(&batch);
    fs::remove_file(&parquet).unwrap();
    columns::write_parquet_with(
        &parquet,
        legacy.schema(),
        &[legacy],
        "segment_during_parquet",
    )
    .unwrap();

    let index = segment.join("index");
    fs::remove_dir_all(&index).unwrap();
    fs::create_dir_all(&index).unwrap();
    Index::create_in_dir(&index, legacy_index_schema()).unwrap();

    let core = Core::open(config).unwrap();
    let found = query::search(
        &core,
        SearchRequest {
            from: "1970-01-01T00:00:00Z".into(),
            to: "2100-01-01T00:00:00Z".into(),
            filters: Default::default(),
            text: None,
            limit: 10,
            cursor: None,
        },
        false,
        Instant::now() + Duration::from_secs(10),
    )
    .unwrap();
    assert_eq!(found.events.len(), 1);
    assert_eq!(found.events[0].relay_id, None);
    assert_eq!(found.events[0].agent_id, None);

    let histogram = analytics::histogram(
        &core,
        HistogramRequest {
            from: "2026-09-29T10:41:51Z".into(),
            to: "2026-09-29T10:41:52Z".into(),
            filters: [
                ("relay_id".into(), vec!["1".into()]),
                ("agent_id".into(), vec!["1".into()]),
            ]
            .into(),
            interval_seconds: 1,
        },
    )
    .await
    .unwrap();
    assert!(histogram.buckets.iter().all(|bucket| bucket.count == 0));
    core.shutdown().unwrap();
}
