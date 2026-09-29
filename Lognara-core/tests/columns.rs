use lognara_core::{columns, wire};

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
