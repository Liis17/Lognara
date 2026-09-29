use lognara_core::{
    config::Config,
    model::LogEvent,
    wire::{self, DecodeError},
};

#[test]
fn decodes_real_relay_encoder_and_preserves_nanoseconds_and_attributes() {
    use lognara_relay::{
        agent_wire::{Payload, Record},
        core_wire::{self, CoreBatch, Group, Source},
        normalize,
    };
    let event = normalize::event(Record { received_at: 1_790_678_511_894_000_001,
        payload: Payload::Json(r#"{"id":"019d3f8e-7c2a-7b3e-9f1d-2a4b6c8d0e1f","timestamp":"2026-09-29T10:41:51.871000001Z","level":"error","message":"oops","attributes":{"large":18446744073709551615,"nested":[true,null,{"x":0.5}]}}"#.into()) });
    let body = core_wire::encode(&CoreBatch {
        dropped: 2,
        groups: vec![Group {
            source: Source {
                server: "srv".into(),
                backend: "backend".into(),
                service: "api".into(),
                environment: None,
                service_instance: None,
            },
            dropped: 3,
            events: vec![event],
        }],
    });
    let batch = wire::decode(&body, 4096).unwrap();
    assert_eq!(batch.dropped, 2);
    assert_eq!(batch.groups[0].dropped, 3);
    let json =
        serde_json::to_value(LogEvent::from(batch.into_events(1, 7).next().unwrap())).unwrap();
    assert_eq!(json["timestamp"], "2026-09-29T10:41:51.871000001Z");
    assert_eq!(json["ingested_at"], "2026-09-29T10:41:51.894000001Z");
    assert_eq!(json["id"], "019d3f8e-7c2a-7b3e-9f1d-2a4b6c8d0e1f");
    assert_eq!(json["attributes"]["large"], u64::MAX);
    assert_eq!(
        json["attributes"]["nested"],
        serde_json::json!([true, null, {"x":0.5}])
    );
    assert_eq!(json["environment"], serde_json::Value::Null);
    assert!(matches!(wire::decode(&body, 2), Err(DecodeError::TooLarge)));
}

#[test]
fn rejects_invalid_data_and_trailing_messagepack() {
    assert!(wire::decode(b"not zstd", 1024).is_err());
    let packed = rmp_serde::to_vec_named(&wire::CoreBatch {
        dropped: 0,
        groups: vec![],
    })
    .unwrap();
    let mut trailing = packed.clone();
    trailing.push(0);
    assert!(wire::decode(&zstd::encode_all(&trailing[..], 3).unwrap(), 1024).is_err());
    assert!(wire::decode(&zstd::encode_all(&packed[..], 3).unwrap(), 1024).is_ok());
}

#[test]
fn configuration_requires_separate_tokens_and_valid_limits() {
    assert!(Config::from_lookup(|_| None).is_err());
    let lookup = |key: &str| match key {
        "LOGNARA_INGEST_TOKEN" => Some("ingest".into()),
        "LOGNARA_QUERY_TOKEN" => Some("query".into()),
        _ => None,
    };
    let config = Config::from_lookup(lookup).unwrap();
    assert_eq!(config.listen_addr.port(), 7402);
    assert_eq!(config.retention.as_secs(), 604800);
    assert!(
        Config::from_lookup(|key| if key == "LOGNARA_MAX_BODY_BYTES" {
            Some("0".into())
        } else {
            lookup(key)
        })
        .is_err()
    );
    assert!(
        Config::from_lookup(|key| if key == "LOGNARA_QUERY_TOKEN" {
            Some("ingest".into())
        } else {
            lookup(key)
        })
        .is_err()
    );
}
