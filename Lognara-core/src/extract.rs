//! Точки при материализации: из атрибута "Metrics" (SchemaVersion 2) и из правил по тексту сообщения.
use regex::Captures;
use serde_json::Value;

use crate::{
    definitions::{CompiledRule, RuleSet},
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

/// Точки всех правил сервиса строки и признак ошибки разбора значения.
pub fn rules(row: &StoredEvent, rules: &RuleSet) -> (Vec<Point>, bool) {
    let mut points = vec![];
    let mut invalid = false;
    for rule in rules.for_service(&row.source.service) {
        let (point, failed) = rule_point(rule, row);
        points.extend(point);
        invalid |= failed;
    }
    (points, invalid)
}

/// Точка одного правила. Значение - группа value_group или 1 без неё; нечисловое значение - ошибка.
pub fn rule_point(rule: &CompiledRule, row: &StoredEvent) -> (Option<Point>, bool) {
    if rule
        .server
        .as_ref()
        .is_some_and(|server| server != &row.source.server)
    {
        return (None, false);
    }
    if rule
        .environment
        .as_ref()
        .is_some_and(|environment| row.source.environment.as_ref() != Some(environment))
    {
        return (None, false);
    }
    let message = row.event.message.as_str();
    if !message.contains(rule.contains.as_str()) {
        return (None, false);
    }
    let Some(captures) = rule.regex.captures(message) else {
        return (None, false);
    };
    let value = match &rule.value_group {
        None => 1.0,
        Some(group) => {
            let value = captures
                .name(group)
                .and_then(|text| text.as_str().trim().parse::<f64>().ok())
                .filter(|value| value.is_finite());
            match value {
                Some(value) => value,
                None => return (None, true),
            }
        }
    };
    let point = Point {
        timestamp: row.event.timestamp,
        source: row.source.clone(),
        name: rule.name.clone(),
        origin: Origin::Rule,
        rule_id: Some(rule.id.clone()),
        labels: labels(rule, &captures),
        value,
        event_id: Some(row.event.id.0.into_bytes()),
    };
    (Some(point), false)
}

/// Метки "k=v,k2=v2" по возрастанию ключа. Не участвовавшие группы опускаются.
fn labels(rule: &CompiledRule, captures: &Captures) -> String {
    let mut pairs: Vec<(&str, &str)> = rule
        .label_groups
        .iter()
        .filter_map(|group| {
            captures
                .name(group)
                .map(|text| (group.as_str(), truncate(text.as_str())))
        })
        .collect();
    pairs.sort_unstable();
    pairs
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// Не длиннее 128 байт и без разрыва UTF-8.
fn truncate(text: &str) -> &str {
    let mut end = text.len().min(128);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        definitions::{Aggregation, Definition, Kind, Rule},
        wire::{Event, LogId, LogLevel, Source},
    };
    use serde_json::{Map, json};
    use std::collections::HashMap;
    use uuid::Uuid;

    fn message_row(service: &str, environment: Option<&str>, message: &str) -> StoredEvent {
        StoredEvent {
            sequence: 1,
            core_received_at: 0,
            source: Source {
                environment: environment.map(Into::into),
                server: "srv".into(),
                backend: "be".into(),
                service: service.into(),
                service_instance: None,
            },
            event: Event {
                id: LogId(Uuid::from_u128(9)),
                timestamp: 700,
                ingested_at: 0,
                action: None,
                level: LogLevel::Info,
                message: message.into(),
                trace_id: None,
                span_id: None,
                parent_span_id: None,
                request_id: None,
                attributes: HashMap::new(),
            },
        }
    }

    fn rule_set(rule: Rule) -> RuleSet {
        RuleSet::load(&[Definition {
            legacy_rule_id: None,
            id: Uuid::from_u128(7),
            server: "srv".into(),
            service: "svc".into(),
            name: "request_ms".into(),
            title: String::new(),
            description: String::new(),
            unit: String::new(),
            kind: Kind::Gauge,
            aggregation: Aggregation::Avg,
            enabled: true,
            rule: Some(rule),
            updated_at: String::new(),
        }])
        .unwrap()
    }

    fn timing_rule() -> Rule {
        Rule {
            environment: None,
            contains: "completed in".into(),
            pattern: r"completed in (?P<value>\d+) ms, status (?P<status>\d+)".into(),
            value_group: Some("value".into()),
            label_groups: vec!["status".into()],
        }
    }

    #[test]
    fn rule_extracts_value_and_labels_with_origin_and_event_id() {
        let set = rule_set(timing_rule());
        let row = message_row(
            "svc",
            Some("prod"),
            "Request /api completed in 123 ms, status 200",
        );
        let (points, invalid) = rules(&row, &set);
        assert!(!invalid);
        assert_eq!(
            points,
            vec![Point {
                timestamp: 700,
                source: row.source.clone(),
                name: "request_ms".into(),
                origin: Origin::Rule,
                rule_id: Some(Uuid::from_u128(7).to_string()),
                labels: "status=200".into(),
                value: 123.0,
                event_id: Some(Uuid::from_u128(9).into_bytes()),
            }]
        );
    }

    #[test]
    fn rule_without_value_group_counts_one_per_match() {
        let set = rule_set(Rule {
            value_group: None,
            label_groups: vec![],
            ..timing_rule()
        });
        let (points, invalid) = rules(
            &message_row("svc", None, "Request completed in 5 ms, status 200"),
            &set,
        );
        assert!(!invalid);
        assert_eq!(points.len(), 1);
        assert_eq!(points[0].value, 1.0);
        assert_eq!(points[0].labels, "");
    }

    #[test]
    fn environment_filter_skips_other_environments() {
        let set = rule_set(Rule {
            environment: Some("Production".into()),
            ..timing_rule()
        });
        let message = "completed in 1 ms, status 200";
        assert!(
            rules(&message_row("svc", Some("prod"), message), &set)
                .0
                .is_empty()
        );
        assert!(rules(&message_row("svc", None, message), &set).0.is_empty());
        assert_eq!(
            rules(&message_row("svc", Some("Production"), message), &set)
                .0
                .len(),
            1
        );
    }

    #[test]
    fn contains_prefilter_skips_before_regex() {
        let set = rule_set(Rule {
            contains: "completed".into(),
            pattern: r"(?P<value>\d+) ms".into(),
            value_group: Some("value".into()),
            label_groups: vec![],
            ..timing_rule()
        });
        // Регулярное выражение совпало бы с "5 ms", поэтому пропуск идёт из-за contains.
        assert_eq!(
            rules(&message_row("svc", None, "took 5 ms"), &set),
            (vec![], false)
        );
    }

    #[test]
    fn non_numeric_value_is_invalid_and_produces_no_point() {
        let set = rule_set(Rule {
            pattern: r"status (?P<value>\w+)".into(),
            contains: "status".into(),
            label_groups: vec![],
            ..timing_rule()
        });
        assert_eq!(
            rules(&message_row("svc", None, "status ok"), &set),
            (vec![], true)
        );
    }

    #[test]
    fn other_services_are_ignored() {
        let set = rule_set(timing_rule());
        assert_eq!(
            rules(
                &message_row("other", None, "completed in 1 ms, status 200"),
                &set
            ),
            (vec![], false)
        );
    }

    #[test]
    fn labels_are_sorted_by_key_and_values_truncated_on_char_boundary() {
        let set = rule_set(Rule {
            pattern: r"(?P<b>\S+) (?P<a>\S+) (?P<missing>x)?".into(),
            contains: " ".into(),
            value_group: None,
            label_groups: vec!["b".into(), "a".into(), "missing".into()],
            ..timing_rule()
        });
        let long = "я".repeat(100);
        let (points, _) = rules(&message_row("svc", None, &format!("{long} z ")), &set);
        // 100 русских букв по 2 байта: обрезка до 128 байт не разрывает символ.
        let expected = format!("a=z,b={}", "я".repeat(64));
        assert_eq!(points[0].labels, expected);
    }

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
