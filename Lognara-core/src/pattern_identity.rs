//! Консервативная нормализация v1: заменяются только UUID, IP, длинные hex-ID,
//! явно обозначенные ID и длительности; в access-log и ServiceMetrics также
//! время и числовые измерения. Слова, статусы и коды ошибок сохраняются.
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    net::{Ipv4Addr, Ipv6Addr},
    sync::LazyLock,
};

pub const VERSION: u8 = 1;
/// Отдельная ревизия производных индексов; не меняет ID незатронутых сообщений.
pub(crate) const INDEX_PAYLOAD: &str = "lognara-pattern-normalization-2";
const MAX_MESSAGE: usize = 16 << 10;
const MAX_VARIABLES: usize = 64;
const PREVIEW_BYTES: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VariableType {
    Ip,
    Uuid,
    Id,
    Duration,
    Hex,
    Timestamp,
    Number,
}

impl VariableType {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ip => "ip",
            Self::Uuid => "uuid",
            Self::Id => "id",
            Self::Duration => "duration",
            Self::Hex => "hex",
            Self::Timestamp => "timestamp",
            Self::Number => "number",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Part {
    Literal {
        text: String,
    },
    Variable {
        name: String,
        variable_type: VariableType,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variable {
    pub name: String,
    pub variable_type: VariableType,
    pub value: String,
}

#[derive(Debug, Clone)]
pub struct Identity {
    pub id: String,
    /// Preview до 4096 байт; ID вычисляется по полным частям.
    pub template: String,
    pub parts: Vec<Part>,
    pub variables: Vec<Variable>,
    pub exact: bool,
}

/// Один алгоритм для индекса, агрегатов и фонового каталога.
pub fn identify(environment: Option<&str>, service: &str, level: &str, message: &str) -> Identity {
    let variables = if message.len() > MAX_MESSAGE {
        None
    } else {
        normalize(message)
    };
    let Some((parts, variables)) = variables else {
        let id = hash(&(VERSION, environment, service, level, "exact", message));
        let text = truncate(message, PREVIEW_BYTES).to_owned();
        return Identity {
            id,
            template: text.clone(),
            parts: vec![Part::Literal { text }],
            variables: vec![],
            exact: true,
        };
    };
    let id = hash(&(VERSION, environment, service, level, "parts", &parts));
    let parts = preview(parts);
    Identity {
        id,
        template: render(&parts),
        parts,
        variables,
        exact: false,
    }
}

pub fn valid_id(id: &str) -> bool {
    id.len() == 67
        && id.starts_with("p1_")
        && id.as_bytes()[3..]
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
}

fn hash(value: &impl Serialize) -> String {
    let bytes = serde_json::to_vec(value).expect("pattern identity serializes");
    format!("p1_{}", hex::encode(Sha256::digest(bytes)))
}

pub fn render(parts: &[Part]) -> String {
    parts
        .iter()
        .map(|part| match part {
            Part::Literal { text } => text.clone(),
            Part::Variable { name, .. } => format!("<{name}>"),
        })
        .collect()
}

fn preview(parts: Vec<Part>) -> Vec<Part> {
    let mut remaining = PREVIEW_BYTES;
    let mut result = vec![];
    for part in parts {
        let size = match &part {
            Part::Literal { text } => text.len(),
            Part::Variable { name, .. } => name.len() + 2,
        };
        if size <= remaining {
            remaining -= size;
            result.push(part);
            continue;
        }
        if let Part::Literal { text } = part {
            let text = truncate(&text, remaining);
            if !text.is_empty() {
                result.push(Part::Literal { text: text.into() });
            }
        }
        break;
    }
    result
}

fn truncate(text: &str, bytes: usize) -> &str {
    let mut end = bytes.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// None означает слишком много переменных: сообщение идентифицируется точным текстом.
fn normalize(message: &str) -> Option<(Vec<Part>, Vec<Variable>)> {
    let bytes = message.as_bytes();
    let mut parts = vec![];
    let mut variables: Vec<Variable> = vec![];
    let mut literal = 0;
    let mut i = 0;
    let spans = format_variables(message);
    let mut spans = spans.into_iter().peekable();
    while i < bytes.len() {
        // Совпадения начинаются только на границе слова и на ASCII-символе.
        let found = if spans.peek().is_some_and(|span| span.0 == i) {
            spans.next()
        } else if (i == 0 || !word(bytes[i - 1])) && bytes[i].is_ascii() {
            matcher(message, i).filter(|found| spans.peek().is_none_or(|span| found.1 <= span.0))
        } else {
            None
        };
        let Some((start, end, variable_type)) = found else {
            i += 1;
            continue;
        };
        if variables.len() == MAX_VARIABLES {
            return None;
        }
        if literal < start {
            parts.push(Part::Literal {
                text: message[literal..start].into(),
            });
        }
        let number = variables
            .iter()
            .filter(|variable| variable.variable_type == variable_type)
            .count()
            + 1;
        let name = format!("{}_{number}", variable_type.as_str());
        parts.push(Part::Variable {
            name: name.clone(),
            variable_type,
        });
        variables.push(Variable {
            name,
            variable_type,
            value: message[start..end].into(),
        });
        literal = end;
        i = end;
    }
    if literal < bytes.len() {
        parts.push(Part::Literal {
            text: message[literal..].into(),
        });
    }
    Some((parts, variables))
}

/// Только подтверждённые структурные форматы: произвольные числа остаются литералами.
fn format_variables(message: &str) -> Vec<(usize, usize, VariableType)> {
    static ACCESS: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
        r#"^\S+ \S+ \S+ \[(?<timestamp>[0-9]{2}/[A-Za-z]{3}/[0-9]{4}:[0-9]{2}:[0-9]{2}:[0-9]{2} [+-][0-9]{4})\] "(?:[^"\\]|\\.)*" [0-9]{3} (?<size>[0-9]+|-)(?: |$)"#,
    ).expect("fixed access-log expression")
    });
    static METRICS: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
        r#"^ServiceMetrics \{ ServiceName: "(?:[^"\\]|\\.)*", Metrics: \[(?<metrics>.*)\], Timestamp: (?<timestamp>[0-9]{1,2}/[0-9]{1,2}/[0-9]{4} [0-9]{2}:[0-9]{2}:[0-9]{2}) \}$"#,
    ).expect("fixed ServiceMetrics expression")
    });
    static METRIC: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"\("(?:[^"\\]|\\.)+": (?<number>-?[0-9]+(?:\.[0-9]+)?(?:[eE][+-]?[0-9]+)?)\)"#)
            .expect("fixed metric expression")
    });
    let span = |value: regex::Match<'_>, kind| (value.start(), value.end(), kind);
    if message.starts_with("ServiceMetrics {") {
        let Some(captures) = METRICS.captures(message) else {
            return vec![];
        };
        let metrics = captures.name("metrics").unwrap();
        let mut result = vec![];
        let mut end = 0;
        for pair in METRIC.captures_iter(metrics.as_str()) {
            let whole = pair.get(0).unwrap();
            let separator = if end == 0 { "" } else { ", " };
            if &metrics.as_str()[end..whole.start()] != separator {
                return vec![];
            }
            let number = pair.name("number").unwrap();
            result.push((
                metrics.start() + number.start(),
                metrics.start() + number.end(),
                VariableType::Number,
            ));
            end = whole.end();
        }
        if end != metrics.as_str().len() {
            return vec![];
        }
        result.push(span(
            captures.name("timestamp").unwrap(),
            VariableType::Timestamp,
        ));
        return result;
    }
    // Не запускаем regexp access-log для обычных сообщений без квадратных скобок.
    if message.contains("] \"")
        && let Some(captures) = ACCESS.captures(message)
    {
        let mut result = vec![span(
            captures.name("timestamp").unwrap(),
            VariableType::Timestamp,
        )];
        let size = captures.name("size").unwrap();
        if size.as_str() != "-" {
            result.push(span(size, VariableType::Number));
        }
        return result;
    }
    vec![]
}

/// Возвращает границы значения переменной; ключ и разделитель остаются литералом.
fn matcher(message: &str, i: usize) -> Option<(usize, usize, VariableType)> {
    if let Some(found) = labeled(message, i) {
        return Some(found);
    }
    let (end, variable_type) = value(message, i)?;
    Some((i, end, variable_type))
}

/// Самостоятельное значение, не требующее ключа.
fn value(message: &str, i: usize) -> Option<(usize, VariableType)> {
    let bytes = message.as_bytes();
    let boundary = |end: usize| end == bytes.len() || !word(bytes[end]);
    if let Some(end) = uuid(bytes, i).filter(|&end| boundary(end)) {
        return Some((end, VariableType::Uuid));
    }
    if let Some(end) = ipv4(message, i) {
        return Some((end, VariableType::Ip));
    }
    if let Some(end) = ipv6(message, i) {
        return Some((end, VariableType::Ip));
    }
    if let Some(end) = duration(bytes, i).filter(|&end| boundary(end)) {
        return Some((end, VariableType::Duration));
    }
    let end = run(bytes, i, |byte| byte.is_ascii_hexdigit());
    if end - i >= 16 && boundary(end) && bytes[i..end].iter().any(u8::is_ascii_digit) {
        return Some((end, VariableType::Hex));
    }
    None
}

fn run(bytes: &[u8], start: usize, accept: impl Fn(u8) -> bool) -> usize {
    let mut end = start;
    while end < bytes.len() && accept(bytes[end]) {
        end += 1;
    }
    end
}

fn uuid(bytes: &[u8], i: usize) -> Option<usize> {
    let mut end = i;
    for (index, length) in [8, 4, 4, 4, 12].into_iter().enumerate() {
        if index > 0 {
            (bytes.get(end) == Some(&b'-')).then_some(())?;
            end += 1;
        }
        (run(bytes, end, |byte| byte.is_ascii_hexdigit()) - end >= length).then_some(())?;
        end += length;
    }
    Some(end)
}

fn ipv4(message: &str, i: usize) -> Option<usize> {
    let bytes = message.as_bytes();
    let end = run(bytes, i, |byte| byte.is_ascii_digit() || byte == b'.');
    // Версии вида 1.2.3.4.5 и завершающая точка предложения не являются адресом.
    let end = if end > i && bytes[end - 1] == b'.' {
        end - 1
    } else {
        end
    };
    let text = &message[i..end];
    (text.split('.').count() == 4
        && (i == 0 || bytes[i - 1] != b'.')
        && (end == bytes.len() || !word(bytes[end]))
        && text.parse::<Ipv4Addr>().is_ok())
    .then_some(end)
}

fn ipv6(message: &str, i: usize) -> Option<usize> {
    let bytes = message.as_bytes();
    let mut end = run(bytes, i, |byte| byte.is_ascii_hexdigit() || byte == b':');
    if end > i && bytes[end - 1] == b':' && !message[i..end].ends_with("::") {
        end -= 1;
    }
    let text = &message[i..end];
    (text.matches(':').count() >= 2
        && text.bytes().any(|byte| byte.is_ascii_hexdigit())
        && (end == bytes.len() || !word(bytes[end]))
        && text.parse::<Ipv6Addr>().is_ok())
    .then_some(end)
}

/// Число с единицей времени, включая составные Go-длительности `1m30.5s`.
fn duration(bytes: &[u8], i: usize) -> Option<usize> {
    let mut end = i;
    let mut pairs = 0;
    loop {
        let digits = run(bytes, end, |byte| byte.is_ascii_digit());
        if digits == end {
            break;
        }
        let mut number = digits;
        if bytes.get(number) == Some(&b'.') {
            let fraction = run(bytes, number + 1, |byte| byte.is_ascii_digit());
            if fraction > number + 1 {
                number = fraction;
            }
        }
        let rest = &bytes[number..];
        let unit = ["µs", "ns", "us", "ms", "min", "sec", "h", "m", "s"]
            .into_iter()
            .find(|unit| {
                rest.starts_with(unit.as_bytes())
                    && rest
                        .get(unit.len())
                        .is_none_or(|&next| !next.is_ascii_alphabetic())
            })?;
        end = number + unit.len();
        pairs += 1;
    }
    (pairs > 0).then_some(end)
}

fn id_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "id" | "pid" | "tid" | "uid" | "uuid" | "guid"
    ) || (lower.len() > 3
        && lower.ends_with("id")
        && matches!(key.as_bytes()[key.len() - 3], b'_' | b'-' | b'.'))
        || (key.len() > 2
            && (key.ends_with("Id") || key.ends_with("ID"))
            && key.as_bytes()[key.len() - 3].is_ascii_lowercase())
}

fn duration_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    [
        "duration", "elapsed", "latency", "took", "_ms", "_seconds", "_sec",
    ]
    .iter()
    .any(|suffix| lower.ends_with(suffix))
        || key.ends_with("Ms")
}

/// `key=value`, `key: value`, `key="value"` для ключей ID и длительности.
fn labeled(message: &str, i: usize) -> Option<(usize, usize, VariableType)> {
    let bytes = message.as_bytes();
    if !bytes[i].is_ascii_alphabetic() {
        return None;
    }
    let key_end = run(bytes, i, |byte| word(byte) || byte == b'-' || byte == b'.');
    let key = message[i..key_end].trim_end_matches(['.', '-']);
    let (is_id, is_duration) = (id_key(key), duration_key(key));
    if !is_id && !is_duration {
        return None;
    }
    let mut start = run(bytes, i + key.len(), |byte| byte == b' ');
    if !matches!(bytes.get(start), Some(b'=' | b':')) {
        return None;
    }
    start = run(bytes, start + 1, |byte| byte == b' ');
    if matches!(bytes.get(start), Some(b'"' | b'\'')) {
        start += 1;
    }
    if is_duration {
        let end = duration(bytes, start).or_else(|| {
            let end = run(bytes, start, |byte| byte.is_ascii_digit() || byte == b'.');
            (end > start && bytes[start].is_ascii_digit()).then_some(end)
        })?;
        let end = if bytes[end - 1] == b'.' { end - 1 } else { end };
        return (end == bytes.len() || !word(bytes[end])).then_some((
            start,
            end,
            VariableType::Duration,
        ));
    }
    if start < bytes.len()
        && let Some((end, variable_type)) = value(message, start)
    {
        return Some((start, end, variable_type));
    }
    let end = run(bytes, start, |byte| {
        word(byte) || byte == b'-' || byte == b'.'
    });
    let end = message[start..end].trim_end_matches(['.', '-']).len() + start;
    (end > start).then_some((start, end, VariableType::Id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn template(message: &str) -> String {
        identify(Some("prod"), "api", "error", message).template
    }

    #[test]
    fn conservative_rules_keep_words_and_codes() {
        assert_eq!(
            template("GET /orders returned 500 E1234 after 250ms from 10.0.0.12:8080"),
            "GET /orders returned 500 E1234 after <duration_1> from <ip_1>:8080"
        );
        assert_eq!(
            template("user_id=42 order 7 failed: ECONNREFUSED"),
            "user_id=<id_1> order 7 failed: ECONNREFUSED"
        );
        assert_eq!(
            template(
                "request 3f2a1b4c-0d9e-4f8a-9b7c-6d5e4f3a2b1c trace=4bf92f3577b34da6a3ce929d0e0e4736"
            ),
            "request <uuid_1> trace=<hex_1>"
        );
        assert_eq!(
            template("userId: \"abc-17\" took=1m30.5s"),
            "userId: \"<id_1>\" took=<duration_1>"
        );
        assert_eq!(
            template("valid=true version 1.2.3.4.5 at 12:30:45"),
            "valid=true version 1.2.3.4.5 at 12:30:45"
        );
        assert_eq!(
            template("peer ::1 and fe80::1ff:fe23:4567:890a"),
            "peer <ip_1> and <ip_2>"
        );
        assert_eq!(
            template("deadline 5 min, 2xx ok, 5s."),
            "deadline 5 min, 2xx ok, <duration_1>."
        );
    }

    #[test]
    fn identity_is_scoped_and_distinguishes_literals() {
        let a = identify(Some("prod"), "api", "error", "id=1 failed");
        let b = identify(Some("prod"), "api", "error", "id=2 failed");
        assert_eq!(a.id, b.id);
        assert_eq!(a.variables[0].value, "1");
        assert!(valid_id(&a.id));
        for other in [
            identify(None, "api", "error", "id=1 failed"),
            identify(Some(""), "api", "error", "id=1 failed"),
            identify(Some("prod"), "web", "error", "id=1 failed"),
            identify(Some("prod"), "api", "warn", "id=1 failed"),
            identify(Some("prod"), "api", "error", "id=<id_1> failed"),
            identify(Some("prod"), "api", "error", "id=1 Failed"),
        ] {
            assert_ne!(a.id, other.id);
        }
        assert_eq!(
            identify(Some("prod"), "api", "error", "id=<id_1> failed").template,
            a.template
        );
        // Стабильность алгоритма v1 между версиями сборки.
        assert_eq!(
            identify(None, "svc", "info", "hello").id,
            hash(&(
                1u8,
                None::<&str>,
                "svc",
                "info",
                "parts",
                vec![Part::Literal {
                    text: "hello".into()
                }]
            ))
        );
    }

    #[test]
    fn access_log_timestamps_and_sizes_do_not_create_patterns() {
        let first = r#"10.0.0.1 - - [19/Oct/2026:22:28:55 +0000] "POST /v1/batches HTTP/1.1" 204 0 "-" "-" "10.0.0.2""#;
        let second = first
            .replace("19/Oct/2026:22:28:55 +0000", "20/Oct/2026:00:13:32 +0300")
            .replace("204 0", "204 179");
        let a = identify(None, "nginx", "unknown", first);
        let b = identify(None, "nginx", "unknown", &second);
        assert_eq!(a.id, b.id);
        assert_ne!(a.variables, b.variables);
        for message in [
            first.replace("204 0", "500 0"),
            first.replace("POST", "GET"),
            first.replace("/v1/batches", "/v1/agents"),
        ] {
            assert_ne!(a.id, identify(None, "nginx", "unknown", &message).id);
        }
    }

    #[test]
    fn service_metrics_values_and_timestamp_do_not_create_patterns() {
        let first = r#"ServiceMetrics { ServiceName: "BarkCloud.Identity", Metrics: [("grpc_request_duration_ms_total": 4), ("grpc_requests_total": 4), ("service_started_unix": 1792529771)], Timestamp: 10/23/2026 12:14:47 }"#;
        let second = first
            .replace(": 4)", ": 5)")
            .replace("1792529771", "1792529981")
            .replace("10/23/2026 12:14:47", "10/24/2026 13:25:27");
        let a = identify(None, "cloud-identity", "info", first);
        let b = identify(None, "cloud-identity", "info", &second);
        assert_eq!(a.id, b.id);
        assert_ne!(a.variables, b.variables);
        assert_ne!(
            a.id,
            identify(
                None,
                "cloud-identity",
                "info",
                &first.replace("grpc_requests_total", "grpc_errors_total")
            )
            .id
        );
        assert_ne!(
            a.id,
            identify(
                None,
                "cloud-identity",
                "info",
                &first.replace("BarkCloud.Identity", "BarkCloud.Users")
            )
            .id
        );
    }

    #[test]
    fn structured_numbers_require_complete_formats_and_keep_limits() {
        let metric = r#"ServiceMetrics { ServiceName: "api", Metrics: [("latency_ms": -1.25e+3)], Timestamp: 10/23/2026 12:14:47 }"#;
        let result = identify(None, "api", "info", metric);
        assert_eq!(result.variables[0].variable_type, VariableType::Number);
        assert_eq!(result.variables[0].value, "-1.25e+3");
        assert_eq!(result.variables[1].variable_type, VariableType::Timestamp);
        for invalid in [
            metric.replace("-1.25e+3", "123oops"),
            metric.replace("Timestamp:", "Date:"),
        ] {
            assert!(
                !identify(None, "api", "info", &invalid)
                    .variables
                    .iter()
                    .any(|v| matches!(
                        v.variable_type,
                        VariableType::Number | VariableType::Timestamp
                    ))
            );
        }
        let metrics = (0..64)
            .map(|i| format!("(\"counter_{i}\": {i})"))
            .collect::<Vec<_>>()
            .join(", ");
        let long = format!(
            "ServiceMetrics {{ ServiceName: \"api\", Metrics: [{metrics}], Timestamp: 10/23/2026 12:14:47 }}"
        );
        assert!(identify(None, "api", "info", &long).exact);
    }

    #[test]
    fn unicode_multiline_and_limits() {
        let line = identify(None, "api", "error", "Ошибка: id=7\nстрока 2 ⏱ 3ms");
        assert_eq!(line.template, "Ошибка: id=<id_1>\nстрока 2 ⏱ <duration_1>");
        let many = (0..65)
            .map(|n| format!("id={n}"))
            .collect::<Vec<_>>()
            .join(" ");
        let exact = identify(None, "api", "error", &many);
        assert!(exact.exact && exact.variables.is_empty());
        assert_ne!(
            exact.id,
            identify(None, "api", "error", &format!("{many} ")).id
        );
        let long = "я".repeat(9000);
        let first = identify(None, "api", "error", &format!("{long}a"));
        let second = identify(None, "api", "error", &format!("{long}b"));
        assert!(first.exact && first.template.len() <= 4096);
        assert_eq!(first.template, second.template);
        assert_ne!(first.id, second.id);
        let wide = format!("{} id=1", "x".repeat(5000));
        let preview = identify(None, "api", "error", &wide);
        assert!(!preview.exact && preview.template.len() <= 4096 && preview.variables.len() == 1);
        assert_ne!(
            preview.id,
            identify(None, "api", "error", &format!("{} id=1", "x".repeat(5001))).id
        );
    }
}
