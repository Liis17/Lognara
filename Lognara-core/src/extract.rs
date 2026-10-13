//! Точки из атрибута "Metrics" (SchemaVersion 2) при материализации. Прочие события пропускаются.
use serde_json::Value;

use crate::{
    model::{StoredEvent, parse_timestamp},
    series::{Origin, Point},
};

/// Точки события и признак ошибки. Ошибка не отменяет валидные записи отчёта:
/// true означает, что отчёт целиком или хотя бы одна запись отброшены.
pub fn native(row: &StoredEvent) -> (Vec<Point>, bool) {
    let Some(report) = row.event.attributes.get("Metrics") else {
        return (vec![], false);
    };
    let Some(report) = report.as_object() else {
        return (vec![], true);
    };
    if report.get("SchemaVersion").and_then(Value::as_u64) != Some(2) {
        return (vec![], true);
    }
    let timestamp = report
        .get("Timestamp")
        .and_then(Value::as_str)
        .and_then(|text| parse_timestamp(text).ok())
        .unwrap_or(row.event.timestamp);
    let mut points = vec![];
    let mut invalid = false;
    for (key, origin) in [("Counters", Origin::Counter), ("Gauges", Origin::Gauge)] {
        let entries = match report.get(key) {
            None => continue,
            Some(Value::Object(entries)) => entries,
            Some(_) => {
                invalid = true;
                continue;
            }
        };
        for (name, value) in entries {
            match value.as_f64() {
                Some(value) if value.is_finite() && (1..=128).contains(&name.len()) => {
                    points.push(Point {
                        timestamp,
                        source: row.source.clone(),
                        name: name.clone(),
                        origin,
                        rule_id: None,
                        labels: String::new(),
                        value,
                        event_id: None,
                    });
                }
                _ => invalid = true,
            }
        }
    }
    (points, invalid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{Event, LogId, LogLevel, Source};
    use serde_json::{Map, json};
    use std::collections::HashMap;
    use uuid::Uuid;

    fn row(metrics: Option<Value>) -> StoredEvent {
        let mut attributes = HashMap::new();
        if let Some(metrics) = metrics {
            attributes.insert("Metrics".to_string(), metrics);
        }
        StoredEvent {
            sequence: 1,
            core_received_at: 0,
            source: Source {
                environment: Some("prod".into()),
                server: "srv".into(),
                backend: "be".into(),
                service: "svc".into(),
                service_instance: None,
            },
            event: Event {
                id: LogId(Uuid::from_u128(1)),
                timestamp: 500,
                ingested_at: 0,
                action: None,
                level: LogLevel::Info,
                message: "m".into(),
                trace_id: None,
                span_id: None,
                parent_span_id: None,
                request_id: None,
                attributes,
            },
        }
    }

    #[test]
    fn valid_report_becomes_counter_points_with_report_timestamp() {
        let row = row(Some(json!({
            "SchemaVersion": 2,
            "ServiceName": "BarkFluff.Onliner",
            "Counters": {"offline_detection_runs": 10},
            "Gauges": {},
            "Timestamp": "2026-10-13T15:14:00.4480321Z"
        })));
        let (points, invalid) = native(&row);
        assert!(!invalid);
        assert_eq!(
            points,
            vec![Point {
                timestamp: 1_791_904_440_448_032_100,
                source: row.source.clone(),
                name: "offline_detection_runs".into(),
                origin: Origin::Counter,
                rule_id: None,
                labels: String::new(),
                value: 10.0,
                event_id: None,
            }]
        );
    }

    #[test]
    fn gauges_become_gauge_points() {
        let (points, invalid) = native(&row(Some(
            json!({"SchemaVersion": 2, "Gauges": {"queue_depth": 7.5}}),
        )));
        assert!(!invalid);
        assert_eq!(points.len(), 1);
        assert_eq!(points[0].origin, Origin::Gauge);
        assert_eq!(points[0].name, "queue_depth");
        assert_eq!(points[0].value, 7.5);
    }

    #[test]
    fn wrong_schema_version_rejects_whole_report() {
        assert_eq!(
            native(&row(Some(
                json!({"SchemaVersion": 3, "Counters": {"runs": 1}})
            ))),
            (vec![], true)
        );
        assert_eq!(
            native(&row(Some(json!({"Counters": {"runs": 1}})))),
            (vec![], true)
        );
        assert_eq!(native(&row(Some(json!("text")))), (vec![], true));
    }

    #[test]
    fn non_numeric_and_oversized_entries_are_skipped_but_valid_ones_kept() {
        let mut counters = Map::new();
        counters.insert("n".repeat(129), json!(1));
        counters.insert("ok".into(), json!(1));
        counters.insert("bad".into(), json!("x"));
        let (points, invalid) = native(&row(Some(
            json!({"SchemaVersion": 2, "Counters": counters}),
        )));
        assert!(invalid);
        assert_eq!(
            points.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            ["ok"]
        );
    }

    #[test]
    fn missing_or_unparsable_timestamp_falls_back_to_event_time() {
        let (points, _) = native(&row(Some(
            json!({"SchemaVersion": 2, "Counters": {"runs": 1}}),
        )));
        assert_eq!(points[0].timestamp, 500);
        let (points, _) = native(&row(Some(
            json!({"SchemaVersion": 2, "Counters": {"runs": 1}, "Timestamp": "yesterday"}),
        )));
        assert_eq!(points[0].timestamp, 500);
    }

    #[test]
    fn event_without_metrics_attribute_is_ignored() {
        assert_eq!(native(&row(None)), (vec![], false));
    }
}
