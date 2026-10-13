use lognara_agent::framing::Framer;
use lognara_agent::sources::{Format, Multiline, Source, SourceKind};
use lognara_agent::wire::Payload;

#[test]
fn frames_crlf_empty_json_and_binary_without_flushing_an_active_tail() {
    let mut source = Source::stream(SourceKind::Stdout);
    source.format = Format::Ndjson;
    let mut reader = Framer::new(&source, 1024, 0, None).unwrap();
    let records = reader.feed(b"{\"message\":\"hi\"}\r\n\n{bad}\n\xff\ntail");
    assert_eq!(records.len(), 4);
    assert_eq!(
        records[0].payload,
        Payload::Json("{\"message\":\"hi\"}".into())
    );
    assert_eq!(records[1].payload, Payload::Text("".into()));
    assert_eq!(records[2].payload, Payload::Text("{bad}".into()));
    assert_eq!(records[3].payload, Payload::Binary(vec![255]));
    assert!(reader.idle().is_none());
    assert_eq!(
        reader.finish_all().pop().unwrap().payload,
        Payload::Text("tail".into())
    );
}

#[test]
fn multiline_keeps_boundaries_and_resumes_fragmented_utf8_from_checkpoint() {
    let mut source = Source::stream(SourceKind::Stderr);
    source.multiline = Some(Multiline {
        start_pattern: "^ERROR".into(),
        flush_timeout_ms: 1000,
    });
    let mut reader = Framer::new(&source, 1024, 0, None).unwrap();
    let records = reader.feed(b"ERROR first\n  trace\nERROR next\n");
    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0].payload,
        Payload::Text("ERROR first\n  trace".into())
    );
    assert_eq!(records[0].end, 20);
    assert_eq!(
        reader.idle().unwrap().payload,
        Payload::Text("ERROR next".into())
    );

    let source = Source::stream(SourceKind::Stdout);
    let mut reader = Framer::new(&source, 8, 0, None).unwrap();
    let input = "абвгдежзикл\n".as_bytes();
    let first = reader.feed(&input[..18]).remove(0);
    assert!(first.continued);
    let mut reader = Framer::new(&source, 8, first.end, first.continuation.clone()).unwrap();
    let mut records = vec![first];
    records.extend(reader.feed(&input[records[0].end as usize..]));
    let text = records
        .iter()
        .map(|r| match &r.payload {
            Payload::Text(t) => t.as_str(),
            _ => panic!("UTF-8 fragment became binary"),
        })
        .collect::<String>();
    assert_eq!(text, "абвгдежзикл");
    assert!(!records.last().unwrap().continued);
}

#[test]
fn fragments_never_exceed_the_limit_and_multiline_headers_are_checked_before_joining() {
    let mut source = Source::stream(SourceKind::Stdout);
    source.multiline = Some(Multiline {
        start_pattern: "^E".into(),
        flush_timeout_ms: 1000,
    });
    let mut reader = Framer::new(&source, 8, 0, None).unwrap();
    let mut records = reader.feed(b"E 12345\nE second\nE abcdefghijklmnop\n");
    records.extend(reader.finish_all());
    assert_eq!(records[0].payload, Payload::Text("E 12345".into()));
    assert_eq!(records[1].payload, Payload::Text("E second".into()));
    for record in &records {
        match &record.payload {
            Payload::Text(s) => assert!(s.len() <= 8),
            _ => panic!("expected text"),
        }
    }
}
