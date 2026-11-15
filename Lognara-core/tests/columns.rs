use datafusion::arrow::{array::RecordBatch, datatypes::Schema};
use lognara_core::{columns, wire};
use std::sync::Arc;

#[test]
fn parquet_round_trip_preserves_every_field_and_selects_physical_rows() {
    let rows: Vec<_> = wire::decode(include_bytes!("fixtures/relay-batch.bin"), 1 << 20)
        .unwrap()
        .into_events(42, 12345)
        .collect();
    let mut second = rows[0].clone();
    second.sequence = 43;
    second.event.timestamp = -1;
    second.event.level = wire::LogLevel::Unknown;
    second.event.trace_id = None;
    second.event.parent_span_id = None;
    second.source.environment = None;
    let mut third = second.clone();
    third.sequence = 44;
    third.event.message = "японский 日本語\n".repeat(10_000);
    let expected = vec![rows[0].clone(), second, third];
    let batch = columns::encode(&expected).unwrap();
    assert_eq!(columns::decode(&batch).unwrap(), expected);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("logs.parquet");
    columns::write_parquet(&path, &[batch]).unwrap();
    let all: Vec<_> = columns::read_all(&path)
        .unwrap()
        .iter()
        .flat_map(|batch| columns::decode(batch).unwrap())
        .collect();
    assert_eq!(all, expected);
    assert_eq!(
        columns::read_rows(&path, &[0, 2]).unwrap(),
        vec![expected[0].clone(), expected[2].clone()]
    );
}

#[test]
fn reads_legacy_parquet_without_node_columns_as_unscoped_events() {
    let mut row = wire::decode(include_bytes!("fixtures/relay-batch.bin"), 1 << 20)
        .unwrap()
        .into_events(1, 123)
        .next()
        .unwrap();
    row.relay_id = Some(7);
    row.agent_id = Some(9);
    let current = columns::encode(&[row]).unwrap();
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
    let legacy = RecordBatch::try_new(Arc::new(Schema::new(fields)), arrays).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.parquet");
    columns::write_parquet_with(&path, legacy.schema(), &[legacy], "segment_during_parquet")
        .unwrap();

    let decoded = columns::read_rows(&path, &[0]).unwrap();
    assert_eq!(decoded.len(), 1);
    assert_eq!(decoded[0].relay_id, None);
    assert_eq!(decoded[0].agent_id, None);
}
