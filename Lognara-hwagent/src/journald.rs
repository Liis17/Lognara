//! Системный журнал через `journalctl --follow --output=json`.
//!
//! Cursor последней записи сохраняется в state только после durable-записи в spool:
//! падение между шагами даёт повтор, а не пропуск. Пока очередь полна, чтение pipe
//! приостанавливается и journalctl ждёт. Первый запуск без cursor читает новые записи
//! и один раз догружает историю auth/authpriv (вход, sudo) до момента запуска.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use lognara_agent::wire::{Payload, Record, unix_nanos};
use lognara_agent::{Collector, SpoolError};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

const BATCH: usize = 200;
const FLUSH: Duration = Duration::from_secs(1);
const RESTART: Duration = Duration::from_secs(5);
/// Строка JSON журнала длиннее этого отбрасывается с отметкой.
const MAX_LINE: usize = 512 << 10;
/// Поля журнала, переносимые в attributes под префиксом `journal.`.
const FIELDS: &[(&str, &str)] = &[
    ("_SYSTEMD_UNIT", "unit"),
    ("SYSLOG_IDENTIFIER", "identifier"),
    ("SYSLOG_FACILITY", "facility"),
    ("_PID", "pid"),
    ("_UID", "uid"),
    ("_EXE", "exe"),
    ("_COMM", "comm"),
    ("_HOSTNAME", "hostname"),
    ("_TRANSPORT", "transport"),
    ("CONTAINER_NAME", "container"),
    ("_BOOT_ID", "boot_id"),
];

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct JournalSettings {
    pub enabled: bool,
    pub journalctl: String,
    /// Аргументы-совпадения journalctl (`_SYSTEMD_UNIT=ssh.service`, `+`); пусто - весь журнал.
    pub matches: Vec<String>,
    /// Догрузка истории auth/authpriv при первом запуске.
    pub backfill_auth: bool,
}

impl Default for JournalSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            journalctl: "/usr/bin/journalctl".into(),
            matches: Vec::new(),
            backfill_auth: true,
        }
    }
}

impl JournalSettings {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !Path::new(&self.journalctl).is_absolute() {
            return Err("journald.journalctl must be an absolute path");
        }
        if self.matches.len() > 64
            || self.matches.iter().any(|value| {
                value.is_empty()
                    || value.len() > 1024
                    || value.starts_with('-')
                    || value.contains('\0')
            })
        {
            return Err("journald.matches: up to 64 journalctl matches, no options");
        }
        Ok(())
    }
}

/// Запись журнала → JSON-запись агента; relay разберёт timestamp, level и message.
/// Возвращает запись и cursor.
pub fn convert(line: &str) -> Option<(Record, Option<String>)> {
    let entry: Map<String, Value> = serde_json::from_str(line).ok()?;
    let text = |key: &str| field_text(entry.get(key)?);
    let message = text("MESSAGE").unwrap_or_default();
    let level = match text("PRIORITY").and_then(|p| p.parse::<u8>().ok()) {
        Some(0..=2) => "fatal",
        Some(3) => "error",
        Some(4) => "warn",
        Some(5 | 6) => "info",
        Some(_) => "debug",
        None => "unknown",
    };
    let timestamp = text("__REALTIME_TIMESTAMP")
        .and_then(|micros| micros.parse::<i128>().ok())
        .and_then(|micros| OffsetDateTime::from_unix_timestamp_nanos(micros * 1000).ok())
        .and_then(|time| time.format(&Rfc3339).ok());
    let mut attributes = Map::new();
    for (key, name) in FIELDS {
        if let Some(value) = text(key) {
            attributes.insert(format!("journal.{name}"), Value::String(value));
        }
    }
    let mut body = json!({
        "level": level,
        "message": message,
        "action": "journal",
        "attributes": attributes,
    });
    if let Some(timestamp) = timestamp {
        body["timestamp"] = Value::String(timestamp);
    }
    Some((
        Record {
            received_at: unix_nanos(),
            origin: None,
            payload: Payload::Json(body.to_string()),
        },
        text("__CURSOR"),
    ))
}

/// Journald отдаёт непечатаемые значения массивом байт.
fn field_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Array(bytes) => {
            let bytes: Option<Vec<u8>> = bytes
                .iter()
                .map(|byte| byte.as_u64().and_then(|b| u8::try_from(b).ok()))
                .collect();
            Some(String::from_utf8_lossy(&bytes?).into_owned())
        }
        _ => None,
    }
}

struct Cursor {
    path: PathBuf,
}

impl Cursor {
    fn load(&self) -> Option<String> {
        let text = std::fs::read_to_string(&self.path).ok()?;
        let text = text.trim();
        (!text.is_empty() && text.len() <= 4096).then(|| text.to_owned())
    }

    fn store(&self, cursor: &str) -> std::io::Result<()> {
        let dir = self.path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir)?;
        let tmp = self.path.with_extension("tmp");
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(cursor.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&tmp, &self.path)?;
        std::fs::File::open(dir)?.sync_all()
    }
}

/// Сохраняет пачку, повторяя при полной очереди; false - остановка.
async fn save(
    collector: &Collector,
    records: &mut Vec<Record>,
    cursor: &mut Option<String>,
    state: &Cursor,
    stop: &CancellationToken,
) -> bool {
    if records.is_empty() {
        return true;
    }
    loop {
        match collector.save(records.clone()).await {
            Ok(()) => break,
            Err(SpoolError::TooLarge) => {
                warn!(records = records.len(), "journal batch too large; skipped");
                break;
            }
            Err(error) => {
                warn!(%error, "journal batch waits for queue space");
                tokio::select! {
                    _ = stop.cancelled() => return false,
                    _ = tokio::time::sleep(FLUSH) => {}
                }
            }
        }
    }
    records.clear();
    if let Some(cursor) = cursor.take() {
        let path = state.path.clone();
        let result = tokio::task::spawn_blocking(move || Cursor { path }.store(&cursor)).await;
        if !matches!(result, Ok(Ok(()))) {
            warn!("journal cursor was not saved; entries may repeat after restart");
        }
    }
    true
}

/// Читает JSON-строки до EOF или остановки; true - EOF.
async fn pump(
    reader: impl AsyncBufRead + Unpin,
    collector: &Collector,
    state: &Cursor,
    track_cursor: bool,
    stop: &CancellationToken,
) -> bool {
    let mut lines = reader.lines();
    let mut records = Vec::new();
    let mut cursor = None;
    let mut deadline = tokio::time::Instant::now() + FLUSH;
    loop {
        let next = tokio::select! {
            _ = stop.cancelled() => {
                save(collector, &mut records, &mut cursor, state, stop).await;
                return false;
            }
            _ = tokio::time::sleep_until(deadline) => {
                if !save(collector, &mut records, &mut cursor, state, stop).await {
                    return false;
                }
                deadline = tokio::time::Instant::now() + FLUSH;
                continue;
            }
            line = lines.next_line() => line,
        };
        match next {
            Ok(Some(line)) if line.len() <= MAX_LINE => {
                if let Some((record, entry_cursor)) = convert(&line) {
                    records.push(record);
                    if track_cursor && entry_cursor.is_some() {
                        cursor = entry_cursor;
                    }
                }
            }
            Ok(Some(line)) => warn!(bytes = line.len(), "journal entry too large; skipped"),
            Ok(None) | Err(_) => {
                save(collector, &mut records, &mut cursor, state, stop).await;
                return true;
            }
        }
        if records.len() >= BATCH && !save(collector, &mut records, &mut cursor, state, stop).await
        {
            return false;
        }
    }
}

fn command(settings: &JournalSettings) -> Command {
    let mut command = Command::new(&settings.journalctl);
    command
        .args(["--output=json", "--no-pager", "--quiet", "--all"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    command
}

async fn backfill(
    settings: &JournalSettings,
    until: i64,
    collector: &Collector,
    state: &Cursor,
    stop: &CancellationToken,
) {
    let mut command = command(settings);
    command.args([
        format!("--until=@{until}"),
        "SYSLOG_FACILITY=4".into(),
        "+".into(),
        "SYSLOG_FACILITY=10".into(),
    ]);
    let Ok(mut child) = command.spawn() else {
        warn!("journal auth backfill could not start");
        return;
    };
    let Some(stdout) = child.stdout.take() else {
        return;
    };
    info!("journal auth history backfill started");
    pump(BufReader::new(stdout), collector, state, false, stop).await;
    let _ = child.kill().await;
}

pub async fn run(
    settings: JournalSettings,
    state_path: PathBuf,
    collector: Collector,
    stop: CancellationToken,
) {
    if !Path::new(&settings.journalctl).exists() {
        warn!(path = %settings.journalctl, "journalctl not found; journal collection disabled");
        return;
    }
    let state = Cursor { path: state_path };
    loop {
        let saved = state.load();
        let mut command = command(&settings);
        command.arg("--follow");
        match &saved {
            Some(cursor) => {
                command.arg(format!("--after-cursor={cursor}"));
            }
            None => {
                command.arg("--lines=0");
            }
        }
        command.args(&settings.matches);
        let started = unix_nanos() / 1_000_000_000;
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                warn!(%error, "journalctl could not start");
                tokio::select! {
                    _ = stop.cancelled() => return,
                    _ = tokio::time::sleep(RESTART) => continue,
                }
            }
        };
        let Some(stdout) = child.stdout.take() else {
            return;
        };
        let follow = pump(BufReader::new(stdout), &collector, &state, true, &stop);
        let finished = if saved.is_none() && settings.backfill_auth {
            // Догрузка идёт параллельно с новыми записями; возможны единичные повторы на границе.
            let history = backfill(&settings, started, &collector, &state, &stop);
            let (finished, ()) = tokio::join!(follow, history);
            finished
        } else {
            follow.await
        };
        let _ = child.kill().await;
        if !finished {
            return;
        }
        warn!("journalctl exited; restarting");
        tokio::select! {
            _ = stop.cancelled() => return,
            _ = tokio::time::sleep(RESTART) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_journal_entry_with_byte_message_and_priority() {
        let line = r#"{"__CURSOR":"s=1;i=2","__REALTIME_TIMESTAMP":"1763211600123456","PRIORITY":"5","MESSAGE":[115,117,100,111,10],"_SYSTEMD_UNIT":"ssh.service","SYSLOG_IDENTIFIER":"sudo","_PID":"42","_UID":"0","_SOURCE_REALTIME_TIMESTAMP":"1"}"#;
        let (record, cursor) = convert(line).unwrap();
        assert_eq!(cursor.as_deref(), Some("s=1;i=2"));
        let Payload::Json(json) = record.payload else {
            panic!()
        };
        let value: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["message"], "sudo\n");
        assert_eq!(value["level"], "info");
        assert!(
            value["timestamp"]
                .as_str()
                .unwrap()
                .starts_with("2025-11-15T13:00:00.123456")
        );
        assert_eq!(value["attributes"]["journal.unit"], "ssh.service");
        assert_eq!(value["attributes"]["journal.identifier"], "sudo");
        assert_eq!(value["attributes"]["journal.pid"], "42");
        assert!(
            value["attributes"]
                .get("journal._SOURCE_REALTIME_TIMESTAMP")
                .is_none()
        );
        assert!(convert("not json").is_none());
        let (record, _) = convert(r#"{"PRIORITY":"2","MESSAGE":"panic"}"#).unwrap();
        let Payload::Json(json) = record.payload else {
            panic!()
        };
        assert!(json.contains(r#""level":"fatal""#));
    }

    #[tokio::test]
    async fn cursor_is_stored_only_after_durable_save() {
        let dir = tempfile::tempdir().unwrap();
        let cursor = Cursor {
            path: dir.path().join("state/journal.cursor"),
        };
        assert_eq!(cursor.load(), None);
        cursor.store("s=abc;i=9").unwrap();
        assert_eq!(cursor.load().as_deref(), Some("s=abc;i=9"));
    }

    #[test]
    fn rejects_options_in_matches() {
        let mut settings = JournalSettings {
            matches: vec!["--since=yesterday".into()],
            ..JournalSettings::default()
        };
        assert!(settings.validate().is_err());
        settings.matches = vec!["_SYSTEMD_UNIT=ssh.service".into(), "+".into()];
        assert!(settings.validate().is_ok());
    }
}
