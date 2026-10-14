//! Fingerprint проблемы v1: область environment/service/action + exception.type
//! и первые фреймы exception.stacktrace без позиций; без исключения — pattern_id сообщения.
use crate::{pattern_identity::Identity, wire::LogLevel};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;

pub const VERSION: u8 = 1;
const MAX_TYPE: usize = 256;
const MAX_STACK: usize = 64 << 10;
const MAX_FRAME: usize = 512;
const MAX_FRAMES: usize = 5;

/// Атрибуты OpenTelemetry; нестроковые значения не участвуют в fingerprint.
#[derive(Default, Deserialize)]
pub struct Exception {
    #[serde(rename = "exception.type")]
    kind: Option<Value>,
    #[serde(rename = "exception.stacktrace")]
    stacktrace: Option<Value>,
}

impl Exception {
    /// Некорректный JSON атрибутов означает отсутствие исключения.
    pub fn parse(attributes: &str) -> Self {
        serde_json::from_str(attributes).unwrap_or_default()
    }

    pub fn from_map(attributes: &HashMap<String, Value>) -> Self {
        Self {
            kind: attributes.get("exception.type").cloned(),
            stacktrace: attributes.get("exception.stacktrace").cloned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint {
    pub id: String,
    pub exception_type: Option<String>,
    pub frames: Vec<String>,
}

pub fn is_error(level: LogLevel) -> bool {
    matches!(level, LogLevel::Error | LogLevel::Fatal)
}

/// None для уровней ниже error: проблемы образуют только error и fatal.
pub fn fingerprint(
    environment: Option<&str>,
    service: &str,
    action: Option<&str>,
    level: LogLevel,
    pattern: &Identity,
    exception: &Exception,
) -> Option<Fingerprint> {
    if !is_error(level) {
        return None;
    }
    let kind = exception
        .kind
        .as_ref()
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|kind| !kind.is_empty())
        .map(|kind| truncate(kind, MAX_TYPE).to_owned());
    let Some(kind) = kind else {
        let id = hash(&(
            VERSION,
            environment,
            service,
            action,
            "pattern",
            &pattern.id,
        ));
        return Some(Fingerprint {
            id,
            exception_type: None,
            frames: vec![],
        });
    };
    let frames = exception
        .stacktrace
        .as_ref()
        .and_then(Value::as_str)
        .map(frames)
        .unwrap_or_default();
    let id = hash(&(
        VERSION,
        environment,
        service,
        action,
        "exception",
        &kind,
        &frames,
    ));
    Some(Fingerprint {
        id,
        exception_type: Some(kind),
        frames,
    })
}

pub fn valid_id(id: &str) -> bool {
    id.len() == 67
        && id.starts_with("f1_")
        && id.as_bytes()[3..]
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
}

fn hash(value: &impl serde::Serialize) -> String {
    let bytes = serde_json::to_vec(value).expect("issue identity serializes");
    format!("f1_{}", hex::encode(Sha256::digest(bytes)))
}

fn truncate(text: &str, bytes: usize) -> &str {
    let mut end = text.len().min(bytes);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Строки `at …` (.NET, Java, JS, Rust) и `File "…"` (Python); Python печатает
/// внешний вызов первым, поэтому его порядок разворачивается.
fn frames(stacktrace: &str) -> Vec<String> {
    let text = truncate(stacktrace, MAX_STACK);
    let python = text
        .trim_start()
        .starts_with("Traceback (most recent call last)");
    let mut frames: Vec<String> = text
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let frame = line
                .strip_prefix("at ")
                .or_else(|| line.starts_with("File \"").then_some(line))?;
            Some(truncate(&strip_positions(frame), MAX_FRAME).to_owned())
        })
        .collect();
    if python {
        frames.reverse();
    }
    frames.truncate(MAX_FRAMES);
    frames
}

/// Убирает номера строк/колонок и адреса: они меняются при каждом деплое.
fn strip_positions(frame: &str) -> String {
    let bytes = frame.as_bytes();
    let mut out = String::with_capacity(frame.len());
    let mut i = 0;
    while i < bytes.len() {
        let boundary = !out
            .chars()
            .last()
            .is_some_and(|char| char.is_alphanumeric() || char == '_');
        if boundary
            && frame[i..].starts_with("0x")
            && bytes.get(i + 2).is_some_and(u8::is_ascii_hexdigit)
        {
            i += 2;
            while bytes.get(i).is_some_and(u8::is_ascii_hexdigit) {
                i += 1;
            }
            continue;
        }
        if bytes[i].is_ascii_digit() && (out.ends_with(':') || out.ends_with("line ")) {
            while bytes.get(i).is_some_and(u8::is_ascii_digit) {
                i += 1;
            }
            continue;
        }
        let char = frame[i..].chars().next().expect("index on char boundary");
        out.push(char);
        i += char.len_utf8();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pattern_identity::identify;
    use serde_json::json;

    fn print(
        action: Option<&str>,
        level: LogLevel,
        message: &str,
        attributes: Value,
    ) -> Option<Fingerprint> {
        let pattern = identify(Some("prod"), "billing", level.as_str(), message);
        fingerprint(
            Some("prod"),
            "billing",
            action,
            level,
            &pattern,
            &Exception::parse(&attributes.to_string()),
        )
    }

    const DOTNET: &str = "System.NullReferenceException: Object reference not set\n   at InvoiceMapper.Map(Invoice src) in /src/InvoiceMapper.cs:line 84\n   at InvoiceService.Sync(Guid tenantId) in /src/InvoiceService.cs:line 41\n--- End of stack trace from previous location ---\n   at Program.<Main>$(String[] args) in /src/Program.cs:line 7";

    #[test]
    fn exception_type_and_frames_without_positions() {
        let attributes = json!({"exception.type": "System.NullReferenceException", "exception.stacktrace": DOTNET});
        let first = print(
            Some("invoice.sync"),
            LogLevel::Error,
            "Mapping failed id=1",
            attributes,
        )
        .unwrap();
        assert!(valid_id(&first.id));
        assert_eq!(
            first.frames,
            [
                "InvoiceMapper.Map(Invoice src) in /src/InvoiceMapper.cs:line ",
                "InvoiceService.Sync(Guid tenantId) in /src/InvoiceService.cs:line ",
                "Program.<Main>$(String[] args) in /src/Program.cs:line ",
            ]
        );
        // Другие номера строк, сообщение и fatal дают ту же проблему.
        let moved = print(
            Some("invoice.sync"),
            LogLevel::Fatal,
            "Another text",
            json!({"exception.type": " System.NullReferenceException ", "exception.stacktrace": DOTNET.replace("84", "90").replace("41", "45")}),
        )
        .unwrap();
        assert_eq!(first.id, moved.id);
        for other in [
            print(
                Some("invoice.create"),
                LogLevel::Error,
                "Mapping failed id=1",
                json!({"exception.type": "System.NullReferenceException", "exception.stacktrace": DOTNET}),
            ),
            print(
                Some("invoice.sync"),
                LogLevel::Error,
                "Mapping failed id=1",
                json!({"exception.type": "System.InvalidOperationException", "exception.stacktrace": DOTNET}),
            ),
            print(
                Some("invoice.sync"),
                LogLevel::Error,
                "Mapping failed id=1",
                json!({"exception.type": "System.NullReferenceException", "exception.stacktrace": DOTNET.replace("InvoiceMapper.Map", "InvoiceMapper.Load")}),
            ),
            print(
                Some("invoice.sync"),
                LogLevel::Error,
                "Mapping failed id=1",
                json!({"exception.type": "System.NullReferenceException"}),
            ),
        ] {
            assert_ne!(first.id, other.unwrap().id);
        }
    }

    #[test]
    fn fallback_levels_and_other_runtimes() {
        assert!(print(None, LogLevel::Warn, "slow", json!({})).is_none());
        let a = print(Some("pay"), LogLevel::Error, "timeout id=1", json!({})).unwrap();
        let b = print(
            Some("pay"),
            LogLevel::Error,
            "timeout id=2",
            json!({"exception.type": 7, "exception.stacktrace": "at x"}),
        )
        .unwrap();
        assert_eq!(a.id, b.id);
        assert!(a.exception_type.is_none() && a.frames.is_empty());
        assert_ne!(
            a.id,
            print(None, LogLevel::Error, "timeout id=1", json!({}))
                .unwrap()
                .id
        );
        assert_ne!(
            a.id,
            print(Some("pay"), LogLevel::Fatal, "timeout id=1", json!({}))
                .unwrap()
                .id
        );
        assert_eq!(
            frames(
                "java.lang.IllegalStateException: x\n\tat com.acme.Pay.charge(Pay.java:42)\n\tat com.acme.Api.run(Api.java:7)"
            ),
            [
                "com.acme.Pay.charge(Pay.java:)",
                "com.acme.Api.run(Api.java:)"
            ]
        );
        assert_eq!(
            frames("Error: boom\n    at charge (/app/pay.js:10:5)\n    at 0x7f3a (native)"),
            ["charge (/app/pay.js::)", " (native)"]
        );
        assert_eq!(
            frames(
                "Traceback (most recent call last):\n  File \"/app/main.py\", line 12, in run\n    pay()\n  File \"/app/pay.py\", line 3, in pay\nValueError: bad"
            ),
            [
                "File \"/app/pay.py\", line , in pay",
                "File \"/app/main.py\", line , in run"
            ]
        );
        let many = (0..9)
            .map(|n| format!("at f{n}()"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(frames(&many).len(), 5);
        assert_eq!(Exception::parse("not json").kind, None);
    }
}
