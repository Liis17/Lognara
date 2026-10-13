use lognara_agent::sources::{Format, SourceKind, Sources};

#[test]
fn accepts_file_sources_and_rejects_unsafe_or_ambiguous_settings() {
    let sources = Sources::parse(br#"{"sources":[{"id":"app","type":"file","include":["/var/log/app/*.log"],"format":"ndjson"}]}"#, false).unwrap();
    assert_eq!(sources.sources[0].kind, SourceKind::File);
    assert_eq!(sources.sources[0].format, Format::Ndjson);
    for json in [
        r#"{"sources":[{"id":"app","type":"file","include":["relative.log"]}]}"#,
        r#"{"sources":[{"id":"app","type":"file","include":["/var/*/*.log"]}]}"#,
        r#"{"sources":[{"id":"app","type":"stdout"}]}"#,
        r#"{"sources":[{"id":"app","type":"file","include":["/tmp/a"],"typo":1}]}"#,
        r#"{"sources":[{"id":"app","type":"file","include":["/tmp/a"],"format":"ndjson","multiline":{"start_pattern":"^x"}}]}"#,
        r#"{"sources":[{"id":"app","type":"file","include":["/tmp/a"]},{"id":"app","type":"file","include":["/tmp/b"]}]}"#,
    ] {
        assert!(Sources::parse(json.as_bytes(), false).is_err(), "{json}");
    }
}

#[test]
fn run_adds_default_streams_and_overrides_only_the_configured_stream() {
    let sources = Sources::parse(
        br#"{"sources":[{"id":"errors","type":"stderr","format":"ndjson"}]}"#,
        true,
    )
    .unwrap();
    assert_eq!(sources.sources.len(), 2);
    let stderr = sources.stream(SourceKind::Stderr).unwrap();
    assert_eq!(stderr.id, "errors");
    assert_eq!(stderr.format, Format::Ndjson);
    assert_eq!(
        sources.stream(SourceKind::Stdout).unwrap().format,
        Format::Text
    );
}

#[test]
fn capture_reservations_fit_the_default_memory_budget() {
    let mut config = lognara_agent::config::Config::from_lookup(|key| match key {
        "LOGNARA_SERVICE" | "LOGNARA_SERVER" | "LOGNARA_BACKEND" | "LOGNARA_RELAY_TOKEN" => {
            Some("test".into())
        }
        _ => None,
    })
    .unwrap();
    config.load_sources(None, true).unwrap();
    assert_eq!(config.max_buffer_bytes, 64 << 20);
    assert!(
        config.sources.reserved_bytes(config.max_record_bytes)
            + 2 * config.pipeline_bytes()
            + lognara_agent::config::INDEX_BYTES
            + 4 * config.metadata_bytes()
            <= config.max_buffer_bytes
    );
}
