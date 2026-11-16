//! Алерты на логи: новые совпадения запроса с прошлой проверки.
//!
//! Правило хранит строку запроса страницы Logs и разобранные фильтры. Core отбирает
//! события по sequence, поэтому поздняя доставка не теряется и не считается дважды.
//! Получатели, группы, outbox и отправка в Telegram общие с алертами метрик.

use std::collections::{BTreeMap, BTreeSet};

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use url::Url;
use uuid::Uuid;

use crate::{
    alerts::{
        OUTBOX_TTL_MS, RenderedTelegramMessage, finish_single_outbox, insert_deliveries,
        parse_timestamp_ms, render_telegram_message,
    },
    api::{ApiError, AppState},
    core_client::{AlertSeriesStatus, LogAlertReply, LogAlertRule},
    store::{Store, StoreError, now_ms},
    token_access::{Scope, TokenAccess},
};

/// Совпадает с лимитом правил одного запроса Core.
pub const MAX_LOG_ALERTS: i64 = 100;
const FIELDS: &[&str] = &[
    "environment",
    "server",
    "backend",
    "service",
    "service_instance",
    "action",
    "request_id",
    "level",
    "relay_id",
    "agent_id",
];
const LEVELS: &[&str] = &[
    "trace", "debug", "info", "warn", "error", "fatal", "unknown",
];
const SAMPLE_CHARS: usize = 300;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LogAlertText {
    pub query: String,
    #[serde(default = "default_mode")]
    pub mode: String,
}

fn default_mode() -> String {
    "all".into()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogAlertInput {
    pub name: String,
    pub enabled: bool,
    /// Строка запроса страницы Logs; используется в ссылке уведомления.
    pub query: String,
    #[serde(default)]
    pub filters: BTreeMap<String, Vec<String>>,
    pub text: Option<LogAlertText>,
    /// Минимум новых совпадений за одну проверку (раз в минуту).
    pub threshold: u32,
    /// Пауза между сообщениями; совпадения паузы приходят одной сводкой.
    pub cooldown_seconds: u32,
    pub group_ids: Vec<String>,
    #[serde(default)]
    pub message_template: Option<String>,
    #[serde(default)]
    pub message_fields: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LogAlert {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub query: String,
    pub filters: BTreeMap<String, Vec<String>>,
    pub text: Option<LogAlertText>,
    pub threshold: u32,
    pub cooldown_seconds: u32,
    pub group_ids: Vec<String>,
    pub message_template: Option<String>,
    pub message_fields: Vec<String>,
    pub revision: i64,
    pub no_recipients: bool,
    pub last_count: u64,
    pub last_match_at: Option<i64>,
    pub last_checked_at: Option<i64>,
    pub last_sent_at: Option<i64>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum LogAlertError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("invalid log alert")]
    Invalid(&'static str),
    #[error("log alert conflict")]
    Conflict(&'static str),
    #[error("log alert not found")]
    NotFound,
}

#[derive(Debug)]
pub enum LogAlertToolError {
    Invalid(&'static str),
    Conflict(&'static str),
    NotFound,
    Unavailable,
}

impl From<LogAlertError> for LogAlertToolError {
    fn from(error: LogAlertError) -> Self {
        match error {
            LogAlertError::Store(error) => {
                tracing::error!(%error, "log alert storage failed");
                Self::Unavailable
            }
            LogAlertError::Invalid(message) => Self::Invalid(message),
            LogAlertError::Conflict(code) => Self::Conflict(code),
            LogAlertError::NotFound => Self::NotFound,
        }
    }
}

impl From<StoreError> for LogAlertToolError {
    fn from(error: StoreError) -> Self {
        tracing::error!(%error, "log alert storage failed");
        Self::Unavailable
    }
}

impl From<LogAlertError> for ApiError {
    fn from(error: LogAlertError) -> Self {
        match error {
            LogAlertError::Store(error) => error.into(),
            LogAlertError::Invalid(message) => ApiError::Invalid(message.into()),
            LogAlertError::Conflict(code) => ApiError::Conflict(code),
            LogAlertError::NotFound => ApiError::NotFound,
        }
    }
}

fn printable(value: &str) -> bool {
    !value.chars().any(char::is_control)
}

fn apply_log_alert_scope(input: &mut LogAlertInput, scope: Scope) -> Result<(), LogAlertToolError> {
    if let Some((field, id)) = scope.core_param() {
        let expected = id.to_string();
        if input
            .filters
            .get(field)
            .is_some_and(|values| values.len() != 1 || values[0] != expected)
        {
            return Err(LogAlertToolError::Invalid(
                "filters cannot set a different node than this token",
            ));
        }
        input.filters.insert(field.to_owned(), vec![expected]);
    }
    Ok(())
}

fn preserve_log_alert_node_filters(
    input: &mut LogAlertInput,
    current: &LogAlert,
) -> Result<(), LogAlertToolError> {
    for field in ["relay_id", "agent_id"] {
        match (
            current.filters.get(field).cloned(),
            input.filters.get(field).cloned(),
        ) {
            (Some(current), Some(provided)) if current != provided => {
                return Err(LogAlertToolError::Invalid(
                    "updates cannot change a rule's node filters",
                ));
            }
            (Some(current), None) => {
                input.filters.insert(field.to_owned(), current);
            }
            (None, Some(_)) => {
                return Err(LogAlertToolError::Invalid(
                    "updates cannot add node filters to a rule",
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

fn exactly_node_filter(filters: &BTreeMap<String, Vec<String>>, field: &str, id: i64) -> bool {
    filters
        .get(field)
        .is_some_and(|values| values.len() == 1 && values[0] == id.to_string())
}

fn log_alert_filters_visible(
    filters: &BTreeMap<String, Vec<String>>,
    scope: Scope,
    agent_relays: &BTreeMap<i64, i64>,
) -> bool {
    match scope {
        Scope::All => true,
        Scope::Agent { id } => exactly_node_filter(filters, "agent_id", id),
        Scope::Relay { id } => {
            exactly_node_filter(filters, "relay_id", id)
                || filters.get("agent_id").is_some_and(|agents| {
                    !agents.is_empty()
                        && agents.iter().all(|agent| {
                            agent
                                .parse::<i64>()
                                .ok()
                                .and_then(|agent| agent_relays.get(&agent))
                                == Some(&id)
                        })
                })
        }
    }
}

/// Проверка до Core: одна ошибочная строка иначе отклонила бы запрос всех правил.
fn validate(input: &LogAlertInput) -> Result<(), LogAlertError> {
    if input.name.trim().is_empty() || input.name.chars().count() > 100 || !printable(&input.name) {
        return Err(LogAlertError::Invalid(
            "name must contain 1..100 printable characters",
        ));
    }
    if input.query.len() > 8192 || !printable(&input.query) {
        return Err(LogAlertError::Invalid(
            "query must be printable and at most 8192 bytes",
        ));
    }
    if input.filters.len() > FIELDS.len() {
        return Err(LogAlertError::Invalid("too many filters"));
    }
    for (field, values) in &input.filters {
        if !FIELDS.contains(&field.as_str()) {
            return Err(LogAlertError::Invalid("unsupported filter field"));
        }
        if values.is_empty()
            || values.len() > 64
            || values
                .iter()
                .any(|value| value.is_empty() || value.len() > 4096 || !printable(value))
        {
            return Err(LogAlertError::Invalid(
                "each filter requires 1..64 printable values",
            ));
        }
        let valid = match field.as_str() {
            "level" => values.iter().all(|value| LEVELS.contains(&value.as_str())),
            "relay_id" | "agent_id" => values
                .iter()
                .all(|value| value.parse::<i64>().is_ok_and(|id| id > 0)),
            _ => true,
        };
        if !valid {
            return Err(LogAlertError::Invalid("invalid level or node id"));
        }
    }
    if let Some(text) = &input.text {
        let tokens = text
            .query
            .split(|c: char| !c.is_alphanumeric())
            .filter(|token| !token.is_empty())
            .count();
        if !(1..=64).contains(&tokens)
            || text.query.len() > 8192
            || !matches!(text.mode.as_str(), "all" | "phrase")
        {
            return Err(LogAlertError::Invalid(
                "text requires 1..64 words and mode all or phrase",
            ));
        }
    }
    if input.filters.is_empty() && input.text.is_none() {
        return Err(LogAlertError::Invalid(
            "log alert requires a filter or text",
        ));
    }
    if !(1..=1_000_000).contains(&input.threshold) || input.cooldown_seconds > 86_400 {
        return Err(LogAlertError::Invalid(
            "threshold 1..1000000, cooldown 0..86400 s",
        ));
    }
    if input.group_ids.iter().collect::<BTreeSet<_>>().len() != input.group_ids.len() {
        return Err(LogAlertError::Invalid("duplicate alert group"));
    }
    if input.message_template.as_ref().is_some_and(|template| {
        template.chars().count() > 2000
            || template
                .chars()
                .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    }) {
        return Err(LogAlertError::Invalid(
            "message template must be printable and at most 2000 characters",
        ));
    }
    if let Some(fields) = &input.message_fields
        && (fields.iter().any(|field| {
            ![
                "name",
                "title",
                "description",
                "service",
                "server",
                "environment",
                "condition",
                "value",
                "time",
                "last_point",
                "status",
                "count",
                "query",
                "samples",
            ]
            .contains(&field.as_str())
        }) || fields.iter().collect::<BTreeSet<_>>().len() != fields.len())
    {
        return Err(LogAlertError::Invalid("invalid message fields"));
    }
    Ok(())
}

fn read_log_alerts(conn: &Connection) -> rusqlite::Result<Vec<LogAlert>> {
    let mut alerts = BTreeMap::<String, LogAlert>::new();
    {
        let mut statement = conn.prepare(
            "SELECT a.id,a.name,a.enabled,a.query,a.filters,a.text,a.text_mode,a.threshold,
                    a.cooldown_seconds,a.revision,s.last_count,s.last_match_at,s.last_checked_at,
                    s.last_sent_at,a.message_template,a.message_fields
             FROM log_alerts a LEFT JOIN log_alert_states s ON s.alert_id=a.id
             ORDER BY a.name,a.id",
        )?;
        let rows = statement.query_map([], |row| {
            let filters: String = row.get(4)?;
            let text: Option<String> = row.get(5)?;
            Ok(LogAlert {
                id: row.get(0)?,
                name: row.get(1)?,
                enabled: row.get::<_, i64>(2)? != 0,
                query: row.get(3)?,
                filters: serde_json::from_str(&filters)
                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
                text: text.map(|query| LogAlertText {
                    query,
                    mode: row.get(6).unwrap_or_else(|_| default_mode()),
                }),
                threshold: row.get::<_, i64>(7)? as u32,
                cooldown_seconds: row.get::<_, i64>(8)? as u32,
                group_ids: vec![],
                revision: row.get(9)?,
                no_recipients: true,
                last_count: row.get::<_, Option<i64>>(10)?.unwrap_or(0) as u64,
                last_match_at: row.get(11)?,
                last_checked_at: row.get(12)?,
                last_sent_at: row.get(13)?,
                message_template: row.get(14)?,
                message_fields: serde_json::from_str::<Vec<String>>(&row.get::<_, String>(15)?)
                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
            })
        })?;
        for alert in rows {
            let alert = alert?;
            alerts.insert(alert.id.clone(), alert);
        }
    }
    let mut statement = conn.prepare(
        "SELECT u.alert_id,u.group_id,
                EXISTS(SELECT 1 FROM metric_alert_group_recipients gr WHERE gr.group_id=u.group_id)
         FROM log_alert_groups_used u ORDER BY u.alert_id,u.group_id",
    )?;
    for row in statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, bool>(2)?,
        ))
    })? {
        let (id, group, has_recipients) = row?;
        if let Some(alert) = alerts.get_mut(&id) {
            alert.group_ids.push(group);
            alert.no_recipients &= !has_recipients;
        }
    }
    let mut alerts: Vec<_> = alerts.into_values().collect();
    alerts.sort_by(|a, b| (&a.name, &a.id).cmp(&(&b.name, &b.id)));
    Ok(alerts)
}

impl Store {
    pub async fn log_alerts(&self) -> Result<Vec<LogAlert>, StoreError> {
        self.call(|conn| read_log_alerts(conn)).await
    }

    pub(crate) async fn save_log_alert(
        &self,
        id: Option<String>,
        input: LogAlertInput,
    ) -> Result<String, LogAlertError> {
        self.save_log_alert_at_revision(id, input, None).await
    }

    pub(crate) async fn save_log_alert_checked(
        &self,
        id: String,
        input: LogAlertInput,
        expected_revision: i64,
    ) -> Result<String, LogAlertError> {
        self.save_log_alert_at_revision(Some(id), input, Some(expected_revision))
            .await
    }

    async fn save_log_alert_at_revision(
        &self,
        id: Option<String>,
        input: LogAlertInput,
        expected_revision: Option<i64>,
    ) -> Result<String, LogAlertError> {
        validate(&input)?;
        let filters = serde_json::to_string(&input.filters)
            .map_err(|_| LogAlertError::Invalid("invalid filters"))?;
        self.call(move |conn| {
            let tx = conn.transaction()?;
            let old: Option<(i64, i64, Option<String>, String)> = match &id {
                Some(id) => tx
                    .query_row(
                        "SELECT revision,enabled,message_template,message_fields FROM log_alerts WHERE id=?1",
                        [id],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                    )
                    .optional()?,
                None => None,
            };
            if expected_revision.is_some_and(|expected| {
                old.as_ref().map(|(revision, _, _, _)| *revision) != Some(expected)
            }) {
                return Ok(Err(LogAlertError::Conflict("log_alert_changed")));
            }
            if id.is_some() && old.is_none() {
                return Ok(Err(LogAlertError::NotFound));
            }
            for group_id in &input.group_ids {
                let exists: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM metric_alert_groups WHERE id=?1)",
                    [group_id],
                    |row| row.get(0),
                )?;
                if !exists {
                    return Ok(Err(LogAlertError::Conflict("group_not_found")));
                }
            }
            let enabled: i64 = tx.query_row(
                "SELECT count(*) FROM log_alerts WHERE enabled=1",
                [],
                |row| row.get(0),
            )?;
            let was_enabled = old.as_ref().is_some_and(|(_, enabled, ..)| *enabled != 0);
            if input.enabled && !was_enabled && enabled >= MAX_LOG_ALERTS {
                return Ok(Err(LogAlertError::Conflict("log_alert_limit")));
            }
            let (text, mode) = match &input.text {
                Some(text) => (Some(text.query.clone()), text.mode.clone()),
                None => (None, default_mode()),
            };
            let now = now_ms();
            let id = match (id, old) {
                (Some(id), Some((revision, _, old_template, old_fields))) => {
                    // Сообщения прежней версии правила больше не отправляются.
                    tx.execute(
                        "UPDATE metric_alert_deliveries SET status='cancelled',last_error=NULL
                         WHERE status IN ('pending','sending') AND event_id IN
                         (SELECT id FROM metric_alert_outbox WHERE log_alert_id=?1)",
                        [&id],
                    )?;
                    crate::alerts::finish_outbox(&tx, now)?;
                    let message_template = input
                        .message_template
                        .clone()
                        .map(|value| if value.trim().is_empty() { None } else { Some(value) })
                        .unwrap_or(old_template);
                    let fields = input.message_fields.clone().unwrap_or_else(|| {
                        serde_json::from_str(&old_fields)
                            .unwrap_or_else(|_| crate::alerts::default_log_message_fields())
                    });
                    let fields = serde_json::to_string(&fields).map_err(|error| {
                        rusqlite::Error::ToSqlConversionFailure(Box::new(error))
                    })?;
                    tx.execute(
                        "UPDATE log_alerts SET name=?2,enabled=?3,query=?4,filters=?5,text=?6,text_mode=?7,
                         threshold=?8,cooldown_seconds=?9,revision=?10,message_template=?11,message_fields=?12 WHERE id=?1",
                        params![id, input.name, input.enabled as i64, input.query, filters, text, mode,
                            input.threshold, input.cooldown_seconds, revision + 1,message_template,fields],
                    )?;
                    id
                }
                _ => {
                    let id = Uuid::new_v4().to_string();
                    let message_template = input
                        .message_template
                        .clone()
                        .filter(|value| !value.trim().is_empty());
                    let message_fields = input
                        .message_fields
                        .clone()
                        .unwrap_or_else(crate::alerts::default_log_message_fields);
                    let message_fields = serde_json::to_string(&message_fields).map_err(|error| {
                        rusqlite::Error::ToSqlConversionFailure(Box::new(error))
                    })?;
                    tx.execute(
                        "INSERT INTO log_alerts(id,name,enabled,query,filters,text,text_mode,threshold,
                         cooldown_seconds,revision,created_at,message_template,message_fields)
                         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,1,?10,?11,?12)",
                        params![id, input.name, input.enabled as i64, input.query, filters, text, mode,
                            input.threshold, input.cooldown_seconds, now,message_template,message_fields],
                    )?;
                    id
                }
            };
            tx.execute("DELETE FROM log_alert_groups_used WHERE alert_id=?1", [&id])?;
            for group_id in &input.group_ids {
                tx.execute(
                    "INSERT INTO log_alert_groups_used(alert_id,group_id) VALUES(?1,?2)",
                    params![id, group_id],
                )?;
            }
            // Новое условие начинает с текущего watermark Core.
            tx.execute(
                "INSERT INTO log_alert_states(alert_id) VALUES(?1)
                 ON CONFLICT(alert_id) DO UPDATE SET after_sequence=NULL,suppressed=0,last_count=0",
                [&id],
            )?;
            tx.execute("UPDATE metric_alert_global SET revision=revision+1 WHERE singleton=1", [])?;
            tx.commit()?;
            Ok(Ok(id))
        })
        .await?
    }

    pub(crate) async fn delete_log_alert(&self, id: String) -> Result<bool, StoreError> {
        Ok(self
            .delete_log_alert_at_revision(id, None)
            .await?
            .expect("unconditional log-alert delete cannot conflict"))
    }

    pub(crate) async fn delete_log_alert_checked(
        &self,
        id: String,
        expected_revision: i64,
    ) -> Result<bool, LogAlertError> {
        self.delete_log_alert_at_revision(id, Some(expected_revision))
            .await?
            .ok_or(LogAlertError::Conflict("log_alert_changed"))
    }

    async fn delete_log_alert_at_revision(
        &self,
        id: String,
        expected_revision: Option<i64>,
    ) -> Result<Option<bool>, StoreError> {
        self.call(move |conn| {
            let tx = conn.transaction()?;
            if let Some(expected_revision) = expected_revision {
                let current_revision: Option<i64> = tx
                    .query_row(
                        "SELECT revision FROM log_alerts WHERE id=?1",
                        [&id],
                        |row| row.get(0),
                    )
                    .optional()?;
                if current_revision != Some(expected_revision) {
                    return Ok(None);
                }
            }
            let changed = tx.execute("DELETE FROM log_alerts WHERE id=?1", [&id])?;
            if changed > 0 {
                tx.execute(
                    "UPDATE metric_alert_global SET revision=revision+1 WHERE singleton=1",
                    [],
                )?;
            }
            tx.commit()?;
            Ok(Some(changed > 0))
        })
        .await
    }

    async fn log_core_status(&self) -> Result<String, StoreError> {
        self.call(|conn| {
            conn.query_row(
                "SELECT log_core_status FROM metric_alert_global WHERE singleton=1",
                [],
                |row| row.get(0),
            )
        })
        .await
    }

    async fn record_log_core_status(&self, status: &'static str) -> Result<(), StoreError> {
        self.call(move |conn| {
            conn.execute(
                "UPDATE metric_alert_global SET log_core_status=?1 WHERE singleton=1",
                [status],
            )
        })
        .await
        .map(|_| ())
    }

    /// Фиксирует watermark и события всех правил одной транзакцией.
    pub(crate) async fn commit_log_evaluation(
        &self,
        global_revision: i64,
        alerts: Vec<LogAlert>,
        reply: LogAlertReply,
        public_url: Url,
    ) -> Result<(), StoreError> {
        self.call(move |conn| {
            let tx = conn.transaction()?;
            let (enabled, revision): (i64, i64) = tx.query_row(
                "SELECT enabled,revision FROM metric_alert_global WHERE singleton=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            // Правило изменено во время запроса: результат относится к старой версии.
            if enabled == 0 || revision != global_revision {
                tx.commit()?;
                return Ok(());
            }
            let now = now_ms();
            let as_of = parse_timestamp_ms(&reply.as_of);
            for (alert, result) in alerts.iter().zip(&reply.results) {
                let (after, suppressed, last_sent): (Option<i64>, i64, Option<i64>) = tx.query_row(
                    "SELECT after_sequence,suppressed,last_sent_at FROM log_alert_states WHERE alert_id=?1",
                    [&alert.id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )?;
                let watermark = reply.watermark.min(i64::MAX as u64) as i64;
                // Базовая линия или Core с новыми данными (watermark уменьшился): без событий.
                if after.is_none_or(|after| after > watermark) {
                    tx.execute(
                        "UPDATE log_alert_states SET after_sequence=?2,suppressed=0,last_count=0,
                         last_checked_at=?3 WHERE alert_id=?1",
                        params![alert.id, watermark, now],
                    )?;
                    continue;
                }
                let count = result.count.min(i64::MAX as u64) as i64;
                let cooldown_passed = last_sent.is_none_or(|sent| {
                    now.saturating_sub(sent) >= alert.cooldown_seconds as i64 * 1000
                });
                let matched = count >= alert.threshold as i64;
                let mut sent_at = last_sent;
                let mut next_suppressed = suppressed;
                if matched && cooldown_passed {
                    let message = log_alert_message(alert, count, suppressed, &result.samples, &reply.as_of, &public_url);
                    enqueue_log_event(&tx, alert, &message, now)?;
                    sent_at = Some(now);
                    next_suppressed = 0;
                } else if matched {
                    next_suppressed = suppressed.saturating_add(count);
                } else if suppressed > 0 && cooldown_passed {
                    let message = log_alert_message(alert, 0, suppressed, &[], &reply.as_of, &public_url);
                    enqueue_log_event(&tx, alert, &message, now)?;
                    sent_at = Some(now);
                    next_suppressed = 0;
                }
                let last_match = result
                    .samples
                    .first()
                    .and_then(|event| event["timestamp"].as_str())
                    .and_then(parse_timestamp_ms);
                tx.execute(
                    "UPDATE log_alert_states SET after_sequence=?2,suppressed=?3,last_sent_at=?4,
                     last_count=?5,last_match_at=COALESCE(?6,last_match_at),last_checked_at=?7
                     WHERE alert_id=?1",
                    params![alert.id, watermark, next_suppressed, sent_at, count, last_match, as_of.unwrap_or(now)],
                )?;
            }
            tx.commit()?;
            Ok(())
        })
        .await
    }
}

pub async fn mcp_log_alerts(
    app: &AppState,
    access: &TokenAccess,
) -> Result<Vec<LogAlert>, LogAlertToolError> {
    let alerts = app.store.log_alerts().await?;
    let agent_relays = match access.node_scope {
        Scope::Relay { .. } => Some(
            crate::alerts::agent_relays(app)
                .await
                .map_err(|_| LogAlertToolError::Unavailable)?,
        ),
        Scope::All | Scope::Agent { .. } => None,
    };
    let empty_relays = BTreeMap::new();
    let agent_relays = agent_relays.as_ref().unwrap_or(&empty_relays);
    Ok(alerts
        .into_iter()
        .filter(|alert| log_alert_filters_visible(&alert.filters, access.node_scope, agent_relays))
        .collect())
}

pub async fn mcp_save_log_alert(
    app: &AppState,
    access: &TokenAccess,
    id: Option<String>,
    mut input: LogAlertInput,
) -> Result<LogAlert, LogAlertToolError> {
    let expected_revision = if let Some(id) = &id {
        let current = mcp_log_alerts(app, access)
            .await?
            .into_iter()
            .find(|alert| alert.id == *id)
            .ok_or(LogAlertToolError::NotFound)?;
        preserve_log_alert_node_filters(&mut input, &current)?;
        Some(current.revision)
    } else {
        validate(&input)?;
        apply_log_alert_scope(&mut input, access.node_scope)?;
        None
    };
    let saved = match (id, expected_revision) {
        (Some(id), Some(expected_revision)) => {
            app.store
                .save_log_alert_checked(id, input, expected_revision)
                .await?
        }
        (None, None) => app.store.save_log_alert(None, input).await?,
        _ => unreachable!("MCP log-alert updates always carry a captured revision"),
    };
    mcp_log_alerts(app, access)
        .await?
        .into_iter()
        .find(|alert| alert.id == saved)
        .ok_or(LogAlertToolError::NotFound)
}

pub async fn mcp_delete_log_alert(
    app: &AppState,
    access: &TokenAccess,
    id: String,
) -> Result<(), LogAlertToolError> {
    let current = mcp_log_alerts(app, access)
        .await?
        .into_iter()
        .find(|alert| alert.id == id)
        .ok_or(LogAlertToolError::NotFound)?;
    if app
        .store
        .delete_log_alert_checked(id, current.revision)
        .await?
    {
        Ok(())
    } else {
        Err(LogAlertToolError::NotFound)
    }
}

fn enqueue_log_event(
    tx: &rusqlite::Transaction<'_>,
    alert: &LogAlert,
    message: &RenderedTelegramMessage,
    now: i64,
) -> rusqlite::Result<()> {
    let event_id = Uuid::new_v4().to_string();
    let previous: Option<i64> = tx.query_row(
        "SELECT max(created_at) FROM metric_alert_outbox WHERE log_alert_id=?1",
        [&alert.id],
        |row| row.get(0),
    )?;
    let created_at = previous.map_or(now, |previous| now.max(previous.saturating_add(1)));
    tx.execute(
        "INSERT INTO metric_alert_outbox(id,alert_id,log_alert_id,alert_revision,event_kind,message,created_at,expires_at,parse_mode,reply_markup)
         VALUES(?1,NULL,?2,?3,'log_match',?4,?5,?6,?7,?8)",
        params![event_id, alert.id, alert.revision, message.text, created_at, created_at + OUTBOX_TTL_MS,
            message.parse_mode,message.reply_markup],
    )?;
    let recipients = {
        let mut statement = tx.prepare(
            "SELECT DISTINCT r.id,r.name,r.chat_id FROM log_alert_groups_used g
             JOIN metric_alert_group_recipients gr ON gr.group_id=g.group_id
             JOIN metric_alert_recipients r ON r.id=gr.recipient_id
             WHERE g.alert_id=?1 ORDER BY r.id",
        )?;
        statement
            .query_map([&alert.id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?
            .collect::<Result<Vec<(String, String, i64)>, _>>()?
    };
    insert_deliveries(tx, &event_id, &recipients, now)?;
    finish_single_outbox(tx, &event_id, now)
}

fn truncate(text: &str, limit: usize) -> String {
    let single: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if single.chars().count() <= limit {
        single
    } else {
        let mut cut: String = single.chars().take(limit).collect();
        cut.push('…');
        cut
    }
}

/// Формирует HTML-уведомление и сохраняет кнопку вместе с уже отрендеренным текстом outbox.
pub(crate) fn log_alert_message(
    alert: &LogAlert,
    count: i64,
    suppressed: i64,
    samples: &[Value],
    as_of: &str,
    public_url: &Url,
) -> RenderedTelegramMessage {
    let selected = |field: &str| alert.message_fields.iter().any(|value| value == field);
    let first = samples.first();
    let sample_field = |event: Option<&Value>, name: &str| {
        event
            .and_then(|event| event[name].as_str())
            .unwrap_or("")
            .to_owned()
    };
    let server = sample_field(first, "server");
    let service = sample_field(first, "service");
    let environment = sample_field(first, "environment");
    let last_point = sample_field(first, "timestamp");
    let mut source = Vec::new();
    if selected("name") || selected("title") {
        source.push(format!("Имя алерта: {}", alert.name));
    }
    if selected("server") && !server.is_empty() {
        source.push(format!("Сервер: {server}"));
    }
    if selected("service") && !service.is_empty() {
        source.push(format!("Сервис: {service}"));
    }
    if selected("environment") && !environment.is_empty() {
        source.push(format!("Окружение: {environment}"));
    }
    if selected("query") {
        source.push(format!("Запрос: {}", truncate(&alert.query, 500)));
    }
    if selected("samples") {
        for event in samples.iter().take(3) {
            let field = |name: &str| event[name].as_str().unwrap_or("");
            source.push(format!(
                "{} {}/{} {}: {}",
                field("timestamp"),
                field("server"),
                field("service"),
                field("level"),
                truncate(field("message"), SAMPLE_CHARS),
            ));
        }
    }
    let mut condition = Vec::new();
    if selected("count") || selected("value") {
        condition.push(format!("Новых совпадений: {count}"));
    }
    if suppressed > 0 && (selected("count") || selected("value")) {
        condition.push(format!("Совпадений за время паузы: {suppressed}"));
    }
    if selected("condition") {
        condition.push(format!("Порог: не меньше {} за проверку", alert.threshold));
    }
    let mut time = Vec::new();
    if selected("time") {
        time.push(format!("Время Core: {as_of}"));
    }
    if selected("last_point") && !last_point.is_empty() {
        time.push(format!("Последнее совпадение: {last_point}"));
    }
    let status = if count > 0 {
        "сработал"
    } else {
        "сводка совпадений"
    };
    let mut placeholders = BTreeMap::new();
    placeholders.insert("name", alert.name.clone());
    placeholders.insert("title", alert.name.clone());
    placeholders.insert("description", String::new());
    placeholders.insert("service", service);
    placeholders.insert("server", server);
    placeholders.insert("environment", environment);
    placeholders.insert(
        "condition",
        format!("не меньше {} совпадений за проверку", alert.threshold),
    );
    placeholders.insert("value", count.to_string());
    placeholders.insert("time", as_of.to_owned());
    placeholders.insert(
        "last_point",
        if last_point.is_empty() {
            "нет данных".into()
        } else {
            last_point
        },
    );
    placeholders.insert("status", status.to_owned());
    placeholders.insert("count", count.to_string());
    placeholders.insert("query", alert.query.clone());
    render_telegram_message(
        if count > 0 {
            "Алерт на логи сработал"
        } else {
            "Сводка алерта на логи"
        },
        "🔴",
        alert.message_template.as_deref(),
        &placeholders,
        vec![
            ("Источник", source),
            ("Условие / значение", condition),
            ("Время", time),
        ],
        &logs_link(public_url, alert),
    )
}

fn logs_link(public_url: &Url, alert: &LogAlert) -> String {
    let mut url = public_url
        .join("logs")
        .unwrap_or_else(|_| public_url.clone());
    let mut query = url.query_pairs_mut();
    query
        .append_pair("q", &alert.query)
        .append_pair("range", "1h");
    for field in ["relay_id", "agent_id"] {
        if let Some(values) = alert.filters.get(field) {
            for value in values {
                query.append_pair(field, value);
            }
        }
    }
    drop(query);
    url.to_string()
}

/// Проверка всех включённых правил одним запросом; вызывается из `alerts::watch`.
pub async fn run_log_pass(app: &AppState, global_revision: i64) {
    let Ok(alerts) = app.store.log_alerts().await else {
        return;
    };
    let alerts: Vec<LogAlert> = alerts.into_iter().filter(|alert| alert.enabled).collect();
    if alerts.is_empty() {
        return;
    }
    let after: BTreeMap<String, Option<i64>> = match app
        .store
        .call(|conn| {
            let mut statement =
                conn.prepare("SELECT alert_id,after_sequence FROM log_alert_states")?;
            statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect()
        })
        .await
    {
        Ok(after) => after,
        Err(_) => return,
    };
    let rules: Vec<LogAlertRule> = alerts
        .iter()
        .map(|alert| LogAlertRule {
            id: alert.id.clone(),
            filters: alert.filters.clone(),
            text: alert
                .text
                .as_ref()
                .map(|text| json!({"query": text.query, "mode": text.mode})),
            after_sequence: after
                .get(&alert.id)
                .copied()
                .flatten()
                .and_then(|after| u64::try_from(after).ok()),
        })
        .collect();
    match app.core.log_alert_query(&rules).await {
        Ok(reply) => {
            let _ = app.store.record_log_core_status("available").await;
            if let Err(error) = app
                .store
                .commit_log_evaluation(
                    global_revision,
                    alerts,
                    reply,
                    app.config.public_url.clone(),
                )
                .await
            {
                tracing::warn!("log alert evaluation could not be committed: {error}");
            }
        }
        Err(status) => {
            let status = match status {
                AlertSeriesStatus::Busy => "busy",
                AlertSeriesStatus::Misconfigured => "incompatible",
                AlertSeriesStatus::Timeout | AlertSeriesStatus::Unavailable => "unavailable",
            };
            let _ = app.store.record_log_core_status(status).await;
        }
    }
}

pub async fn list(
    State(app): State<AppState>,
    _user: crate::session::AuthUser,
) -> Result<Json<Value>, ApiError> {
    let alerts = app.store.log_alerts().await?;
    let core_status = app.store.log_core_status().await?;
    Ok(Json(json!({"alerts": alerts, "core_status": core_status})))
}

pub async fn create(
    State(app): State<AppState>,
    _user: crate::session::AuthUser,
    Json(input): Json<LogAlertInput>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let id = app.store.save_log_alert(None, input).await?;
    Ok((StatusCode::CREATED, Json(json!({"id": id}))))
}

pub async fn update(
    State(app): State<AppState>,
    _user: crate::session::AuthUser,
    Path(id): Path<String>,
    Json(input): Json<LogAlertInput>,
) -> Result<Json<Value>, ApiError> {
    Uuid::parse_str(&id).map_err(|_| ApiError::BadRequest("invalid_request"))?;
    let id = app.store.save_log_alert(Some(id), input).await?;
    Ok(Json(json!({"id": id})))
}

pub async fn delete(
    State(app): State<AppState>,
    _user: crate::session::AuthUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    Uuid::parse_str(&id).map_err(|_| ApiError::BadRequest("invalid_request"))?;
    if app.store.delete_log_alert(id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_log_link_includes_actual_node_filters() {
        let alert = LogAlert {
            id: "alert".into(),
            name: "node rule".into(),
            enabled: true,
            query: "service:api".into(),
            filters: BTreeMap::from([
                ("agent_id".into(), vec!["9".into()]),
                ("relay_id".into(), vec!["4".into()]),
                ("service".into(), vec!["api".into()]),
            ]),
            text: None,
            threshold: 1,
            cooldown_seconds: 0,
            group_ids: vec![],
            message_template: None,
            message_fields: crate::alerts::default_log_message_fields(),
            revision: 1,
            no_recipients: false,
            last_count: 0,
            last_match_at: None,
            last_checked_at: None,
            last_sent_at: None,
        };
        let public_url = Url::parse("https://dock.example/").unwrap();
        let link = Url::parse(&logs_link(&public_url, &alert)).unwrap();
        let pairs: Vec<_> = link
            .query_pairs()
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        assert_eq!(
            pairs,
            vec![
                ("q".to_owned(), "service:api".to_owned()),
                ("range".to_owned(), "1h".to_owned()),
                ("relay_id".to_owned(), "4".to_owned()),
                ("agent_id".to_owned(), "9".to_owned()),
            ]
        );
    }
}
