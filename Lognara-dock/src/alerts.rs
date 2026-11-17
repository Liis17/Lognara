//! Общие настройки, правила, состояние и durable-доставка алертов метрик.

use std::{
    collections::{BTreeMap, BTreeSet},
    net::IpAddr,
    time::Duration,
};

use axum::{
    Json,
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::time::{Instant, MissedTickBehavior, interval_at};
use url::Url;
use uuid::Uuid;

use crate::{
    api::{ApiError, AppState},
    core_client::{AlertSeriesQuery, AlertSeriesReply, AlertSeriesStatus, AlertSeriesWindow},
    series::MetricKey,
    store::{Store, StoreError, now_ms},
    token_access::{Scope, TokenAccess},
};

const MAX_ENABLED: i64 = 100;
const MAX_TRIGGERS: usize = 20;
const ALERT_SEND_PASS_LIMIT: usize = 10;
const ALERT_SEND_PASS_BUDGET: Duration = Duration::from_secs(30);
const NO_DATA_LOSS_MS: i64 = 5 * 60 * 1000;
pub(crate) const OUTBOX_TTL_MS: i64 = 24 * 60 * 60 * 1000;
const OUTBOX_RETENTION_MS: i64 = 7 * 24 * 60 * 60 * 1000;
const TELEGRAM_API: &str = "https://api.telegram.org/";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TriggerInput {
    pub window_seconds: u64,
    pub aggregate: String,
    pub comparator: String,
    pub threshold: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DefinitionInput {
    pub key: MetricKey,
    /// Узел задаёт только сервер (токен MCP); браузерный ввод всегда получает `all`.
    #[serde(skip)]
    pub node_scope: Scope,
    pub enabled: bool,
    pub environment: Option<String>,
    pub no_data: String,
    pub combine: String,
    pub group_ids: Vec<String>,
    pub triggers: Vec<TriggerInput>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub message_template: Option<String>,
    #[serde(default)]
    pub message_fields: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AlertDefinition {
    pub id: String,
    pub key: MetricKey,
    /// Узел правила; `all` для правил, созданных в UI.
    pub node_scope: Scope,
    pub enabled: bool,
    pub environment: Option<String>,
    pub no_data: String,
    pub combine: String,
    pub group_ids: Vec<String>,
    pub triggers: Vec<TriggerInput>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub message_template: Option<String>,
    pub message_fields: Vec<String>,
    pub revision: i64,
    pub enabled_at: i64,
    pub state: String,
    pub loss_registered: bool,
    pub loss_at: Option<i64>,
    pub last_point_at: Option<i64>,
    pub last_as_of: Option<i64>,
    pub no_recipients: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Recipient {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub chat_id: i64,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Group {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub recipient_ids: Vec<String>,
    pub alerts: Vec<MetricKey>,
    /// Имена алертов на логи с этой группой.
    pub log_alerts: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct Settings {
    pub enabled: bool,
    pub token_configured: bool,
    pub send_paused: bool,
    pub send_pause_reason: Option<String>,
    pub core_status: String,
    pub core_checked_at: Option<i64>,
    pub core_error: Option<String>,
    pub recipients: Vec<Recipient>,
    pub groups: Vec<Group>,
    pub alerts: Vec<AlertDefinition>,
    pub pending_deliveries: u64,
    pub permanent_errors: u64,
}

#[derive(Debug, Serialize)]
pub struct Summary {
    pub enabled: bool,
    pub alerts: Vec<SummaryAlert>,
}

#[derive(Debug, Serialize)]
pub struct SummaryAlert {
    pub key: MetricKey,
    pub node_scope: Scope,
    pub enabled: bool,
    pub state: String,
    pub no_recipients: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecipientInput {
    pub kind: String,
    pub name: String,
    pub chat_id: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupInput {
    pub kind: String,
    pub name: String,
    pub recipient_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GlobalInput {
    pub enabled: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenInput {
    pub token: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessagePreviewInput {
    pub kind: String,
    pub name: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub server: Option<String>,
    #[serde(default)]
    pub service: Option<String>,
    #[serde(default)]
    pub origin: Option<String>,
    #[serde(default)]
    pub environment: Option<String>,
    #[serde(default)]
    pub condition: Option<String>,
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub time: Option<String>,
    #[serde(default)]
    pub last_point: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub count: Option<u64>,
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub message_template: Option<String>,
    #[serde(default)]
    pub message_fields: Vec<String>,
}

#[derive(Debug, Clone)]
struct GlobalConfig {
    enabled: bool,
    token_configured: bool,
    send_paused: bool,
    send_pause_reason: Option<String>,
    throttle_until: i64,
    revision: i64,
}

#[derive(Debug, thiserror::Error)]
enum AlertStoreError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("invalid alert settings")]
    Invalid(&'static str),
    #[error("alert settings conflict")]
    Conflict(&'static str),
    #[error("alert record not found")]
    NotFound,
    #[error("recipient group is still used by alerts")]
    GroupInUse(Vec<MetricKey>, Vec<String>),
    #[error("group recipients must match its type")]
    GroupKindMismatch,
    #[error("recipient type is used by a group")]
    RecipientKindInUse,
}

const METRIC_MESSAGE_FIELDS: &[&str] = &[
    "name",
    "title",
    "description",
    "service",
    "server",
    "origin",
    "node",
    "environment",
    "condition",
    "value",
    "time",
    "last_point",
    "status",
    "count",
    "query",
    "samples",
];

pub(crate) fn default_metric_message_fields() -> Vec<String> {
    [
        "name",
        "description",
        "service",
        "server",
        "origin",
        "node",
        "environment",
        "condition",
        "value",
        "time",
        "last_point",
        "status",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

pub(crate) fn default_log_message_fields() -> Vec<String> {
    ["name", "count", "query", "samples", "time", "status"]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

fn valid_message_text(value: &str, max_chars: usize) -> bool {
    value.chars().count() <= max_chars
        && !value
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
}

fn validate_message_fields(fields: &[String], allowed: &[&str]) -> Result<(), AlertStoreError> {
    if fields
        .iter()
        .any(|field| !allowed.contains(&field.as_str()))
        || fields.iter().collect::<BTreeSet<_>>().len() != fields.len()
    {
        return Err(AlertStoreError::Invalid("invalid message fields"));
    }
    Ok(())
}

fn validate_key(key: &MetricKey) -> Result<(), AlertStoreError> {
    if key.server.is_empty()
        || key.server.len() > 128
        || key.service.is_empty()
        || key.service.len() > 128
        || key.name.is_empty()
        || key.name.len() > 128
        || !matches!(key.origin.as_str(), "counter" | "gauge" | "rule")
        // Управляющие символы подделали бы строки текста уведомления.
        || [&key.server, &key.service, &key.name]
            .iter()
            .any(|value| value.chars().any(char::is_control))
    {
        return Err(AlertStoreError::Invalid("invalid metric identity"));
    }
    Ok(())
}

fn validate_definition(input: &DefinitionInput) -> Result<(), AlertStoreError> {
    validate_key(&input.key)?;
    if input
        .environment
        .as_ref()
        .is_some_and(|value| value.len() > 128 || value.chars().any(char::is_control))
    {
        return Err(AlertStoreError::Invalid("invalid environment"));
    }
    if !matches!(input.no_data.as_str(), "normal" | "loss") {
        return Err(AlertStoreError::Invalid("no_data must be normal or loss"));
    }
    if !matches!(input.combine.as_str(), "and" | "or") {
        return Err(AlertStoreError::Invalid("combine must be and or or"));
    }
    if input.triggers.is_empty() || input.triggers.len() > MAX_TRIGGERS {
        return Err(AlertStoreError::Invalid("alerts require 1..20 triggers"));
    }
    if input.triggers.iter().any(|trigger| {
        !(60..=604_800).contains(&trigger.window_seconds)
            || !matches!(
                trigger.aggregate.as_str(),
                "avg" | "min" | "max" | "sum" | "count" | "increase"
            )
            || !matches!(trigger.comparator.as_str(), ">" | "<")
            || !trigger.threshold.is_finite()
    }) {
        return Err(AlertStoreError::Invalid("invalid alert trigger"));
    }
    if input.group_ids.iter().collect::<BTreeSet<_>>().len() != input.group_ids.len() {
        return Err(AlertStoreError::Invalid("duplicate alert group"));
    }
    for value in [&input.title, &input.description, &input.message_template]
        .into_iter()
        .flatten()
    {
        if !valid_message_text(value, 2000) {
            return Err(AlertStoreError::Invalid(
                "message text must be printable and at most 2000 characters",
            ));
        }
    }
    if let Some(fields) = &input.message_fields {
        validate_message_fields(fields, METRIC_MESSAGE_FIELDS)?;
    }
    Ok(())
}

fn validate_name(name: &str) -> Result<(), AlertStoreError> {
    if name.trim().is_empty() || name.chars().count() > 100 || name.chars().any(char::is_control) {
        return Err(AlertStoreError::Invalid(
            "name must contain 1..100 printable characters",
        ));
    }
    Ok(())
}

fn validate_recipient(input: &RecipientInput) -> Result<(), AlertStoreError> {
    validate_name(&input.name)?;
    if !matches!(input.kind.as_str(), "personal" | "channel") {
        return Err(AlertStoreError::Invalid(
            "recipient kind must be personal or channel",
        ));
    }
    if input.chat_id == 0 || input.chat_id.unsigned_abs() > 4_503_599_627_370_495 {
        return Err(AlertStoreError::Invalid(
            "chat_id must be a Telegram numeric ID",
        ));
    }
    Ok(())
}

fn validate_group(input: &GroupInput) -> Result<(), AlertStoreError> {
    validate_name(&input.name)?;
    if !matches!(input.kind.as_str(), "personal" | "channel") {
        return Err(AlertStoreError::Invalid(
            "group kind must be personal or channel",
        ));
    }
    if input.recipient_ids.iter().collect::<BTreeSet<_>>().len() != input.recipient_ids.len() {
        return Err(AlertStoreError::Invalid("duplicate group recipient"));
    }
    Ok(())
}

fn read_global(conn: &Connection) -> rusqlite::Result<GlobalConfig> {
    conn.query_row(
        "SELECT enabled, telegram_token IS NOT NULL, send_paused, send_pause_reason,
                throttle_until, revision FROM metric_alert_global WHERE singleton=1",
        [],
        |row| {
            Ok(GlobalConfig {
                enabled: row.get::<_, i64>(0)? != 0,
                token_configured: row.get::<_, i64>(1)? != 0,
                send_paused: row.get::<_, i64>(2)? != 0,
                send_pause_reason: row.get(3)?,
                throttle_until: row.get(4)?,
                revision: row.get(5)?,
            })
        },
    )
}

fn read_alerts(conn: &Connection) -> rusqlite::Result<Vec<AlertDefinition>> {
    let mut alerts = BTreeMap::<String, AlertDefinition>::new();
    {
        let mut statement = conn.prepare(
            "SELECT a.id,a.server,a.service,a.name,a.origin,a.enabled,a.environment,
                    a.no_data,a.combine_mode,a.revision,s.condition_state,s.loss_registered,
                    a.enabled_at,s.loss_at,s.last_point_at,s.last_as_of,a.scope_kind,a.scope_id,
                    a.title,a.description,a.message_template,a.message_fields
             FROM metric_alerts a LEFT JOIN metric_alert_states s ON s.alert_id=a.id
             ORDER BY a.server,a.service,a.name,a.origin,a.scope_kind,a.scope_id",
        )?;
        for row in statement.query_map([], |row| {
            let id: String = row.get(0)?;
            let key = MetricKey {
                server: row.get(1)?,
                service: row.get(2)?,
                name: row.get(3)?,
                origin: row.get(4)?,
            };
            let node_scope = Scope::from_columns(&row.get::<_, String>(16)?, row.get(17)?)
                .ok_or(rusqlite::Error::InvalidQuery)?;
            Ok((
                id.clone(),
                AlertDefinition {
                    id,
                    key,
                    node_scope,
                    enabled: row.get::<_, i64>(5)? != 0,
                    environment: row.get(6)?,
                    no_data: row.get(7)?,
                    combine: row.get(8)?,
                    group_ids: vec![],
                    triggers: vec![],
                    revision: row.get(9)?,
                    state: row
                        .get::<_, Option<String>>(10)?
                        .unwrap_or_else(|| "unknown".into()),
                    loss_registered: row.get::<_, Option<i64>>(11)?.unwrap_or(0) != 0,
                    enabled_at: row.get(12)?,
                    loss_at: row.get(13)?,
                    last_point_at: row.get(14)?,
                    last_as_of: row.get(15)?,
                    no_recipients: true,
                    title: row.get(18)?,
                    description: row.get(19)?,
                    message_template: row.get(20)?,
                    message_fields: serde_json::from_str::<Vec<String>>(&row.get::<_, String>(21)?)
                        .map_err(|_| rusqlite::Error::InvalidQuery)?,
                },
            ))
        })? {
            let (id, alert) = row?;
            alerts.insert(id, alert);
        }
    }
    {
        let mut statement = conn.prepare(
            "SELECT alert_id,window_seconds,aggregate,comparator,threshold
             FROM metric_alert_triggers ORDER BY alert_id,position",
        )?;
        for row in statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                TriggerInput {
                    window_seconds: row.get::<_, i64>(1)? as u64,
                    aggregate: row.get(2)?,
                    comparator: row.get(3)?,
                    threshold: row.get(4)?,
                },
            ))
        })? {
            let (id, trigger) = row?;
            if let Some(alert) = alerts.get_mut(&id) {
                alert.triggers.push(trigger);
            }
        }
    }
    {
        let mut statement = conn.prepare(
            "SELECT alert_id,group_id FROM metric_alert_groups_used ORDER BY alert_id,group_id",
        )?;
        for row in statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })? {
            let (id, group) = row?;
            if let Some(alert) = alerts.get_mut(&id) {
                alert.group_ids.push(group);
            }
        }
    }
    {
        let mut statement = conn.prepare(
            "SELECT ag.alert_id,count(DISTINCT gr.recipient_id)
             FROM metric_alert_groups_used ag
             LEFT JOIN metric_alert_group_recipients gr ON gr.group_id=ag.group_id
             GROUP BY ag.alert_id",
        )?;
        for row in statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })? {
            let (id, count) = row?;
            if let Some(alert) = alerts.get_mut(&id) {
                alert.no_recipients = count == 0;
            }
        }
    }
    Ok(alerts.into_values().collect())
}

impl Store {
    async fn alert_global(&self) -> Result<GlobalConfig, AlertStoreError> {
        Ok(self.call(|conn| read_global(conn)).await?)
    }

    async fn alert_token(&self) -> Result<Option<String>, StoreError> {
        self.call(|conn| {
            conn.query_row(
                "SELECT telegram_token FROM metric_alert_global WHERE singleton=1",
                [],
                |row| row.get(0),
            )
        })
        .await
    }

    async fn set_alert_token(&self, token: String) -> Result<(), AlertStoreError> {
        self.call(move |conn| {
            conn.execute(
                "UPDATE metric_alert_global SET telegram_token=?1,send_paused=0,
                 send_pause_reason=NULL,throttle_until=0,revision=revision+1 WHERE singleton=1",
                [token],
            )
        })
        .await?;
        Ok(())
    }

    async fn set_alert_global(&self, enabled: bool) -> Result<bool, AlertStoreError> {
        let updated = self.call(move |conn| {
            let tx = conn.transaction()?;
            let current = read_global(&tx)?;
            if enabled && !current.token_configured {
                tx.commit()?;
                return Ok(false);
            }
            let enabled_changed = current.enabled != enabled;
            let clear_pause = enabled && current.send_paused;
            if enabled_changed || clear_pause {
                tx.execute(
                    "UPDATE metric_alert_global SET enabled=?1,
                     send_paused=CASE WHEN ?1=1 THEN 0 ELSE send_paused END,
                     send_pause_reason=CASE WHEN ?1=1 THEN NULL ELSE send_pause_reason END,
                     revision=revision+1 WHERE singleton=1",
                    [if enabled { 1 } else { 0 }],
                )?;
                if enabled_changed && !enabled {
                    tx.execute(
                        "UPDATE metric_alert_deliveries SET status='cancelled',last_error=NULL
                         WHERE status IN ('pending','sending')",
                        [],
                    )?;
                    finish_outbox(&tx, now_ms())?;
                } else if enabled_changed {
                    tx.execute(
                        "UPDATE metric_alert_states SET condition_state='unknown',loss_registered=0,
                         loss_at=NULL,last_point_at=NULL,last_as_of=NULL
                         WHERE alert_id IN (SELECT id FROM metric_alerts WHERE enabled=1)",
                        [],
                    )?;
                    tx.execute(
                        "UPDATE metric_alerts SET enabled_at=?1 WHERE enabled=1",
                        [now_ms()],
                    )?;
                    // Лог-алерты начинают с текущего watermark: события паузы не рассылаются.
                    tx.execute(
                        "UPDATE log_alert_states SET after_sequence=NULL,suppressed=0",
                        [],
                    )?;
                }
                if !enabled {
                    tx.execute(
                        "UPDATE metric_alert_global SET core_loss_registered=0,core_loss_event_id=NULL,
                         core_status='disabled',core_checked_at=?1,core_error=NULL WHERE singleton=1",
                        [now_ms()],
                    )?;
                }
            }
            tx.commit()?;
            Ok(true)
        })
        .await?;
        Ok(updated)
    }

    async fn alert_settings(&self) -> Result<Settings, AlertStoreError> {
        self.call(|conn| {
            let global = read_global(conn)?;
            let recipients = {
                let mut statement = conn.prepare(
                    "SELECT id,kind,name,chat_id,last_error FROM metric_alert_recipients ORDER BY kind,name,id",
                )?;
                statement
                    .query_map([], |row| {
                        Ok(Recipient {
                            id: row.get(0)?,
                            kind: row.get(1)?,
                            name: row.get(2)?,
                            chat_id: row.get(3)?,
                            last_error: row.get(4)?,
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()?
            };
            let mut groups = vec![];
            {
                let mut statement = conn.prepare(
                    "SELECT id,kind,name FROM metric_alert_groups ORDER BY kind,name,id",
                )?;
                for row in statement.query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?))
                })? {
                    let (id, kind, name) = row?;
                    let recipient_ids = conn
                        .prepare("SELECT recipient_id FROM metric_alert_group_recipients WHERE group_id=?1 ORDER BY recipient_id")?
                        .query_map([&id], |row| row.get(0))?
                        .collect::<Result<Vec<String>, _>>()?;
                    let mut alert_statement = conn.prepare(
                        "SELECT a.server,a.service,a.name,a.origin FROM metric_alert_groups_used u
                         JOIN metric_alerts a ON a.id=u.alert_id WHERE u.group_id=?1
                         ORDER BY a.server,a.service,a.name,a.origin",
                    )?;
                    let alerts = alert_statement
                        .query_map([&id], |row| {
                            Ok(MetricKey {
                                server: row.get(0)?,
                                service: row.get(1)?,
                                name: row.get(2)?,
                                origin: row.get(3)?,
                            })
                        })?
                        .collect::<Result<Vec<_>, _>>()?;
                    let log_alerts = group_log_alerts(conn, &id)?;
                    groups.push(Group { id, kind, name, recipient_ids, alerts, log_alerts });
                }
            }
            let alerts = read_alerts(conn)?;
            let pending_deliveries = conn.query_row(
                "SELECT count(*) FROM metric_alert_deliveries WHERE status='pending'",
                [],
                |row| row.get::<_, i64>(0),
            )? as u64;
            let permanent_errors = conn.query_row(
                "SELECT count(*) FROM metric_alert_deliveries WHERE status='permanent_error'",
                [],
                |row| row.get::<_, i64>(0),
            )? as u64;
            let (core_status, core_checked_at, core_error): (String, Option<i64>, Option<String>) = conn.query_row(
                "SELECT core_status,core_checked_at,core_error FROM metric_alert_global WHERE singleton=1",
                [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            Ok(Settings {
                enabled: global.enabled,
                token_configured: global.token_configured,
                send_paused: global.send_paused,
                send_pause_reason: global.send_pause_reason,
                core_status,
                core_checked_at,
                core_error,
                recipients,
                groups,
                alerts,
                pending_deliveries,
                permanent_errors,
            })
        })
        .await
        .map_err(Into::into)
    }

    async fn alert_summary(&self) -> Result<Summary, AlertStoreError> {
        self.call(|conn| {
            let global = read_global(conn)?;
            let alerts = read_alerts(conn)?
                .into_iter()
                .map(|alert| SummaryAlert {
                    key: alert.key,
                    node_scope: alert.node_scope,
                    enabled: alert.enabled,
                    state: alert.state,
                    no_recipients: alert.no_recipients,
                })
                .collect();
            Ok(Summary {
                enabled: global.enabled,
                alerts,
            })
        })
        .await
        .map_err(Into::into)
    }

    /// Правило браузера по ключу: только глобальное (`all`), скоупленные правила MCP отдельно.
    async fn alert_definition(
        &self,
        key: MetricKey,
    ) -> Result<Option<AlertDefinition>, AlertStoreError> {
        validate_key(&key)?;
        self.call(move |conn| {
            Ok(read_alerts(conn)?
                .into_iter()
                .find(|alert| alert.key == key && alert.node_scope == Scope::All))
        })
        .await
        .map_err(Into::into)
    }

    async fn alert_list(&self) -> Result<Vec<AlertDefinition>, AlertStoreError> {
        self.call(|conn| read_alerts(conn))
            .await
            .map_err(Into::into)
    }

    async fn alert_group_list(&self) -> Result<Vec<AlertGroupInfo>, AlertStoreError> {
        self.call(|conn| {
            let mut statement =
                conn.prepare("SELECT id,kind,name FROM metric_alert_groups ORDER BY name,id")?;
            statement
                .query_map([], |row| {
                    Ok(AlertGroupInfo {
                        id: row.get(0)?,
                        kind: row.get(1)?,
                        name: row.get(2)?,
                    })
                })?
                .collect()
        })
        .await
        .map_err(Into::into)
    }

    async fn alert_rows_for_worker(
        &self,
    ) -> Result<(GlobalConfig, Vec<AlertDefinition>), AlertStoreError> {
        self.call(|conn| Ok((read_global(conn)?, read_alerts(conn)?)))
            .await
            .map_err(Into::into)
    }

    /// Создаёт правило или меняет существующее по ключу и скоупу (для браузера это `all`).
    async fn save_alert_definition(
        &self,
        input: DefinitionInput,
    ) -> Result<String, AlertStoreError> {
        self.write_alert_definition(input, AlertWrite::Upsert).await
    }

    /// Создаёт правило, но не перезаписывает существующее с тем же ключом и скоупом.
    async fn create_alert_definition(
        &self,
        input: DefinitionInput,
    ) -> Result<String, AlertStoreError> {
        self.write_alert_definition(input, AlertWrite::Create).await
    }

    /// Меняет правило `id`: ключ можно сменить, скоуп и состояние сохраняются.
    async fn update_alert_definition(
        &self,
        id: String,
        input: DefinitionInput,
    ) -> Result<String, AlertStoreError> {
        self.write_alert_definition(input, AlertWrite::Update(id))
            .await
    }

    async fn write_alert_definition(
        &self,
        input: DefinitionInput,
        mode: AlertWrite,
    ) -> Result<String, AlertStoreError> {
        validate_definition(&input)?;
        let (scope_kind, scope_id) = input.node_scope.columns();
        self.call(move |conn| {
            let tx = conn.transaction()?;
            let global = read_global(&tx)?;
            for group_id in &input.group_ids {
                let exists: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM metric_alert_groups WHERE id=?1)",
                    [group_id],
                    |row| row.get(0),
                )?;
                if !exists {
                    return Ok(Err(AlertStoreError::Conflict("group_not_found")));
                }
            }
            type OldMetricAlert = (String, i64, i64, Option<String>, Option<String>, Option<String>, String);
            let old: Option<OldMetricAlert> = match &mode {
                AlertWrite::Update(id) => tx
                    .query_row(
                        "SELECT id,revision,enabled,title,description,message_template,message_fields
                         FROM metric_alerts WHERE id=?1",
                        [id],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?)),
                    )
                    .optional()?,
                _ => tx
                    .query_row(
                        "SELECT id,revision,enabled,title,description,message_template,message_fields
                         FROM metric_alerts
                         WHERE server=?1 AND service=?2 AND name=?3 AND origin=?4
                           AND scope_kind=?5 AND scope_id=?6",
                        params![input.key.server,input.key.service,input.key.name,input.key.origin,scope_kind,scope_id],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?)),
                    )
                    .optional()?,
            };
            match (&mode, &old) {
                (AlertWrite::Update(_), None) => return Ok(Err(AlertStoreError::NotFound)),
                (AlertWrite::Create, Some(_)) => {
                    return Ok(Err(AlertStoreError::Conflict("alert_exists")));
                }
                _ => {}
            }
            let reset_state = old.is_none() || input.enabled;
            if input.enabled
                && !global.enabled
                && old.as_ref().is_none_or(|(_, _, was_enabled, ..)| *was_enabled == 0)
            {
                return Ok(Err(AlertStoreError::Conflict("global_disabled_or_alert_limit")));
            }
            let current_enabled: i64 = tx.query_row(
                "SELECT count(*) FROM metric_alerts WHERE enabled=1",
                [],
                |row| row.get(0),
            )?;
            let next_enabled = current_enabled
                - old.as_ref().filter(|(_, _, enabled, ..)| *enabled != 0).map_or(0, |_| 1)
                + if input.enabled { 1 } else { 0 };
            if next_enabled > MAX_ENABLED {
                return Ok(Err(AlertStoreError::Conflict("global_disabled_or_alert_limit")));
            }
            let now = now_ms();
            let id = match old {
                Some((id, revision, _, old_title, old_description, old_template, old_fields)) => {
                    cancel_alert_pending(&tx, &id, now)?;
                    let fields: Vec<String> = input.message_fields.clone().unwrap_or_else(|| {
                        serde_json::from_str(&old_fields).unwrap_or_else(|_| default_metric_message_fields())
                    });
                    let fields = serde_json::to_string(&fields).map_err(|error| {
                        rusqlite::Error::ToSqlConversionFailure(Box::new(error))
                    })?;
                    let title = input.title.clone().or(old_title);
                    let description = input.description.clone().or(old_description);
                    let message_template = input
                        .message_template
                        .clone()
                        .map(|value| if value.trim().is_empty() { None } else { Some(value) })
                        .unwrap_or(old_template);
                    tx.execute(
                        "UPDATE metric_alerts SET server=?2,service=?3,name=?4,origin=?5,enabled=?6,environment=?7,
                         no_data=?8,combine_mode=?9,revision=?10,enabled_at=?11,title=?12,description=?13,
                         message_template=?14,message_fields=?15 WHERE id=?1",
                        params![id,input.key.server,input.key.service,input.key.name,input.key.origin,
                            if input.enabled { 1 } else { 0 },input.environment,input.no_data,input.combine,revision+1,now,
                            title,description,message_template,fields],
                    )?;
                    id
                }
                None => {
                    let id = Uuid::new_v4().to_string();
                    let title = input.title.clone();
                    let description = input.description.clone();
                    let message_template = input
                        .message_template
                        .clone()
                        .filter(|value| !value.trim().is_empty());
                    let message_fields = input
                        .message_fields
                        .clone()
                        .unwrap_or_else(default_metric_message_fields);
                    let message_fields = serde_json::to_string(&message_fields).map_err(|error| {
                        rusqlite::Error::ToSqlConversionFailure(Box::new(error))
                    })?;
                    tx.execute(
                        "INSERT INTO metric_alerts(id,server,service,name,origin,enabled,environment,
                         no_data,combine_mode,revision,enabled_at,scope_kind,scope_id,title,description,message_template,message_fields)
                         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,1,?10,?11,?12,?13,?14,?15,?16)",
                        params![id,input.key.server,input.key.service,input.key.name,input.key.origin,
                            if input.enabled { 1 } else { 0 },input.environment,input.no_data,input.combine,now,scope_kind,scope_id,
                            title,description,message_template,message_fields],
                    )?;
                    id
                }
            };
            tx.execute("DELETE FROM metric_alert_triggers WHERE alert_id=?1", [&id])?;
            tx.execute("DELETE FROM metric_alert_groups_used WHERE alert_id=?1", [&id])?;
            for (position, trigger) in input.triggers.iter().enumerate() {
                tx.execute(
                    "INSERT INTO metric_alert_triggers(alert_id,position,window_seconds,aggregate,comparator,threshold)
                     VALUES(?1,?2,?3,?4,?5,?6)",
                    params![id,position as i64,trigger.window_seconds as i64,trigger.aggregate,trigger.comparator,trigger.threshold],
                )?;
            }
            for group_id in &input.group_ids {
                tx.execute(
                    "INSERT INTO metric_alert_groups_used(alert_id,group_id) VALUES(?1,?2)",
                    params![id,group_id],
                )?;
            }
            if reset_state {
                tx.execute(
                    "INSERT INTO metric_alert_states(alert_id,condition_state,loss_registered,loss_at,last_point_at,last_as_of)
                     VALUES(?1,'unknown',0,NULL,NULL,NULL)
                     ON CONFLICT(alert_id) DO UPDATE SET condition_state='unknown',loss_registered=0,
                     loss_at=NULL,last_point_at=NULL,last_as_of=NULL",
                    [&id],
                )?;
            }
            tx.execute(
                "UPDATE metric_alert_global SET revision=revision+1 WHERE singleton=1",
                [],
            )?;
            tx.commit()?;
            Ok(Ok(id))
        })
        .await
        .map_err(map_alert_save_error)?
    }

    /// Удаляет глобальное правило браузера по ключу; скоупленные правила MCP не затрагивает.
    async fn delete_alert_definition(&self, key: MetricKey) -> Result<bool, AlertStoreError> {
        validate_key(&key)?;
        self.call(move |conn| {
            let tx = conn.transaction()?;
            let changed = tx.execute(
                "DELETE FROM metric_alerts WHERE server=?1 AND service=?2 AND name=?3 AND origin=?4 AND scope_kind='all'",
                params![key.server,key.service,key.name,key.origin],
            )?;
            if changed > 0 {
                tx.execute("UPDATE metric_alert_global SET revision=revision+1 WHERE singleton=1", [])?;
            }
            tx.commit()?;
            Ok(changed > 0)
        })
        .await
        .map_err(Into::into)
    }

    /// Удаляет правило по `id`; видимость проверяет вызывающий MCP-сервис.
    async fn delete_alert_by_id(&self, id: String) -> Result<bool, AlertStoreError> {
        self.call(move |conn| {
            let tx = conn.transaction()?;
            let changed = tx.execute("DELETE FROM metric_alerts WHERE id=?1", [&id])?;
            if changed > 0 {
                tx.execute(
                    "UPDATE metric_alert_global SET revision=revision+1 WHERE singleton=1",
                    [],
                )?;
            }
            tx.commit()?;
            Ok(changed > 0)
        })
        .await
        .map_err(Into::into)
    }

    async fn create_recipient(&self, input: RecipientInput) -> Result<Recipient, AlertStoreError> {
        validate_recipient(&input)?;
        let id = Uuid::new_v4().to_string();
        self.call({
            let id = id.clone();
            move |conn| {
                let tx = conn.transaction()?;
                let exists: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM metric_alert_recipients WHERE chat_id=?1)",
                    [input.chat_id],
                    |row| row.get(0),
                )?;
                if exists {
                    tx.commit()?;
                    return Err(rusqlite::Error::InvalidQuery);
                }
                tx.execute(
                    "INSERT INTO metric_alert_recipients(id,kind,name,chat_id,created_at) VALUES(?1,?2,?3,?4,?5)",
                    params![id,input.kind,input.name,input.chat_id,now_ms()],
                )?;
                tx.execute("UPDATE metric_alert_global SET revision=revision+1 WHERE singleton=1", [])?;
                tx.commit()?;
                Ok(Recipient { id, kind: input.kind, name: input.name, chat_id: input.chat_id, last_error: None })
            }
        })
        .await
        .map_err(map_duplicate)
    }

    async fn update_recipient(
        &self,
        id: String,
        input: RecipientInput,
    ) -> Result<Recipient, AlertStoreError> {
        validate_recipient(&input)?;
        self.call(move |conn| {
            let tx = conn.transaction()?;
            let old: Option<(i64, String)> = tx.query_row(
                "SELECT chat_id,kind FROM metric_alert_recipients WHERE id=?1",
                [&id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            ).optional()?;
            let Some((old_chat, old_kind)) = old else {
                tx.commit()?;
                return Err(rusqlite::Error::QueryReturnedNoRows);
            };
            let in_group: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM metric_alert_group_recipients WHERE recipient_id=?1)",
                [&id],
                |row| row.get(0),
            )?;
            if in_group && old_kind != input.kind {
                tx.commit()?;
                return Err(rusqlite::Error::InvalidParameterName("recipient_kind_in_use".into()));
            }
            let duplicate: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM metric_alert_recipients WHERE chat_id=?1 AND id<>?2)",
                params![input.chat_id,id],
                |row| row.get(0),
            )?;
            if duplicate {
                tx.commit()?;
                return Err(rusqlite::Error::InvalidQuery);
            }
            if old_chat != input.chat_id {
                tx.execute(
                    "UPDATE metric_alert_deliveries SET status='cancelled'
                     WHERE recipient_id=?1 AND status IN ('pending','sending')",
                    [&id],
                )?;
                finish_outbox(&tx, now_ms())?;
            }
            tx.execute(
                "UPDATE metric_alert_recipients SET kind=?2,name=?3,chat_id=?4,last_error=NULL WHERE id=?1",
                params![id,input.kind,input.name,input.chat_id],
            )?;
            tx.execute("UPDATE metric_alert_global SET revision=revision+1 WHERE singleton=1", [])?;
            tx.commit()?;
            Ok(Recipient { id, kind: input.kind, name: input.name, chat_id: input.chat_id, last_error: None })
        })
        .await
        .map_err(|error| match error {
            StoreError::Sqlite(rusqlite::Error::InvalidQuery) => AlertStoreError::Conflict("recipient_exists"),
            StoreError::Sqlite(rusqlite::Error::InvalidParameterName(name)) if name == "recipient_kind_in_use" => AlertStoreError::RecipientKindInUse,
            StoreError::Sqlite(rusqlite::Error::QueryReturnedNoRows) => AlertStoreError::NotFound,
            error => AlertStoreError::Store(error),
        })
    }

    async fn delete_recipient(&self, id: String) -> Result<bool, AlertStoreError> {
        self.call(move |conn| {
            let tx = conn.transaction()?;
            tx.execute(
                "UPDATE metric_alert_deliveries SET status='cancelled'
                 WHERE recipient_id=?1 AND status IN ('pending','sending')",
                [&id],
            )?;
            let changed = tx.execute("DELETE FROM metric_alert_recipients WHERE id=?1", [&id])?;
            if changed > 0 {
                tx.execute(
                    "UPDATE metric_alert_global SET revision=revision+1 WHERE singleton=1",
                    [],
                )?;
                finish_outbox(&tx, now_ms())?;
            }
            tx.commit()?;
            Ok(changed > 0)
        })
        .await
        .map_err(Into::into)
    }

    async fn save_group(
        &self,
        id: Option<String>,
        input: GroupInput,
    ) -> Result<Group, AlertStoreError> {
        validate_group(&input)?;
        let update = id.is_some();
        let id = id.unwrap_or_else(|| Uuid::new_v4().to_string());
        self.call({
            let id = id.clone();
            move |conn| {
                let tx = conn.transaction()?;
                for recipient_id in &input.recipient_ids {
                    let kind: Option<String> = tx.query_row(
                        "SELECT kind FROM metric_alert_recipients WHERE id=?1",
                        [recipient_id],
                        |row| row.get(0),
                    ).optional()?;
                    match kind.as_deref() {
                        None => {
                            tx.commit()?;
                            return Err(rusqlite::Error::QueryReturnedNoRows);
                        }
                        Some(kind) if kind != input.kind => {
                            tx.commit()?;
                            return Err(rusqlite::Error::InvalidQuery);
                        }
                        Some(_) => {}
                    }
                }
                let exists = id_is_group(&tx, &id)?;
                if update && !exists {
                    tx.commit()?;
                    return Err(rusqlite::Error::QueryReturnedNoRows);
                }
                if exists {
                    tx.execute(
                        "UPDATE metric_alert_groups SET kind=?2,name=?3 WHERE id=?1",
                        params![id,input.kind,input.name],
                    )?;
                    tx.execute("DELETE FROM metric_alert_group_recipients WHERE group_id=?1", [&id])?;
                } else {
                    tx.execute(
                        "INSERT INTO metric_alert_groups(id,kind,name,created_at) VALUES(?1,?2,?3,?4)",
                        params![id,input.kind,input.name,now_ms()],
                    )?;
                }
                for recipient_id in &input.recipient_ids {
                    tx.execute(
                        "INSERT INTO metric_alert_group_recipients(group_id,recipient_id) VALUES(?1,?2)",
                        params![id,recipient_id],
                    )?;
                }
                tx.execute("UPDATE metric_alert_global SET revision=revision+1 WHERE singleton=1", [])?;
                let alerts = {
                    let mut alert_statement = tx.prepare(
                        "SELECT a.server,a.service,a.name,a.origin FROM metric_alert_groups_used u
                         JOIN metric_alerts a ON a.id=u.alert_id WHERE u.group_id=?1
                         ORDER BY a.server,a.service,a.name,a.origin",
                    )?;
                    alert_statement
                        .query_map([&id], |row| Ok(MetricKey {
                            server: row.get(0)?, service: row.get(1)?, name: row.get(2)?, origin: row.get(3)?,
                        }))?
                        .collect::<Result<Vec<_>, _>>()?
                };
                let log_alerts = group_log_alerts(&tx, &id)?;
                tx.commit()?;
                Ok(Group { id, kind: input.kind, name: input.name, recipient_ids: input.recipient_ids, alerts, log_alerts })
            }
        })
        .await
        .map_err(|error| match error {
            StoreError::Sqlite(rusqlite::Error::InvalidQuery) => AlertStoreError::GroupKindMismatch,
            StoreError::Sqlite(rusqlite::Error::QueryReturnedNoRows) => AlertStoreError::NotFound,
            error => AlertStoreError::Store(error),
        })
    }

    async fn delete_group(&self, id: String) -> Result<(), AlertStoreError> {
        self.call(move |conn| {
            let tx = conn.transaction()?;
            let alerts = {
                let mut statement = tx.prepare(
                    "SELECT a.server,a.service,a.name,a.origin FROM metric_alert_groups_used u
                     JOIN metric_alerts a ON a.id=u.alert_id WHERE u.group_id=?1
                     ORDER BY a.server,a.service,a.name,a.origin",
                )?;
                statement
                    .query_map([&id], |row| {
                        Ok(MetricKey {
                            server: row.get(0)?,
                            service: row.get(1)?,
                            name: row.get(2)?,
                            origin: row.get(3)?,
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()?
            };
            let log_alerts = group_log_alerts(&tx, &id)?;
            if !alerts.is_empty() || !log_alerts.is_empty() {
                tx.commit()?;
                return Ok(Err((alerts, log_alerts)));
            }
            let changed = tx.execute("DELETE FROM metric_alert_groups WHERE id=?1", [&id])?;
            if changed == 0 {
                tx.commit()?;
                return Err(rusqlite::Error::QueryReturnedNoRows);
            }
            tx.execute(
                "UPDATE metric_alert_global SET revision=revision+1 WHERE singleton=1",
                [],
            )?;
            tx.commit()?;
            Ok(Ok(()))
        })
        .await
        .map_err(|error| match error {
            StoreError::Sqlite(rusqlite::Error::QueryReturnedNoRows) => AlertStoreError::NotFound,
            error => AlertStoreError::Store(error),
        })?
        .map_err(|(alerts, log_alerts)| AlertStoreError::GroupInUse(alerts, log_alerts))
    }
}

/// Имена алертов на логи, использующих группу.
pub(crate) fn group_log_alerts(conn: &Connection, group_id: &str) -> rusqlite::Result<Vec<String>> {
    let mut statement = conn.prepare(
        "SELECT a.name FROM log_alert_groups_used u JOIN log_alerts a ON a.id=u.alert_id
         WHERE u.group_id=?1 ORDER BY a.name,a.id",
    )?;
    statement.query_map([group_id], |row| row.get(0))?.collect()
}

fn id_is_group(conn: &Connection, id: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM metric_alert_groups WHERE id=?1)",
        [id],
        |row| row.get(0),
    )
}

fn map_duplicate(error: StoreError) -> AlertStoreError {
    match error {
        StoreError::Sqlite(rusqlite::Error::InvalidQuery) => {
            AlertStoreError::Conflict("recipient_exists")
        }
        error => AlertStoreError::Store(error),
    }
}

fn cancel_alert_pending(conn: &Connection, alert_id: &str, now: i64) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE metric_alert_deliveries SET status='cancelled',last_error=NULL
         WHERE status IN ('pending','sending') AND event_id IN
         (SELECT id FROM metric_alert_outbox WHERE alert_id=?1)",
        [alert_id],
    )?;
    finish_outbox(conn, now)
}

pub(crate) fn finish_outbox(conn: &Connection, now: i64) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE metric_alert_outbox SET finished_at=?1 WHERE finished_at IS NULL
         AND NOT EXISTS(SELECT 1 FROM metric_alert_deliveries d
                       WHERE d.event_id=metric_alert_outbox.id AND d.status='pending')",
        [now],
    )?;
    Ok(())
}

/// Режим записи правила: браузер ищет по ключу и скоупу, MCP создаёт или меняет по `id`.
enum AlertWrite {
    Upsert,
    Create,
    Update(String),
}

fn map_alert_save_error(error: StoreError) -> AlertStoreError {
    match error {
        StoreError::Sqlite(rusqlite::Error::SqliteFailure(failure, _))
            if failure.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE =>
        {
            AlertStoreError::Conflict("alert_exists")
        }
        error => AlertStoreError::Store(error),
    }
}

/// Группа получателей для MCP: только id, тип и имя, без токена и chat ID.
#[derive(Debug, Clone, Serialize)]
pub struct AlertGroupInfo {
    pub id: String,
    pub kind: String,
    pub name: String,
}

/// Ошибка MCP-операций над алертами; транспорт переводит её в безопасный код.
#[derive(Debug)]
pub enum AlertToolError {
    Invalid(&'static str),
    Conflict(&'static str),
    NotFound,
    Unavailable,
}

impl From<AlertStoreError> for AlertToolError {
    fn from(error: AlertStoreError) -> Self {
        match error {
            AlertStoreError::Invalid(message) => Self::Invalid(message),
            AlertStoreError::Conflict(code) => Self::Conflict(code),
            AlertStoreError::NotFound => Self::NotFound,
            AlertStoreError::Store(error) => {
                tracing::error!(%error, "metric alert storage failed");
                Self::Unavailable
            }
            AlertStoreError::GroupInUse(..)
            | AlertStoreError::GroupKindMismatch
            | AlertStoreError::RecipientKindInUse => Self::Conflict("alert_conflict"),
        }
    }
}

/// Relay каждого агента из core. Ошибка или неполный ответ не раскрывает данные: запрос отказывает.
pub(crate) async fn agent_relays(app: &AppState) -> Result<BTreeMap<i64, i64>, AlertToolError> {
    let reply = app
        .core
        .list_agents()
        .await
        .map_err(|_| AlertToolError::Unavailable)?;
    reply["agents"]
        .as_array()
        .and_then(|agents| {
            agents
                .iter()
                .map(|agent| Some((agent["id"].as_i64()?, agent["relay_id"].as_i64()?)))
                .collect::<Option<BTreeMap<_, _>>>()
        })
        .ok_or(AlertToolError::Unavailable)
}

/// Правила, видимые токену: `all` видит всё, `relay` — свой узел и агентов его relay, `agent` — только себя.
async fn visible_alerts(
    app: &AppState,
    access: &TokenAccess,
    alerts: Vec<AlertDefinition>,
) -> Result<Vec<AlertDefinition>, AlertToolError> {
    Ok(match access.node_scope {
        Scope::All => alerts,
        Scope::Agent { id } => alerts
            .into_iter()
            .filter(|alert| alert.node_scope == Scope::Agent { id })
            .collect(),
        Scope::Relay { id } => {
            let relays = agent_relays(app).await?;
            alerts
                .into_iter()
                .filter(|alert| match alert.node_scope {
                    Scope::Relay { id: relay } => relay == id,
                    Scope::Agent { id: agent } => relays.get(&agent) == Some(&id),
                    Scope::All => false,
                })
                .collect()
        }
    })
}

/// Группы получателей для выбора в правиле; создаются только в UI.
pub async fn mcp_alert_groups(app: &AppState) -> Result<Vec<AlertGroupInfo>, AlertToolError> {
    Ok(app.store.alert_group_list().await?)
}

/// Правила, видимые токену.
pub async fn mcp_alerts(
    app: &AppState,
    access: &TokenAccess,
) -> Result<Vec<AlertDefinition>, AlertToolError> {
    let alerts = app.store.alert_list().await?;
    visible_alerts(app, access, alerts).await
}

/// Создаёт правило с узлом токена (`id` нет) или меняет видимое правило `id`; скоуп правила не меняется.
pub async fn mcp_save_alert(
    app: &AppState,
    access: &TokenAccess,
    id: Option<String>,
    mut input: DefinitionInput,
) -> Result<AlertDefinition, AlertToolError> {
    let saved = match id {
        Some(id) => {
            let current = mcp_alerts(app, access)
                .await?
                .into_iter()
                .find(|alert| alert.id == id)
                .ok_or(AlertToolError::NotFound)?;
            input.node_scope = current.node_scope;
            app.store.update_alert_definition(id, input).await?
        }
        None => {
            input.node_scope = access.node_scope;
            app.store.create_alert_definition(input).await?
        }
    };
    app.store
        .alert_list()
        .await?
        .into_iter()
        .find(|alert| alert.id == saved)
        .ok_or(AlertToolError::NotFound)
}

/// Удаляет видимое токену правило.
pub async fn mcp_delete_alert(
    app: &AppState,
    access: &TokenAccess,
    id: String,
) -> Result<(), AlertToolError> {
    if !mcp_alerts(app, access)
        .await?
        .iter()
        .any(|alert| alert.id == id)
    {
        return Err(AlertToolError::NotFound);
    }
    if app.store.delete_alert_by_id(id).await? {
        Ok(())
    } else {
        Err(AlertToolError::NotFound)
    }
}

impl From<AlertStoreError> for ApiError {
    fn from(error: AlertStoreError) -> Self {
        match error {
            AlertStoreError::Store(error) => error.into(),
            AlertStoreError::Invalid(message) => ApiError::Invalid(message.to_owned()),
            AlertStoreError::Conflict(code) => ApiError::Conflict(code),
            AlertStoreError::NotFound => ApiError::NotFound,
            AlertStoreError::GroupInUse(alerts, log_alerts) => {
                ApiError::GroupInUse(alerts, log_alerts)
            }
            AlertStoreError::GroupKindMismatch => {
                ApiError::Invalid("group members must have the same type".into())
            }
            AlertStoreError::RecipientKindInUse => ApiError::Conflict("recipient_kind_in_use"),
        }
    }
}

pub async fn settings(
    State(app): State<AppState>,
    _user: crate::session::AuthUser,
) -> Result<Json<Settings>, ApiError> {
    Ok(Json(app.store.alert_settings().await?))
}

pub async fn summary(
    State(app): State<AppState>,
    _user: crate::session::AuthUser,
) -> Result<Json<Summary>, ApiError> {
    Ok(Json(app.store.alert_summary().await?))
}

pub async fn definition(
    State(app): State<AppState>,
    _user: crate::session::AuthUser,
    Query(key): Query<MetricKey>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({"alert": app.store.alert_definition(key).await?}),
    ))
}

pub async fn save_definition(
    State(app): State<AppState>,
    _user: crate::session::AuthUser,
    Json(input): Json<DefinitionInput>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let id = app.store.save_alert_definition(input.clone()).await?;
    let alert = app
        .store
        .alert_definition(input.key)
        .await?
        .ok_or(ApiError::NotFound)?;
    let status = if alert.revision == 1 {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(json!({"id": id, "alert": alert}))))
}

pub async fn delete_definition(
    State(app): State<AppState>,
    _user: crate::session::AuthUser,
    Json(key): Json<MetricKey>,
) -> Result<StatusCode, ApiError> {
    if app.store.delete_alert_definition(key).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound)
    }
}

pub async fn set_global(
    State(app): State<AppState>,
    _user: crate::session::AuthUser,
    Json(input): Json<GlobalInput>,
) -> Result<StatusCode, ApiError> {
    if input.enabled {
        let token = app
            .store
            .alert_token()
            .await?
            .ok_or(ApiError::Conflict("bot_token_required"))?;
        app.telegram
            .verify_token(&token)
            .await
            .map_err(telegram_verification_api_error)?;
    }
    if !app.store.set_alert_global(input.enabled).await? {
        return Err(ApiError::Conflict("bot_token_required"));
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn set_token(
    State(app): State<AppState>,
    _user: crate::session::AuthUser,
    Json(input): Json<TokenInput>,
) -> Result<StatusCode, ApiError> {
    let token = input.token.trim().to_owned();
    if !valid_token(&token) {
        return Err(ApiError::BadRequest("invalid_bot_token"));
    }
    app.telegram
        .verify_token(&token)
        .await
        .map_err(telegram_verification_api_error)?;
    app.store.set_alert_token(token).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn create_recipient(
    State(app): State<AppState>,
    _user: crate::session::AuthUser,
    Json(input): Json<RecipientInput>,
) -> Result<(StatusCode, Json<Recipient>), ApiError> {
    Ok((
        StatusCode::CREATED,
        Json(app.store.create_recipient(input).await?),
    ))
}

pub async fn update_recipient(
    State(app): State<AppState>,
    _user: crate::session::AuthUser,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(input): Json<RecipientInput>,
) -> Result<Json<Recipient>, ApiError> {
    Ok(Json(app.store.update_recipient(id, input).await?))
}

/// Удаление правила по id из UI: человек снимает и скоупленные правила, созданные через MCP.
pub async fn delete_rule(
    State(app): State<AppState>,
    _user: crate::session::AuthUser,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<StatusCode, ApiError> {
    Uuid::parse_str(&id).map_err(|_| ApiError::BadRequest("invalid_request"))?;
    if app.store.delete_alert_by_id(id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound)
    }
}

pub async fn delete_recipient(
    State(app): State<AppState>,
    _user: crate::session::AuthUser,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<StatusCode, ApiError> {
    if app.store.delete_recipient(id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound)
    }
}

pub async fn create_group(
    State(app): State<AppState>,
    _user: crate::session::AuthUser,
    Json(input): Json<GroupInput>,
) -> Result<(StatusCode, Json<Group>), ApiError> {
    Ok((
        StatusCode::CREATED,
        Json(app.store.save_group(None, input).await?),
    ))
}

pub async fn update_group(
    State(app): State<AppState>,
    _user: crate::session::AuthUser,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(input): Json<GroupInput>,
) -> Result<Json<Group>, ApiError> {
    Ok(Json(app.store.save_group(Some(id), input).await?))
}

pub async fn delete_group(
    State(app): State<AppState>,
    _user: crate::session::AuthUser,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    match app.store.delete_group(id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(AlertStoreError::GroupInUse(alerts, log_alerts)) => (
            StatusCode::CONFLICT,
            Json(json!({"error":"group_in_use","alerts":alerts,"log_alerts":log_alerts})),
        )
            .into_response(),
        Err(error) => ApiError::from(error).into_response(),
    }
}

#[derive(Clone)]
pub struct TelegramClient {
    http: reqwest::Client,
    base: Url,
}

#[derive(Clone)]
struct PendingDelivery {
    event_id: String,
    recipient_id: String,
    chat_id: i64,
    message: String,
    parse_mode: Option<String>,
    reply_markup: Option<String>,
    token: String,
    attempts: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConditionState {
    Unknown,
    Ok,
    Firing,
}

impl ConditionState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Ok => "ok",
            Self::Firing => "firing",
        }
    }
}

enum PassHealth {
    Available,
    Busy,
    Misconfigured,
    Unavailable,
}

/// Один последовательный проход по правилам и отправке durable outbox.
pub async fn watch(app: AppState) {
    let mut ticker = interval_at(
        Instant::now() + Duration::from_secs(60),
        Duration::from_secs(60),
    );
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut core_unavailable_since: Option<Instant> = None;
    loop {
        tokio::select! {
            _ = app.shutdown.cancelled() => break,
            _ = ticker.tick() => {}
        }
        let Ok((global, alerts)) = app.store.alert_rows_for_worker().await else {
            continue;
        };
        if !global.enabled {
            core_unavailable_since = None;
            continue;
        }

        run_alert_pass(&app, global, alerts, &mut core_unavailable_since).await;
    }
}

/// Запрос core для правила. Скоуп узла передаётся, поэтому значения вне узла не оцениваются и не уходят в Telegram.
fn series_query_for(alert: &AlertDefinition) -> AlertSeriesQuery {
    let windows: Vec<u64> = alert
        .triggers
        .iter()
        .map(|trigger| trigger.window_seconds)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    AlertSeriesQuery {
        server: alert.key.server.clone(),
        service: alert.key.service.clone(),
        name: alert.key.name.clone(),
        origin: alert.key.origin.clone(),
        environment: alert.environment.clone(),
        windows_seconds: windows,
        need_increase: alert
            .triggers
            .iter()
            .any(|trigger| trigger.aggregate == "increase"),
        relay_id: match alert.node_scope {
            Scope::Relay { id } => Some(id),
            _ => None,
        },
        agent_id: match alert.node_scope {
            Scope::Agent { id } => Some(id),
            _ => None,
        },
    }
}

async fn run_alert_pass(
    app: &AppState,
    global: GlobalConfig,
    alerts: Vec<AlertDefinition>,
    core_unavailable_since: &mut Option<Instant>,
) {
    let mut outcomes = Vec::new();
    for alert in alerts.into_iter().filter(|alert| alert.enabled) {
        if app.shutdown.is_cancelled() {
            return;
        }
        let Ok(current) = app.store.alert_global().await else {
            break;
        };
        if !current.enabled || current.revision != global.revision {
            break;
        }
        let query = series_query_for(&alert);
        match app.core.alert_series_query(&query).await {
            Ok(reply) => {
                outcomes.push(Ok(()));
                if let Err(error) = app
                    .store
                    .commit_alert_evaluation(
                        global.revision,
                        alert,
                        reply,
                        app.config.public_url.clone(),
                    )
                    .await
                {
                    tracing::warn!("metric alert evaluation could not be committed: {error}");
                }
            }
            Err(status) => {
                outcomes.push(Err(status));
                if matches!(
                    status,
                    AlertSeriesStatus::Unavailable | AlertSeriesStatus::Timeout
                ) {
                    record_core_unavailable(app, core_unavailable_since).await;
                    send_pending(app).await;
                    return;
                }
            }
        }
    }

    if !outcomes.is_empty() {
        match classify_health(&outcomes) {
            PassHealth::Available => {
                *core_unavailable_since = None;
                let _ = app
                    .store
                    .record_core_health("available", None, None, app.config.public_url.clone())
                    .await;
            }
            PassHealth::Busy => {
                *core_unavailable_since = None;
                let _ = app
                    .store
                    .record_core_health(
                        "busy",
                        Some("overloaded"),
                        None,
                        app.config.public_url.clone(),
                    )
                    .await;
            }
            PassHealth::Misconfigured => {
                *core_unavailable_since = None;
                let _ = app
                    .store
                    .record_core_health(
                        "incompatible",
                        Some("incompatible"),
                        None,
                        app.config.public_url.clone(),
                    )
                    .await;
            }
            PassHealth::Unavailable => {
                record_core_unavailable(app, core_unavailable_since).await;
            }
        }
    } else if let Ok(current) = app.store.alert_global().await
        && current.enabled
        && current.revision == global.revision
    {
        let probe = AlertSeriesQuery {
            server: "__dock_health_check__".into(),
            service: "dock".into(),
            name: "health_check".into(),
            origin: "counter".into(),
            environment: None,
            windows_seconds: vec![60],
            need_increase: false,
            relay_id: None,
            agent_id: None,
        };
        let result = app.core.alert_series_query(&probe).await;
        match classify_health(&[result.map(|_| ())]) {
            PassHealth::Available => {
                *core_unavailable_since = None;
                let _ = app
                    .store
                    .record_core_health("available", None, None, app.config.public_url.clone())
                    .await;
            }
            PassHealth::Busy => {
                *core_unavailable_since = None;
                let _ = app
                    .store
                    .record_core_health(
                        "busy",
                        Some("overloaded"),
                        None,
                        app.config.public_url.clone(),
                    )
                    .await;
            }
            PassHealth::Misconfigured => {
                *core_unavailable_since = None;
                let _ = app
                    .store
                    .record_core_health(
                        "incompatible",
                        Some("incompatible"),
                        None,
                        app.config.public_url.clone(),
                    )
                    .await;
            }
            PassHealth::Unavailable => {
                record_core_unavailable(app, core_unavailable_since).await;
            }
        }
    }
    // Отдельный статус Core: старый Core без запроса логов не останавливает алерты метрик.
    crate::log_alerts::run_log_pass(app, global.revision).await;
    send_pending(app).await;
}

async fn record_core_unavailable(app: &AppState, since: &mut Option<Instant>) {
    let started = *since.get_or_insert_with(Instant::now);
    let lost = (Instant::now() - started).as_millis() >= NO_DATA_LOSS_MS as u128;
    let loss_message = lost.then(|| {
        format!(
            "Core не отвечает более 5 минут. Алерты метрик приостановлены до восстановления.\n{}",
            settings_link(&app.config.public_url),
        )
    });
    let _ = app
        .store
        .record_core_health(
            "unavailable",
            Some("unavailable"),
            loss_message,
            app.config.public_url.clone(),
        )
        .await;
}

fn classify_health(outcomes: &[Result<(), AlertSeriesStatus>]) -> PassHealth {
    if outcomes.iter().any(Result::is_ok) {
        PassHealth::Available
    } else if outcomes
        .iter()
        .any(|result| matches!(result, Err(AlertSeriesStatus::Misconfigured)))
    {
        PassHealth::Misconfigured
    } else if outcomes
        .iter()
        .any(|result| matches!(result, Err(AlertSeriesStatus::Busy)))
    {
        PassHealth::Busy
    } else {
        PassHealth::Unavailable
    }
}

impl Store {
    async fn commit_alert_evaluation(
        &self,
        global_revision: i64,
        alert: AlertDefinition,
        reply: AlertSeriesReply,
        public_url: Url,
    ) -> Result<(), StoreError> {
        self.call(move |conn| {
            let tx = conn.transaction()?;
            let current_global: (i64, i64) = tx.query_row(
                "SELECT enabled,revision FROM metric_alert_global WHERE singleton=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            if current_global.0 == 0 || current_global.1 != global_revision {
                tx.commit()?;
                return Ok(());
            }
            let current_alert: Option<(i64, i64)> = tx
                .query_row(
                    "SELECT revision,enabled FROM metric_alerts WHERE id=?1",
                    [&alert.id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            if current_alert != Some((alert.revision, 1)) {
                tx.commit()?;
                return Ok(());
            }
            let (old_state, loss_registered, previous_last_point): (String, bool, Option<i64>) =
                tx.query_row(
                "SELECT condition_state,loss_registered,last_point_at FROM metric_alert_states WHERE alert_id=?1",
                [&alert.id],
                |row| Ok((row.get(0)?, row.get::<_, i64>(1)? != 0, row.get(2)?)),
            )?;
            let previous_state = state_from_str(&old_state);
            let as_of = parse_timestamp_ms(&reply.as_of).ok_or(rusqlite::Error::InvalidQuery)?;
            let latest_point = reply.last_point_at.as_deref().and_then(parse_timestamp_ms);
            let enabled_at = alert.enabled_at;
            let last_point = latest_point
                .filter(|point| *point >= enabled_at)
                .or_else(|| previous_last_point.filter(|point| *point >= enabled_at));
            let freshness_reference = last_point.unwrap_or(enabled_at);
            let stale = as_of.saturating_sub(freshness_reference) >= NO_DATA_LOSS_MS;
            let windows: BTreeMap<u64, &AlertSeriesWindow> = reply
                .windows
                .iter()
                .map(|window| (window.seconds, window))
                .collect();
            let (next_state, next_loss_registered, loss_at, events) =
                if alert.no_data == "loss" && stale {
                    let loss_at = (!loss_registered)
                        .then_some(freshness_reference.saturating_add(NO_DATA_LOSS_MS));
                    let events = if !loss_registered {
                        vec![(
                            "data_loss",
                            alert_message(
                                &alert,
                                "data_loss",
                                &windows,
                                &reply.as_of,
                                &reply.last_point_at,
                                &public_url,
                            ),
                        )]
                    } else {
                        Vec::new()
                    };
                    (previous_state, true, loss_at, events)
                } else {
                    let evaluated_state = evaluate_conditions(&alert, &windows);
                    let next_state = if evaluated_state == ConditionState::Unknown {
                        previous_state
                    } else {
                        evaluated_state
                    };
                    let mut events = Vec::new();
                    if loss_registered {
                        events.push((
                            "data_recovered",
                            alert_message(
                                &alert,
                                "data_recovered",
                                &windows,
                                &reply.as_of,
                                &reply.last_point_at,
                                &public_url,
                            ),
                        ));
                    }
                    if previous_state != next_state {
                        let kind = match next_state {
                            ConditionState::Firing => Some("firing"),
                            ConditionState::Ok if previous_state == ConditionState::Firing => {
                                Some("recovered")
                            }
                            _ => None,
                        };
                        if let Some(kind) = kind {
                            // A data recovery is a separate event; retain both transitions.
                            events.push((
                                kind,
                                alert_message(
                                    &alert,
                                    kind,
                                    &windows,
                                    &reply.as_of,
                                    &reply.last_point_at,
                                    &public_url,
                                ),
                            ));
                        }
                    }
                    (next_state, false, None, events)
                };

            tx.execute(
                "UPDATE metric_alert_states SET condition_state=?2,loss_registered=?3,
                 loss_at=CASE WHEN ?3=1 THEN COALESCE(loss_at,?4) ELSE NULL END,
                 last_point_at=?5,last_as_of=?6 WHERE alert_id=?1",
                params![
                    alert.id,
                    next_state.as_str(),
                    if next_loss_registered { 1 } else { 0 },
                    loss_at,
                    last_point,
                    as_of,
                ],
            )?;
            for (kind, message) in events {
                enqueue_alert_event(&tx, &alert, kind, &message, now_ms())?;
            }
            tx.commit()?;
            Ok(())
        })
        .await
    }

    async fn record_core_health(
        &self,
        status: &'static str,
        error: Option<&'static str>,
        loss_message: Option<String>,
        public_url: Url,
    ) -> Result<(), StoreError> {
        self.call(move |conn| {
            let tx = conn.transaction()?;
            let (enabled, registered, previous_event): (i64, i64, Option<String>) = tx.query_row(
                "SELECT enabled,core_loss_registered,core_loss_event_id FROM metric_alert_global WHERE singleton=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            if enabled == 0 {
                tx.commit()?;
                return Ok(());
            }
            tx.execute(
                "UPDATE metric_alert_global SET core_status=?1,core_checked_at=?2,core_error=?3 WHERE singleton=1",
                params![status,now_ms(),error],
            )?;
            if status == "available" && registered != 0 {
                let event_id = Uuid::new_v4().to_string();
                let now = now_ms();
                insert_core_event(
                    &tx,
                    &event_id,
                    "core_recovered",
                    &format!("Core снова отвечает. Проверка метрик возобновлена.\n{}", settings_link(&public_url)),
                    now,
                )?;
                let recipients = previous_event
                    .as_deref()
                    .map(|previous| snapshot_event_recipients(&tx, previous))
                    .transpose()?
                    .unwrap_or_default();
                insert_deliveries(&tx, &event_id, &recipients, now)?;
                finish_single_outbox(&tx, &event_id, now)?;
                tx.execute(
                    "UPDATE metric_alert_global SET core_loss_registered=0,core_loss_event_id=NULL WHERE singleton=1",
                    [],
                )?;
            } else if status == "unavailable"
                && registered == 0
                && let Some(message) = loss_message
            {
                let event_id = Uuid::new_v4().to_string();
                let now = now_ms();
                insert_core_event(&tx, &event_id, "core_unavailable", &message, now)?;
                let recipients = current_enabled_recipients(&tx)?;
                insert_deliveries(&tx, &event_id, &recipients, now)?;
                finish_single_outbox(&tx, &event_id, now)?;
                tx.execute(
                    "UPDATE metric_alert_global SET core_loss_registered=1,core_loss_event_id=?1 WHERE singleton=1",
                    [&event_id],
                )?;
            }
            tx.commit()?;
            Ok(())
        })
        .await
    }

    async fn next_alert_delivery(&self) -> Result<Option<PendingDelivery>, StoreError> {
        self.call(|conn| {
            let tx = conn.transaction()?;
            let now = now_ms();
            tx.execute(
                "UPDATE metric_alert_deliveries SET status='expired',last_error='expired'
                 WHERE status='pending' AND event_id IN
                 (SELECT id FROM metric_alert_outbox WHERE expires_at<=?1)",
                [now],
            )?;
            tx.execute(
                "DELETE FROM metric_alert_outbox WHERE finished_at IS NOT NULL AND finished_at<=?1",
                [now - OUTBOX_RETENTION_MS],
            )?;
            finish_outbox(&tx, now)?;
            let global = read_global(&tx)?;
            if !global.enabled || global.send_paused || !global.token_configured || global.throttle_until > now {
                tx.commit()?;
                return Ok(None);
            }
            type DeliveryRow = (String, String, i64, String, Option<String>, Option<String>, String, i64);
            let row: Option<DeliveryRow> = tx.query_row(
                "SELECT d.event_id,d.recipient_id,d.recipient_chat_id,o.message,o.parse_mode,o.reply_markup,
                        g.telegram_token,d.attempts
                 FROM metric_alert_deliveries d
                 JOIN metric_alert_recipients r ON r.id=d.recipient_id AND r.chat_id=d.recipient_chat_id
                 JOIN metric_alert_outbox o ON o.id=d.event_id
                 JOIN metric_alert_global g ON g.singleton=1
                 WHERE d.status='pending' AND d.next_attempt_at<=?1 AND o.expires_at>?1
                   AND ((o.alert_id IS NULL AND o.log_alert_id IS NULL) OR EXISTS(
                       SELECT 1 FROM metric_alerts a WHERE a.id=o.alert_id AND a.enabled=1
                         AND a.revision=o.alert_revision) OR EXISTS(
                       SELECT 1 FROM log_alerts l WHERE l.id=o.log_alert_id AND l.enabled=1
                         AND l.revision=o.alert_revision))
                   AND NOT EXISTS(
                     SELECT 1 FROM metric_alert_deliveries earlier
                     JOIN metric_alert_outbox eo ON eo.id=earlier.event_id
                     WHERE earlier.recipient_id=d.recipient_id
                       AND COALESCE(eo.alert_id,eo.log_alert_id,'')=COALESCE(o.alert_id,o.log_alert_id,'')
                       AND earlier.status IN ('pending','sending')
                       AND (eo.created_at<o.created_at OR (eo.created_at=o.created_at AND eo.id<o.id)))
                 ORDER BY o.created_at,o.id,d.recipient_id LIMIT 1",
                [now],
                |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?)),
            ).optional()?;
            let Some((event_id, recipient_id, chat_id, message, parse_mode, reply_markup, token, attempts)) = row else {
                tx.commit()?;
                return Ok(None);
            };
            let claimed = tx.execute(
                "UPDATE metric_alert_deliveries SET status='sending',attempts=attempts+1
                 WHERE event_id=?1 AND recipient_id=?2 AND status='pending'",
                params![event_id,recipient_id],
            )?;
            tx.commit()?;
            if claimed == 0 {
                return Ok(None);
            }
            Ok(Some(PendingDelivery {
                event_id,
                recipient_id,
                chat_id,
                message,
                parse_mode,
                reply_markup,
                token,
                attempts: attempts as u32 + 1,
            }))
        })
        .await
    }

    async fn finish_alert_delivery(
        &self,
        delivery: PendingDelivery,
        result: Result<(), TelegramFailure>,
    ) -> Result<(), StoreError> {
        self.call(move |conn| {
            let tx = conn.transaction()?;
            let now = now_ms();
            let still_sending: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM metric_alert_deliveries
                 WHERE event_id=?1 AND recipient_id=?2 AND status='sending')",
                params![delivery.event_id,delivery.recipient_id],
                |row| row.get(0),
            )?;
            if !still_sending {
                tx.commit()?;
                return Ok(());
            }
            match result {
                Ok(()) => {
                    tx.execute(
                        "UPDATE metric_alert_deliveries SET status='sent',sent_at=?3,last_error=NULL
                         WHERE event_id=?1 AND recipient_id=?2 AND status='sending'",
                        params![delivery.event_id,delivery.recipient_id,now],
                    )?;
                    tx.execute(
                        "UPDATE metric_alert_recipients SET last_error=NULL WHERE id=?1",
                        [&delivery.recipient_id],
                    )?;
                }
                Err(TelegramFailure::InvalidToken) => {
                    tx.execute(
                        "UPDATE metric_alert_global SET send_paused=1,send_pause_reason='invalid_token',revision=revision+1 WHERE singleton=1",
                        [],
                    )?;
                    tx.execute(
                        "UPDATE metric_alert_deliveries SET status='pending',next_attempt_at=?3,last_error='bot_token_invalid'
                         WHERE event_id=?1 AND recipient_id=?2 AND status='sending'",
                        params![delivery.event_id,delivery.recipient_id,now+60_000],
                    )?;
                }
                Err(TelegramFailure::PermanentRecipient) => {
                    tx.execute(
                        "UPDATE metric_alert_deliveries SET status='permanent_error',last_error='recipient_rejected'
                         WHERE event_id=?1 AND recipient_id=?2 AND status='sending'",
                        params![delivery.event_id,delivery.recipient_id],
                    )?;
                    tx.execute(
                        "UPDATE metric_alert_recipients SET last_error='recipient_rejected' WHERE id=?1",
                        [&delivery.recipient_id],
                    )?;
                }
                Err(TelegramFailure::RateLimited(wait)) => {
                    let until = now.saturating_add(wait.as_millis().min(i64::MAX as u128) as i64);
                    tx.execute(
                        "UPDATE metric_alert_global SET throttle_until=max(throttle_until,?1) WHERE singleton=1",
                        [until],
                    )?;
                    tx.execute(
                        "UPDATE metric_alert_deliveries SET status='pending',next_attempt_at=?3,last_error='rate_limited'
                         WHERE event_id=?1 AND recipient_id=?2 AND status='sending'",
                        params![delivery.event_id,delivery.recipient_id,until],
                    )?;
                }
                Err(TelegramFailure::Transient) => {
                    let delay = retry_delay(delivery.attempts);
                    tx.execute(
                        "UPDATE metric_alert_deliveries SET status='pending',next_attempt_at=?3,last_error='delivery_unavailable'
                         WHERE event_id=?1 AND recipient_id=?2 AND status='sending'",
                        params![delivery.event_id,delivery.recipient_id,now+delay],
                    )?;
                }
            }
            finish_outbox(&tx, now)?;
            tx.commit()?;
            Ok(())
        })
        .await
    }
}

pub(crate) async fn send_pending(app: &AppState) {
    send_pending_bounded(app, ALERT_SEND_PASS_LIMIT, ALERT_SEND_PASS_BUDGET).await;
}

async fn send_pending_bounded(app: &AppState, limit: usize, budget: Duration) {
    let deadline = Instant::now() + budget;
    for _ in 0..limit {
        if app.shutdown.is_cancelled() || Instant::now() >= deadline {
            return;
        }
        let delivery = match app.store.next_alert_delivery().await {
            Ok(Some(delivery)) => delivery,
            _ => return,
        };
        let result = tokio::select! {
            _ = app.shutdown.cancelled() => return,
            result = tokio::time::timeout_at(
                deadline,
                app.telegram.send(
                    &delivery.token,
                    delivery.chat_id,
                    &delivery.message,
                    delivery.parse_mode.as_deref(),
                    delivery.reply_markup.as_deref(),
                ),
            ) => result.unwrap_or(Err(TelegramFailure::Transient)),
        };
        let deadline_reached = Instant::now() >= deadline;
        if app
            .store
            .finish_alert_delivery(delivery, result)
            .await
            .is_err()
        {
            return;
        }
        if deadline_reached {
            return;
        }
    }
}

fn enqueue_alert_event(
    tx: &rusqlite::Transaction<'_>,
    alert: &AlertDefinition,
    kind: &str,
    message: &RenderedTelegramMessage,
    now: i64,
) -> rusqlite::Result<()> {
    let event_id = Uuid::new_v4().to_string();
    let previous: Option<i64> = tx.query_row(
        "SELECT max(created_at) FROM metric_alert_outbox WHERE alert_id=?1",
        [&alert.id],
        |row| row.get(0),
    )?;
    let created_at = previous.map_or(now, |previous| now.max(previous.saturating_add(1)));
    tx.execute(
        "INSERT INTO metric_alert_outbox(id,alert_id,alert_revision,event_kind,message,created_at,expires_at,parse_mode,reply_markup)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![event_id,alert.id,alert.revision,kind,message.text,created_at,created_at+OUTBOX_TTL_MS,
            message.parse_mode,message.reply_markup],
    )?;
    let recipients = {
        let mut statement = tx.prepare(
            "SELECT DISTINCT r.id,r.name,r.chat_id FROM metric_alert_groups_used g
             JOIN metric_alert_group_recipients gr ON gr.group_id=g.group_id
             JOIN metric_alert_recipients r ON r.id=gr.recipient_id
             WHERE g.alert_id=?1 ORDER BY r.id",
        )?;
        statement
            .query_map([&alert.id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    insert_deliveries(tx, &event_id, &recipients, now)?;
    finish_single_outbox(tx, &event_id, now)
}

fn insert_core_event(
    tx: &rusqlite::Transaction<'_>,
    event_id: &str,
    kind: &str,
    message: &str,
    now: i64,
) -> rusqlite::Result<()> {
    let previous: Option<i64> = tx.query_row(
        "SELECT max(created_at) FROM metric_alert_outbox WHERE alert_id IS NULL AND log_alert_id IS NULL",
        [],
        |row| row.get(0),
    )?;
    let created_at = previous.map_or(now, |previous| now.max(previous.saturating_add(1)));
    tx.execute(
        "INSERT INTO metric_alert_outbox(id,alert_id,alert_revision,event_kind,message,created_at,expires_at)
         VALUES(?1,NULL,NULL,?2,?3,?4,?5)",
        params![event_id,kind,message,created_at,created_at+OUTBOX_TTL_MS],
    )?;
    Ok(())
}

fn current_enabled_recipients(
    tx: &rusqlite::Transaction<'_>,
) -> rusqlite::Result<Vec<(String, String, i64)>> {
    let mut statement = tx.prepare(
        "SELECT DISTINCT r.id,r.name,r.chat_id FROM metric_alerts a
         JOIN metric_alert_groups_used g ON g.alert_id=a.id
         JOIN metric_alert_group_recipients gr ON gr.group_id=g.group_id
         JOIN metric_alert_recipients r ON r.id=gr.recipient_id
         WHERE a.enabled=1
         UNION
         SELECT r.id,r.name,r.chat_id FROM log_alerts a
         JOIN log_alert_groups_used g ON g.alert_id=a.id
         JOIN metric_alert_group_recipients gr ON gr.group_id=g.group_id
         JOIN metric_alert_recipients r ON r.id=gr.recipient_id
         WHERE a.enabled=1 ORDER BY 1",
    )?;
    statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?
        .collect()
}

fn snapshot_event_recipients(
    tx: &rusqlite::Transaction<'_>,
    event_id: &str,
) -> rusqlite::Result<Vec<(String, String, i64)>> {
    let mut statement = tx.prepare(
        "SELECT d.recipient_id,d.recipient_name,d.recipient_chat_id
         FROM metric_alert_deliveries d
         JOIN metric_alert_recipients r ON r.id=d.recipient_id AND r.chat_id=d.recipient_chat_id
         WHERE d.event_id=?1 AND d.status!='cancelled' ORDER BY d.recipient_id",
    )?;
    statement
        .query_map([event_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?
        .collect()
}

pub(crate) fn insert_deliveries(
    tx: &rusqlite::Transaction<'_>,
    event_id: &str,
    recipients: &[(String, String, i64)],
    now: i64,
) -> rusqlite::Result<()> {
    for (id, name, chat_id) in recipients {
        tx.execute(
            "INSERT INTO metric_alert_deliveries(event_id,recipient_id,recipient_name,recipient_chat_id,
             status,next_attempt_at) VALUES(?1,?2,?3,?4,'pending',?5)",
            params![event_id,id,name,chat_id,now],
        )?;
    }
    Ok(())
}

pub(crate) fn finish_single_outbox(
    tx: &rusqlite::Transaction<'_>,
    event_id: &str,
    now: i64,
) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE metric_alert_outbox SET finished_at=?2 WHERE id=?1
         AND NOT EXISTS(SELECT 1 FROM metric_alert_deliveries WHERE event_id=?1 AND status IN ('pending','sending'))",
        params![event_id,now],
    )?;
    Ok(())
}

pub(crate) fn parse_timestamp_ms(value: &str) -> Option<i64> {
    OffsetDateTime::parse(value, &Rfc3339)
        .ok()
        .map(|time| (time.unix_timestamp_nanos() / 1_000_000) as i64)
}

fn state_from_str(value: &str) -> ConditionState {
    match value {
        "ok" => ConditionState::Ok,
        "firing" => ConditionState::Firing,
        _ => ConditionState::Unknown,
    }
}

fn evaluate_conditions(
    alert: &AlertDefinition,
    windows: &BTreeMap<u64, &AlertSeriesWindow>,
) -> ConditionState {
    let values: Vec<Option<bool>> = alert
        .triggers
        .iter()
        .map(|trigger| {
            let window = windows.get(&trigger.window_seconds)?;
            let value = if window.count == 0 {
                match trigger.aggregate.as_str() {
                    "sum" | "count" if alert.no_data == "normal" => Some(0.0),
                    _ if alert.no_data == "loss" => return None,
                    // An empty normal window is a known false condition for aggregates that
                    // require observations; it is distinct from a missing query result.
                    _ => return Some(false),
                }
            } else {
                match trigger.aggregate.as_str() {
                    "avg" => window.avg,
                    "min" => window.min,
                    "max" => window.max,
                    "sum" => Some(window.sum),
                    "count" => Some(window.count as f64),
                    "increase" => match window.increase {
                        Some(value) => Some(value),
                        None if alert.no_data == "normal" => return Some(false),
                        None => return None,
                    },
                    _ => None,
                }
            };
            let value = value?;
            Some(if trigger.comparator == ">" {
                value > trigger.threshold
            } else {
                value < trigger.threshold
            })
        })
        .collect();
    if alert.combine == "and" {
        if values.contains(&Some(false)) {
            ConditionState::Ok
        } else if values.iter().all(|value| *value == Some(true)) {
            ConditionState::Firing
        } else {
            ConditionState::Unknown
        }
    } else if values.contains(&Some(true)) {
        ConditionState::Firing
    } else if values.iter().all(|value| *value == Some(false)) {
        ConditionState::Ok
    } else {
        ConditionState::Unknown
    }
}

#[derive(Debug, Clone)]
pub(crate) struct RenderedTelegramMessage {
    pub text: String,
    pub parse_mode: Option<String>,
    pub reply_markup: Option<String>,
}

fn expand_message_template(template: &str, values: &BTreeMap<&'static str, String>) -> String {
    let mut output = String::with_capacity(template.len());
    let mut cursor = 0;
    while let Some(relative_start) = template[cursor..].find('%') {
        let start = cursor + relative_start;
        output.push_str(&template[cursor..start]);
        let token_start = start + 1;
        let Some(relative_end) = template[token_start..].find('%') else {
            output.push_str(&template[start..]);
            return output;
        };
        let end = token_start + relative_end;
        let token = &template[token_start..end];
        if let Some(value) = values.get(token) {
            output.push_str(value);
        } else {
            output.push_str(&template[start..=end]);
        }
        cursor = end + 1;
    }
    output.push_str(&template[cursor..]);
    output
}

fn escape_html(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

fn append_limited_html_text(output: &mut String, text: &str, remaining: &mut usize) {
    if *remaining == 0 {
        return;
    }
    let length = text.chars().count();
    if length <= *remaining {
        output.push_str(&escape_html(text));
        *remaining -= length;
    } else {
        let take = remaining.saturating_sub(1);
        output.push_str(&escape_html(&text.chars().take(take).collect::<String>()));
        output.push('…');
        *remaining = 0;
    }
}

fn append_html_block(
    output: &mut String,
    heading: Option<&str>,
    text: &str,
    remaining: &mut usize,
) {
    if *remaining == 0 {
        return;
    }
    if let Some(heading) = heading {
        output.push_str("<b>");
        append_limited_html_text(output, heading, remaining);
        output.push_str("</b>");
        if *remaining > 0 {
            output.push('\n');
            *remaining -= 1;
        }
    }
    append_limited_html_text(output, text, remaining);
    if *remaining > 0 {
        output.push('\n');
        *remaining -= 1;
    }
}

fn telegram_button(link: &str) -> Option<String> {
    let url = Url::parse(link).ok()?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || telegram_button_host_is_local_or_private(url.host_str()?)
    {
        return None;
    }
    serde_json::to_string(&json!({
        "inline_keyboard": [[{"text": "Открыть в Dock", "url": url.as_str()}]]
    }))
    .ok()
}

fn telegram_button_host_is_local_or_private(host: &str) -> bool {
    let host = host.trim_end_matches('.');
    if host.eq_ignore_ascii_case("localhost")
        || host.to_ascii_lowercase().ends_with(".localhost")
        || host.to_ascii_lowercase().ends_with(".local")
    {
        return true;
    }
    let host = host.trim_start_matches('[').trim_end_matches(']');
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(address)) => {
            let octets = address.octets();
            address.is_private()
                || address.is_loopback()
                || address.is_link_local()
                || address.is_unspecified()
                || address.is_broadcast()
                || address.is_multicast()
                || (octets[0] == 100 && octets[1] & 0b1100_0000 == 0b0100_0000)
        }
        Ok(IpAddr::V6(address)) => {
            address.is_loopback()
                || address.is_unspecified()
                || address.is_unique_local()
                || address.is_unicast_link_local()
                || address.is_multicast()
                || address.to_ipv4_mapped().is_some_and(|ipv4| {
                    let octets = ipv4.octets();
                    ipv4.is_private()
                        || ipv4.is_loopback()
                        || ipv4.is_link_local()
                        || ipv4.is_unspecified()
                        || ipv4.is_broadcast()
                        || ipv4.is_multicast()
                        || (octets[0] == 100 && octets[1] & 0b1100_0000 == 0b0100_0000)
                })
        }
        Err(_) => false,
    }
}

pub(crate) fn render_telegram_message(
    status: &str,
    emoji: &str,
    template: Option<&str>,
    placeholders: &BTreeMap<&'static str, String>,
    blocks: Vec<(&'static str, Vec<String>)>,
    link: &str,
) -> RenderedTelegramMessage {
    const MESSAGE_BUDGET: usize = 4000;
    let mut text = String::new();
    let mut remaining = MESSAGE_BUDGET;
    text.push_str("<b>");
    append_limited_html_text(&mut text, &format!("{emoji} {status}"), &mut remaining);
    text.push_str("</b>");
    if remaining > 0 {
        text.push('\n');
        remaining -= 1;
    }
    if let Some(template) = template.filter(|value| !value.trim().is_empty()) {
        append_html_block(
            &mut text,
            None,
            &expand_message_template(template, placeholders),
            &mut remaining,
        );
    }
    for (heading, lines) in blocks {
        if !lines.is_empty() {
            append_html_block(&mut text, Some(heading), &lines.join("\n"), &mut remaining);
        }
    }
    let reply_markup = telegram_button(link);
    if reply_markup.is_none() {
        append_html_block(&mut text, Some("Открыть в Dock"), link, &mut remaining);
    }
    RenderedTelegramMessage {
        text,
        parse_mode: Some("HTML".into()),
        reply_markup,
    }
}

pub async fn preview_message(
    State(app): State<AppState>,
    _user: crate::session::AuthUser,
    Json(input): Json<MessagePreviewInput>,
) -> Result<Json<Value>, ApiError> {
    let rendered = render_preview_message(&app, &input)?;
    Ok(Json(
        json!({"html": rendered.text, "button": rendered.reply_markup.is_some()}),
    ))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestMessageInput {
    pub message: MessagePreviewInput,
    pub group_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct TestMessageFailure {
    pub recipient: String,
    pub error: &'static str,
}

/// Тестовое сообщение алерта: тот же рендер, что и предпросмотр, отправленный сразу
/// получателям выбранных групп, без очереди доставки и без изменения состояния алерта.
pub async fn send_test_message(
    State(app): State<AppState>,
    _user: crate::session::AuthUser,
    Json(input): Json<TestMessageInput>,
) -> Result<Json<Value>, ApiError> {
    if input.group_ids.is_empty() || input.group_ids.len() > 100 {
        return Err(ApiError::BadRequest("no_recipients"));
    }
    let rendered = render_preview_message(&app, &input.message)?;
    let token = app
        .store
        .alert_token()
        .await?
        .ok_or(ApiError::BadRequest("bot_token_required"))?;
    let settings = app.store.alert_settings().await?;
    let mut recipients: Vec<&Recipient> = Vec::new();
    for group in settings
        .groups
        .iter()
        .filter(|group| input.group_ids.contains(&group.id))
    {
        for id in &group.recipient_ids {
            if let Some(recipient) = settings.recipients.iter().find(|item| &item.id == id)
                && !recipients
                    .iter()
                    .any(|item| item.chat_id == recipient.chat_id)
            {
                recipients.push(recipient);
            }
        }
    }
    if recipients.is_empty() {
        return Err(ApiError::BadRequest("no_recipients"));
    }
    let text = if rendered.parse_mode.as_deref() == Some("HTML") {
        format!(
            "🧪 <b>Тестовое сообщение</b>
{}",
            rendered.text
        )
    } else {
        format!(
            "🧪 Тестовое сообщение
{}",
            rendered.text
        )
    };
    let mut sent = 0_u32;
    let mut failed = Vec::new();
    for recipient in recipients {
        match app
            .telegram
            .send(
                &token,
                recipient.chat_id,
                &text,
                rendered.parse_mode.as_deref(),
                rendered.reply_markup.as_deref(),
            )
            .await
        {
            Ok(()) => sent += 1,
            Err(TelegramFailure::InvalidToken) => {
                return Err(ApiError::BadRequest("invalid_bot_token"));
            }
            Err(failure) => failed.push(TestMessageFailure {
                recipient: recipient.name.clone(),
                error: match failure {
                    TelegramFailure::PermanentRecipient => "recipient_unavailable",
                    TelegramFailure::RateLimited(_) => "telegram_rate_limited",
                    _ => "telegram_unavailable",
                },
            }),
        }
    }
    Ok(Json(json!({"sent": sent, "failed": failed})))
}

fn render_preview_message(
    app: &AppState,
    input: &MessagePreviewInput,
) -> Result<RenderedTelegramMessage, ApiError> {
    let allowed = match input.kind.as_str() {
        "metric" => METRIC_MESSAGE_FIELDS,
        "logs" => METRIC_MESSAGE_FIELDS,
        _ => return Err(ApiError::Invalid("kind must be metric or logs".into())),
    };
    if input.name.trim().is_empty()
        || input.name.chars().count() > 2000
        || input
            .message_template
            .as_ref()
            .is_some_and(|text| !valid_message_text(text, 2000))
    {
        return Err(ApiError::Invalid("invalid preview text".into()));
    }
    validate_message_fields(&input.message_fields, allowed)
        .map_err(|_| ApiError::Invalid("invalid message fields".into()))?;
    let field = |name: &str| input.message_fields.iter().any(|item| item == name);
    let title = input.title.as_deref().unwrap_or(&input.name);
    let description = input.description.as_deref().unwrap_or("");
    let status_value = input.status.as_deref().unwrap_or("firing");
    let (emoji, status) = match status_value {
        "recovered" => ("🟢", "Алерт восстановился"),
        "data_loss" => ("⚠️", "Нет данных"),
        "error" => ("⚠️", "Ошибка проверки"),
        _ => (
            "🔴",
            if input.kind == "logs" {
                "Алерт на логи сработал"
            } else {
                "Алерт метрики сработал"
            },
        ),
    };
    let mut source = Vec::new();
    if field("name") {
        source.push(format!("Имя: {}", input.name));
    }
    if field("title") {
        source.push(format!("Название: {title}"));
    }
    if field("description") && !description.is_empty() {
        source.push(format!("Описание: {description}"));
    }
    if field("server")
        && let Some(server) = &input.server
    {
        source.push(format!("Сервер: {server}"));
    }
    if field("service")
        && let Some(service) = &input.service
    {
        source.push(format!("Сервис: {service}"));
    }
    if field("origin")
        && let Some(origin) = &input.origin
    {
        source.push(format!("Источник: {origin}"));
    }
    if field("environment") {
        source.push(format!(
            "Окружение: {}",
            input.environment.as_deref().unwrap_or("все")
        ));
    }
    if field("query")
        && let Some(query) = &input.query
    {
        source.push(format!("Запрос: {query}"));
    }
    if field("status") {
        source.push(format!("Статус: {status_value}"));
    }
    if field("samples") && input.kind == "logs" {
        source.push(
            "2026-11-16T09:30:00Z srv-eu-12 / billing error: Database connection refused".into(),
        );
    }
    let mut condition = Vec::new();
    if field("condition")
        && let Some(value) = &input.condition
    {
        condition.push(format!("Условие: {value}"));
    }
    if field("value") {
        if let Some(value) = &input.value {
            condition.push(format!("Значение: {value}"));
        } else if let Some(count) = input.count {
            condition.push(format!("Новых совпадений: {count}"));
        }
    }
    if field("count")
        && let Some(count) = input.count
    {
        condition.push(format!("Новых совпадений: {count}"));
    }
    let mut time = Vec::new();
    if field("time") {
        time.push(format!(
            "Время Core: {}",
            input.time.as_deref().unwrap_or("2026-11-16T09:30:00Z")
        ));
    }
    if field("last_point") {
        time.push(format!(
            "Последняя точка: {}",
            input.last_point.as_deref().unwrap_or("нет данных")
        ));
    }
    let mut placeholders = BTreeMap::new();
    placeholders.insert(
        "value",
        input
            .value
            .clone()
            .unwrap_or_else(|| input.count.unwrap_or_default().to_string()),
    );
    placeholders.insert("name", input.name.clone());
    placeholders.insert("title", title.to_owned());
    placeholders.insert("description", description.to_owned());
    placeholders.insert("service", input.service.clone().unwrap_or_default());
    placeholders.insert("server", input.server.clone().unwrap_or_default());
    placeholders.insert(
        "environment",
        input.environment.clone().unwrap_or_else(|| "все".into()),
    );
    placeholders.insert("condition", input.condition.clone().unwrap_or_default());
    placeholders.insert(
        "time",
        input
            .time
            .clone()
            .unwrap_or_else(|| "2026-11-16T09:30:00Z".into()),
    );
    placeholders.insert(
        "last_point",
        input
            .last_point
            .clone()
            .unwrap_or_else(|| "нет данных".into()),
    );
    placeholders.insert("status", status_value.to_owned());
    placeholders.insert("count", input.count.unwrap_or_default().to_string());
    placeholders.insert("query", input.query.clone().unwrap_or_default());
    let link = if input.kind == "logs" {
        let mut url = app
            .config
            .public_url
            .join("logs")
            .unwrap_or_else(|_| app.config.public_url.clone());
        if let Some(query) = &input.query {
            url.query_pairs_mut()
                .append_pair("q", query)
                .append_pair("range", "1h");
        }
        url.to_string()
    } else {
        let mut url = app
            .config
            .public_url
            .join("metrics/detail")
            .unwrap_or_else(|_| app.config.public_url.clone());
        if let Some(server) = &input.server {
            url.query_pairs_mut().append_pair("server", server);
        }
        if let Some(service) = &input.service {
            url.query_pairs_mut().append_pair("service", service);
        }
        url.query_pairs_mut().append_pair("name", &input.name);
        if let Some(origin) = &input.origin {
            url.query_pairs_mut().append_pair("origin", origin);
        }
        url.to_string()
    };
    let rendered = render_telegram_message(
        status,
        emoji,
        input.message_template.as_deref(),
        &placeholders,
        vec![
            (
                if input.kind == "logs" {
                    "Источник"
                } else {
                    "Метрика / источник"
                },
                source,
            ),
            ("Условие / значение", condition),
            ("Время", time),
        ],
        &link,
    );
    Ok(rendered)
}

fn window_text(seconds: u64) -> String {
    if seconds.is_multiple_of(86_400) {
        format!("{} дн", seconds / 86_400)
    } else if seconds.is_multiple_of(3_600) {
        format!("{} ч", seconds / 3_600)
    } else if seconds.is_multiple_of(60) {
        format!("{} мин", seconds / 60)
    } else {
        format!("{seconds} сек")
    }
}

fn alert_message(
    alert: &AlertDefinition,
    kind: &str,
    windows: &BTreeMap<u64, &AlertSeriesWindow>,
    as_of: &str,
    last_point: &Option<String>,
    public_url: &Url,
) -> RenderedTelegramMessage {
    let (emoji, status) = match kind {
        "firing" => ("🔴", "Алерт метрики сработал"),
        "recovered" => ("🟢", "Алерт метрики восстановился"),
        "data_loss" => ("⚠️", "Нет новых точек метрики"),
        "data_recovered" => ("🟢", "Точки метрики снова поступают"),
        _ => ("⚠️", "Ошибка проверки алерта метрики"),
    };
    let selected = |field: &str| alert.message_fields.iter().any(|value| value == field);
    let title = alert
        .title
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(&alert.key.name);
    let mut source = Vec::new();
    if selected("name") {
        source.push(format!("Имя: {}", alert.key.name));
    }
    if selected("title") {
        source.push(format!("Название: {title}"));
    }
    if selected("description")
        && let Some(description) = alert
            .description
            .as_deref()
            .filter(|value| !value.is_empty())
    {
        source.push(format!("Описание: {description}"));
    }
    if selected("server") {
        source.push(format!("Сервер: {}", alert.key.server));
    }
    if selected("service") {
        source.push(format!("Сервис: {}", alert.key.service));
    }
    if selected("origin") {
        source.push(format!("Источник: {}", alert.key.origin));
    }
    if selected("node") {
        match alert.node_scope {
            Scope::All => {}
            Scope::Relay { id } => source.push(format!("Узел: relay {id}")),
            Scope::Agent { id } => source.push(format!("Узел: agent {id}")),
        }
    }
    if selected("environment") {
        source.push(format!(
            "Окружение: {}",
            alert.environment.as_deref().unwrap_or("все")
        ));
    }
    if selected("status") {
        source.push(format!("Статус: {status}"));
    }
    let mut conditions = Vec::new();
    let mut values = Vec::new();
    for trigger in &alert.triggers {
        let aggregate = match trigger.aggregate.as_str() {
            "avg" => "среднее",
            "min" => "минимум",
            "max" => "максимум",
            "sum" => "сумма",
            "count" => "число точек",
            "increase" => "прирост",
            _ => trigger.aggregate.as_str(),
        };
        let comparator = match trigger.comparator.as_str() {
            ">" => "строго больше",
            ">=" => "не меньше",
            "<" => "строго меньше",
            "<=" => "не больше",
            _ => trigger.comparator.as_str(),
        };
        let value = windows.get(&trigger.window_seconds).and_then(|window| {
            match trigger.aggregate.as_str() {
                "avg" => window.avg,
                "min" => window.min,
                "max" => window.max,
                "sum" => Some(window.sum),
                "count" => Some(window.count as f64),
                "increase" => window.increase,
                _ => None,
            }
        });
        conditions.push(format!(
            "{} за {} {} {} (значение: {})",
            aggregate,
            window_text(trigger.window_seconds),
            comparator,
            trigger.threshold,
            value.map_or_else(|| String::from("нет значения"), |value| value.to_string()),
        ));
        values.push(value.map_or_else(|| String::from("нет значения"), |value| value.to_string()));
    }
    let condition = format!(
        "{}: {}",
        if alert.combine == "and" {
            "Все условия"
        } else {
            "Любое условие"
        },
        conditions.join("; "),
    );
    let mut condition_lines = Vec::new();
    if selected("condition") {
        condition_lines.push(condition.clone());
    }
    if selected("value") {
        condition_lines.push(format!("Значение: {}", values.join("; ")));
    }
    if selected("last_point") {
        condition_lines.push(format!(
            "Последняя точка: {}",
            last_point.as_deref().unwrap_or("нет данных")
        ));
    }
    let mut time_lines = Vec::new();
    if selected("time") {
        time_lines.push(format!("Время Core: {as_of}"));
    }
    let title_value = alert
        .title
        .clone()
        .unwrap_or_else(|| alert.key.name.clone());
    let description = alert.description.clone().unwrap_or_default();
    let last_point_value = last_point.as_deref().unwrap_or("нет данных").to_owned();
    let mut placeholders = BTreeMap::new();
    placeholders.insert("value", values.join("; "));
    placeholders.insert("name", alert.key.name.clone());
    placeholders.insert("title", title_value);
    placeholders.insert("description", description);
    placeholders.insert("service", alert.key.service.clone());
    placeholders.insert("server", alert.key.server.clone());
    placeholders.insert(
        "environment",
        alert.environment.clone().unwrap_or_else(|| "все".into()),
    );
    placeholders.insert("condition", condition);
    placeholders.insert("time", as_of.to_owned());
    placeholders.insert("last_point", last_point_value);
    placeholders.insert("status", status.to_owned());
    render_telegram_message(
        status,
        emoji,
        alert.message_template.as_deref(),
        &placeholders,
        vec![
            ("Метрика / источник", source),
            ("Условие / значение", condition_lines),
            ("Время", time_lines),
        ],
        &metric_link(public_url, alert),
    )
}

fn metric_link(public_url: &Url, alert: &AlertDefinition) -> String {
    let mut url = public_url
        .join("metrics/detail")
        .unwrap_or_else(|_| public_url.clone());
    url.query_pairs_mut()
        .append_pair("server", &alert.key.server)
        .append_pair("service", &alert.key.service)
        .append_pair("name", &alert.key.name)
        .append_pair("origin", &alert.key.origin);
    url.to_string()
}

pub(crate) fn settings_link(public_url: &Url) -> String {
    public_url
        .join("alerts?tab=telegram")
        .map_or_else(|_| "/alerts?tab=telegram".into(), |url| url.to_string())
}

fn retry_delay(attempt: u32) -> i64 {
    (30_000_i64.saturating_mul(1_i64 << attempt.saturating_sub(1).min(7))).min(3_600_000)
}

fn telegram_verification_api_error(error: TelegramFailure) -> ApiError {
    match error {
        TelegramFailure::InvalidToken => ApiError::BadRequest("invalid_bot_token"),
        TelegramFailure::RateLimited(wait) => ApiError::TelegramRateLimited(wait),
        _ => ApiError::TelegramUnavailable,
    }
}

impl TelegramClient {
    pub fn new() -> Self {
        Self::with_base(Url::parse(TELEGRAM_API).expect("static Telegram URL"))
    }

    fn with_base(base: Url) -> Self {
        Self {
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(10))
                .build()
                .expect("Telegram HTTP client"),
            base,
        }
    }

    fn endpoint(&self, token: &str, method: &str) -> Result<Url, TelegramFailure> {
        if !valid_token(token) {
            return Err(TelegramFailure::InvalidToken);
        }
        let mut url = self.base.clone();
        url.path_segments_mut()
            .map_err(|_| TelegramFailure::Transient)?
            .pop_if_empty()
            .push(&format!("bot{token}"))
            .push(method);
        Ok(url)
    }

    async fn verify_token(&self, token: &str) -> Result<(), TelegramFailure> {
        let url = self.endpoint(token, "getMe")?;
        let response = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|_| TelegramFailure::Transient)?;
        let status = response.status();
        let body = response
            .bytes()
            .await
            .map_err(|_| TelegramFailure::Transient)?;
        let reply: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        let ok = reply["ok"].as_bool();
        let code = reply["error_code"].as_u64();
        if status.is_success() && ok == Some(true) {
            Ok(())
        } else if status == StatusCode::UNAUTHORIZED || code == Some(401) {
            Err(TelegramFailure::InvalidToken)
        } else if status == StatusCode::TOO_MANY_REQUESTS || code == Some(429) {
            Err(telegram_rate_limit(&reply))
        } else {
            Err(TelegramFailure::Transient)
        }
    }

    async fn send(
        &self,
        token: &str,
        chat_id: i64,
        text: &str,
        parse_mode: Option<&str>,
        reply_markup: Option<&str>,
    ) -> Result<(), TelegramFailure> {
        let url = self.endpoint(token, "sendMessage")?;
        let mut text = text.to_owned();
        let mut parse_mode = parse_mode.map(str::to_owned);
        let mut reply_markup: Option<Value> = match reply_markup {
            Some(markup) => {
                Some(serde_json::from_str(markup).map_err(|_| TelegramFailure::Transient)?)
            }
            None => None,
        };

        loop {
            let mut payload = json!({"chat_id": chat_id, "text": text});
            let payload = payload.as_object_mut().ok_or(TelegramFailure::Transient)?;
            if let Some(parse_mode) = &parse_mode {
                payload.insert("parse_mode".into(), Value::String(parse_mode.clone()));
            }
            if let Some(reply_markup) = &reply_markup {
                payload.insert("reply_markup".into(), reply_markup.clone());
            }
            let response = self
                .http
                .post(url.clone())
                .json(&Value::Object(payload.clone()))
                .send()
                .await
                .map_err(|_| TelegramFailure::Transient)?;
            let status = response.status();
            let body = response
                .bytes()
                .await
                .map_err(|_| TelegramFailure::Transient)?;
            let reply: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            if status.is_success() && reply["ok"].as_bool() == Some(true) {
                return Ok(());
            }
            if status == StatusCode::UNAUTHORIZED || reply["error_code"] == 401 {
                return Err(TelegramFailure::InvalidToken);
            }
            if status == StatusCode::TOO_MANY_REQUESTS || reply["error_code"] == 429 {
                return Err(telegram_rate_limit(&reply));
            }

            let description = reply["description"].as_str().unwrap_or_default();
            if status == StatusCode::FORBIDDEN
                || reply["error_code"] == 403
                || ((status.is_client_error() || reply["error_code"] == 400)
                    && telegram_description_is_recipient_error(description))
            {
                return Err(TelegramFailure::PermanentRecipient);
            }
            if status != StatusCode::BAD_REQUEST {
                return Err(TelegramFailure::Transient);
            }
            if reply_markup.is_some() && telegram_description_is_button_error(description) {
                if let Some(link) = reply_markup.as_ref().and_then(telegram_markup_url) {
                    append_telegram_text_link(&mut text, link, parse_mode.as_deref());
                }
                reply_markup = None;
                continue;
            }
            if parse_mode.is_some() && telegram_description_is_parse_error(description) {
                text = telegram_plain_text(&text);
                parse_mode = None;
                continue;
            }
            return Err(TelegramFailure::Transient);
        }
    }
}

fn telegram_markup_url(markup: &Value) -> Option<&str> {
    for row in markup["inline_keyboard"].as_array()? {
        for button in row.as_array()? {
            if let Some(url) = button["url"].as_str() {
                return Some(url);
            }
        }
    }
    None
}

fn append_telegram_text_link(text: &mut String, link: &str, parse_mode: Option<&str>) {
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    if parse_mode == Some("HTML") {
        text.push_str("<b>Открыть в Dock:</b> ");
        text.push_str(&escape_html(link));
    } else {
        text.push_str("Открыть в Dock: ");
        text.push_str(link);
    }
}

fn telegram_plain_text(html: &str) -> String {
    html.replace("<b>", "")
        .replace("</b>", "")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&amp;", "&")
}

fn telegram_description_is_button_error(description: &str) -> bool {
    let description = description.to_ascii_lowercase();
    description.contains("button") || description.contains("wrong http url")
}

fn telegram_description_is_parse_error(description: &str) -> bool {
    let description = description.to_ascii_lowercase();
    description.contains("can't parse entities") || description.contains("can’t parse entities")
}

fn telegram_description_is_recipient_error(description: &str) -> bool {
    let description = description.to_ascii_lowercase();
    [
        "chat not found",
        "bot was blocked",
        "user is deactivated",
        "user deactivated",
        "bot was kicked",
        "bot is not a member",
        "not enough rights",
        "have no rights",
        "forbidden",
    ]
    .iter()
    .any(|needle| description.contains(*needle))
}

impl Default for TelegramClient {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug)]
enum TelegramFailure {
    InvalidToken,
    PermanentRecipient,
    RateLimited(Duration),
    Transient,
}

fn telegram_rate_limit(reply: &Value) -> TelegramFailure {
    let seconds = reply["parameters"]["retry_after"]
        .as_u64()
        .unwrap_or(30)
        .clamp(1, 86_400);
    TelegramFailure::RateLimited(Duration::from_secs(seconds))
}

fn valid_token(token: &str) -> bool {
    let Some((bot_id, secret)) = token.split_once(':') else {
        return false;
    };
    (20..=128).contains(&token.len())
        && !bot_id.is_empty()
        && bot_id.bytes().all(|byte| byte.is_ascii_digit())
        && (20..=128).contains(&secret.len())
        && secret
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        body::Body,
        extract::{Path, State},
        http::Request,
        routing::{get, post},
    };
    use std::{
        collections::VecDeque,
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
    };
    use tokio::net::TcpListener;
    use tower::ServiceExt;

    const BOT_TOKEN: &str = "123456:abcdefghijklmnopqrstuvwxyz0123456789";

    fn test_key() -> MetricKey {
        MetricKey {
            server: "server-a".into(),
            service: "api".into(),
            name: "requests".into(),
            origin: "counter".into(),
        }
    }

    #[test]
    fn control_characters_cannot_forge_notification_lines() {
        assert!(validate_key(&test_key()).is_ok());
        for forged in [
            MetricKey {
                name: "requests\nУсловие: подделка".into(),
                ..test_key()
            },
            MetricKey {
                service: "api\r".into(),
                ..test_key()
            },
            MetricKey {
                server: "server\u{7}".into(),
                ..test_key()
            },
        ] {
            assert!(matches!(
                validate_key(&forged),
                Err(AlertStoreError::Invalid(_))
            ));
        }
        let mut definition = input(test_key(), "normal", vec![]);
        definition.environment = Some("prod\nПоследняя точка: 0".into());
        assert!(matches!(
            validate_definition(&definition),
            Err(AlertStoreError::Invalid("invalid environment"))
        ));
    }

    fn input(key: MetricKey, no_data: &str, group_ids: Vec<String>) -> DefinitionInput {
        DefinitionInput {
            key,
            node_scope: Scope::All,
            enabled: true,
            environment: None,
            no_data: no_data.into(),
            combine: "and".into(),
            group_ids,
            triggers: vec![TriggerInput {
                window_seconds: 60,
                aggregate: "count".into(),
                comparator: ">".into(),
                threshold: 1.0,
            }],
            title: None,
            description: None,
            message_template: None,
            message_fields: None,
        }
    }

    #[test]
    fn telegram_metric_message_escapes_html_expands_placeholders_and_adds_button() {
        let mut alert = condition_alert("and", "normal", vec![trigger(300, "avg")]);
        alert.title = Some("CPU < 80%".into());
        alert.description = Some("disk & network".into());
        alert.message_template = Some("%title%: %value%; %unknown% <raw>".into());
        alert.message_fields = vec![
            "name",
            "title",
            "description",
            "server",
            "service",
            "condition",
            "value",
            "time",
            "last_point",
            "status",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        let window = AlertSeriesWindow {
            seconds: 300,
            count: 2,
            sum: 10.0,
            avg: Some(5.0),
            min: Some(4.0),
            max: Some(6.0),
            increase: Some(2.0),
        };
        let windows = BTreeMap::from([(300, &window)]);
        let rendered = alert_message(
            &alert,
            "firing",
            &windows,
            "2026-11-16T09:30:00Z",
            &Some("2026-11-16T09:29:00Z".into()),
            &Url::parse("https://dock.example/").unwrap(),
        );

        assert_eq!(rendered.parse_mode.as_deref(), Some("HTML"));
        assert!(rendered.text.contains("<b>🔴 Алерт метрики сработал</b>"));
        assert!(rendered.text.contains("CPU &lt; 80%"));
        assert!(rendered.text.contains("disk &amp; network"));
        assert!(
            rendered
                .text
                .contains("Все условия: среднее за 5 мин строго больше 1 (значение: 5)")
        );
        assert!(rendered.text.contains("5"));
        assert!(rendered.text.contains("%unknown%"));
        assert!(rendered.text.contains("&lt;raw&gt;"));
        let markup: Value =
            serde_json::from_str(rendered.reply_markup.as_deref().unwrap()).unwrap();
        assert_eq!(markup["inline_keyboard"][0][0]["text"], "Открыть в Dock");
        assert!(
            markup["inline_keyboard"][0][0]["url"]
                .as_str()
                .unwrap()
                .starts_with("https://dock.example/metrics/detail?")
        );
    }

    #[test]
    fn legacy_metric_rule_uses_default_fields_and_html_button() {
        let alert = condition_alert("or", "normal", vec![trigger(300, "sum")]);
        let rendered = alert_message(
            &alert,
            "recovered",
            &BTreeMap::new(),
            "2026-11-16T09:30:00Z",
            &None,
            &Url::parse("https://dock.example/").unwrap(),
        );
        assert!(rendered.text.contains("🟢 Алерт метрики восстановился"));
        assert!(rendered.text.contains("Сервер: server-a"));
        assert!(rendered.text.contains("Сервис: api"));
        assert!(!rendered.text.contains("Все условия"));
        assert_eq!(rendered.parse_mode.as_deref(), Some("HTML"));
        assert!(rendered.reply_markup.is_some());
    }

    #[test]
    fn local_dock_url_is_rendered_as_text_instead_of_a_telegram_button() {
        let rendered = render_telegram_message(
            "status",
            "🟡",
            None,
            &BTreeMap::new(),
            Vec::new(),
            "http://localhost:7403",
        );

        assert!(rendered.reply_markup.is_none());
        assert!(rendered.text.contains("http://localhost:7403"));
    }

    #[test]
    fn watch_query_carries_the_alert_node_scope() {
        let mut alert = condition_alert("and", "normal", vec![trigger(60, "sum")]);
        alert.node_scope = Scope::Relay { id: 4 };
        let query = series_query_for(&alert);
        assert_eq!((query.relay_id, query.agent_id), (Some(4), None));
        alert.node_scope = Scope::Agent { id: 9 };
        let query = series_query_for(&alert);
        assert_eq!((query.relay_id, query.agent_id), (None, Some(9)));
        alert.node_scope = Scope::All;
        let query = series_query_for(&alert);
        assert_eq!((query.relay_id, query.agent_id), (None, None));
    }

    fn timestamp(ms: i64) -> String {
        OffsetDateTime::from_unix_timestamp_nanos(ms as i128 * 1_000_000)
            .unwrap()
            .format(&Rfc3339)
            .unwrap()
    }

    fn reply(as_of: i64, last_point: Option<i64>, count: u64) -> AlertSeriesReply {
        AlertSeriesReply {
            as_of: timestamp(as_of),
            last_point_at: last_point.map(timestamp),
            windows: vec![AlertSeriesWindow {
                seconds: 60,
                count,
                sum: count as f64,
                avg: (count > 0).then_some(1.0),
                min: (count > 0).then_some(1.0),
                max: (count > 0).then_some(1.0),
                increase: (count > 0).then_some(count as f64),
            }],
        }
    }

    fn condition_alert(
        combine: &str,
        no_data: &str,
        triggers: Vec<TriggerInput>,
    ) -> AlertDefinition {
        AlertDefinition {
            id: "test-alert".into(),
            key: test_key(),
            node_scope: Scope::All,
            enabled: true,
            environment: None,
            no_data: no_data.into(),
            combine: combine.into(),
            group_ids: vec![],
            triggers,
            revision: 1,
            enabled_at: 0,
            state: "unknown".into(),
            loss_registered: false,
            loss_at: None,
            last_point_at: None,
            last_as_of: None,
            no_recipients: true,
            title: None,
            description: None,
            message_template: None,
            message_fields: default_metric_message_fields(),
        }
    }

    fn trigger(window_seconds: u64, aggregate: &str) -> TriggerInput {
        TriggerInput {
            window_seconds,
            aggregate: aggregate.into(),
            comparator: ">".into(),
            threshold: 1.0,
        }
    }

    fn window(seconds: u64, count: u64, value: Option<f64>) -> AlertSeriesWindow {
        AlertSeriesWindow {
            seconds,
            count,
            sum: value.unwrap_or_default(),
            avg: value,
            min: value,
            max: value,
            increase: value,
        }
    }

    #[test]
    fn and_or_conditions_follow_three_valued_logic_and_empty_normal_rules_are_false() {
        let windows = [
            window(60, 1, Some(2.0)),
            window(120, 1, Some(0.0)),
            window(180, 1, None),
            window(240, 0, None),
        ];
        let by_seconds: BTreeMap<_, _> = windows
            .iter()
            .map(|window| (window.seconds, window))
            .collect();

        let and_unknown = condition_alert(
            "and",
            "loss",
            vec![trigger(60, "avg"), trigger(180, "increase")],
        );
        assert_eq!(
            evaluate_conditions(&and_unknown, &by_seconds),
            ConditionState::Unknown
        );
        let and_false = condition_alert(
            "and",
            "normal",
            vec![trigger(60, "avg"), trigger(120, "avg")],
        );
        assert_eq!(
            evaluate_conditions(&and_false, &by_seconds),
            ConditionState::Ok
        );
        let and_true = condition_alert("and", "normal", vec![trigger(60, "avg")]);
        assert_eq!(
            evaluate_conditions(&and_true, &by_seconds),
            ConditionState::Firing
        );

        let or_true = condition_alert(
            "or",
            "normal",
            vec![trigger(60, "avg"), trigger(180, "increase")],
        );
        assert_eq!(
            evaluate_conditions(&or_true, &by_seconds),
            ConditionState::Firing
        );
        let or_mixed_false = condition_alert(
            "or",
            "normal",
            vec![trigger(120, "avg"), trigger(180, "increase")],
        );
        assert_eq!(
            evaluate_conditions(&or_mixed_false, &by_seconds),
            ConditionState::Ok
        );
        let or_false = condition_alert("or", "normal", vec![trigger(120, "avg")]);
        assert_eq!(
            evaluate_conditions(&or_false, &by_seconds),
            ConditionState::Ok
        );

        let empty_avg = condition_alert("and", "normal", vec![trigger(240, "avg")]);
        assert_eq!(
            evaluate_conditions(&empty_avg, &by_seconds),
            ConditionState::Ok
        );
        let empty_sum = condition_alert("and", "normal", vec![trigger(240, "sum")]);
        assert_eq!(
            evaluate_conditions(&empty_sum, &by_seconds),
            ConditionState::Ok
        );
        let empty_loss = condition_alert("and", "loss", vec![trigger(240, "avg")]);
        assert_eq!(
            evaluate_conditions(&empty_loss, &by_seconds),
            ConditionState::Unknown
        );
    }

    #[tokio::test]
    async fn normal_less_than_with_insufficient_increase_does_not_fire_or_recover() {
        let directory = tempfile::tempdir().unwrap();
        let (store, configured, _) = configured_store(directory.path(), "normal", 1).await;
        let mut definition = input(configured.key.clone(), "normal", configured.group_ids);
        definition.triggers[0].aggregate = "increase".into();
        definition.triggers[0].comparator = "<".into();
        definition.triggers[0].threshold = 10.0;
        store.save_alert_definition(definition).await.unwrap();
        let (global, alerts) = store.alert_rows_for_worker().await.unwrap();
        let alert = alerts.into_iter().next().unwrap();
        let as_of = alert.enabled_at + 60_000;
        let mut insufficient = reply(as_of, Some(as_of - 1_000), 1);
        insufficient.windows[0].increase = None;

        store
            .commit_alert_evaluation(
                global.revision,
                alert.clone(),
                insufficient,
                Url::parse("https://dock.example/").unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(
            store
                .alert_definition(alert.key)
                .await
                .unwrap()
                .unwrap()
                .state,
            "ok"
        );
        assert!(outbox_rows(&store).await.is_empty());
    }

    async fn configured_store(
        directory: &std::path::Path,
        no_data: &str,
        recipient_count: usize,
    ) -> (Store, AlertDefinition, Vec<Recipient>) {
        let store = Store::open(directory).unwrap();
        let (alert, recipients) = configured_alert_store(&store, no_data, recipient_count).await;
        (store, alert, recipients)
    }

    async fn configured_alert_store(
        store: &Store,
        no_data: &str,
        recipient_count: usize,
    ) -> (AlertDefinition, Vec<Recipient>) {
        store.set_alert_token(BOT_TOKEN.into()).await.unwrap();
        assert!(store.set_alert_global(true).await.unwrap());
        let mut recipients = Vec::new();
        for index in 0..recipient_count {
            recipients.push(
                store
                    .create_recipient(RecipientInput {
                        kind: "personal".into(),
                        name: format!("on-call-{index}"),
                        chat_id: 1000 + index as i64,
                    })
                    .await
                    .unwrap(),
            );
        }
        let group = store
            .save_group(
                None,
                GroupInput {
                    kind: "personal".into(),
                    name: "on-call".into(),
                    recipient_ids: recipients
                        .iter()
                        .map(|recipient| recipient.id.clone())
                        .collect(),
                },
            )
            .await
            .unwrap();
        let key = test_key();
        store
            .save_alert_definition(input(key.clone(), no_data, vec![group.id]))
            .await
            .unwrap();
        let alert = store.alert_definition(key).await.unwrap().unwrap();
        (alert, recipients)
    }

    fn test_app_config(directory: &std::path::Path, core_url: &str) -> crate::config::Config {
        let htpasswd = directory.join("htpasswd");
        std::fs::write(
            &htpasswd,
            format!("admin:{}\n", bcrypt::hash("test-password", 4).unwrap()),
        )
        .unwrap();
        let values = [
            ("LOGNARA_DOCK_PUBLIC_URL", "http://localhost".to_owned()),
            ("LOGNARA_DOCK_CORE_URL", core_url.to_owned()),
            ("LOGNARA_DOCK_CORE_TOKEN", "abcdefghijklmnopqrst".to_owned()),
            (
                "LOGNARA_DOCK_CORE_ADMIN_TOKEN",
                "bcdefghijklmnopqrstu".to_owned(),
            ),
            (
                "LOGNARA_DOCK_HTPASSWD_FILE",
                htpasswd.to_string_lossy().into_owned(),
            ),
            (
                "LOGNARA_DOCK_DATA_DIR",
                directory.join("data").to_string_lossy().into_owned(),
            ),
            (
                "LOGNARA_DOCK_WEB_DIR",
                directory.to_string_lossy().into_owned(),
            ),
        ];
        crate::config::Config::from_lookup(|key| {
            values
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| value.clone())
        })
        .unwrap()
    }

    fn test_app(directory: &std::path::Path, core_url: &str) -> AppState {
        crate::api::App::open(test_app_config(directory, core_url)).unwrap()
    }

    async fn api_request(
        app: AppState,
        session: Option<&str>,
        origin: Option<&str>,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut request = Request::builder().method(method).uri(path);
        if let Some(session) = session {
            request = request.header(
                axum::http::header::COOKIE,
                format!("{}={session}", crate::session::SESSION_COOKIE),
            );
        }
        if let Some(origin) = origin {
            request = request.header(axum::http::header::ORIGIN, origin);
        }
        let request = match body {
            Some(body) => request
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
            None => request.body(Body::empty()).unwrap(),
        };
        let response = crate::api::router(app).oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn serve(router: Router) -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        (format!("http://{address}/"), server)
    }

    async fn outbox_rows(store: &Store) -> Vec<(String, String, i64)> {
        store
            .call(|conn| {
                let mut statement = conn.prepare(
                    "SELECT event_kind,message,created_at FROM metric_alert_outbox ORDER BY created_at,id",
                )?;
                statement
                    .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
                    .collect()
            })
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn legacy_outbox_delivery_keeps_plain_text_without_new_telegram_options() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path()).unwrap();
        let now = now_ms();
        store
            .call(move |conn| {
                conn.execute(
                    "UPDATE metric_alert_global SET enabled=1,telegram_token=?1 WHERE singleton=1",
                    [BOT_TOKEN],
                )?;
                conn.execute(
                    "INSERT INTO metric_alert_recipients(id,kind,name,chat_id,created_at)
                     VALUES('recipient-1','personal','On call',1001,?1)",
                    [now],
                )?;
                conn.execute(
                    "INSERT INTO metric_alert_outbox(id,event_kind,message,created_at,expires_at)
                     VALUES('legacy-event','core_recovered','Old <plain> & text',?1,?2)",
                    params![now, now + OUTBOX_TTL_MS],
                )?;
                conn.execute(
                    "INSERT INTO metric_alert_deliveries(event_id,recipient_id,recipient_name,recipient_chat_id,status,next_attempt_at)
                     VALUES('legacy-event','recipient-1','On call',1001,'pending',?1)",
                    [now],
                )?;
                Ok(())
            })
            .await
            .unwrap();

        let delivery = store.next_alert_delivery().await.unwrap().unwrap();
        assert_eq!(delivery.message, "Old <plain> & text");
        assert_eq!(delivery.parse_mode, None);
        assert_eq!(delivery.reply_markup, None);
    }

    #[tokio::test]
    async fn recipients_groups_and_alerts_enforce_references_and_show_no_recipients() {
        let directory = tempfile::tempdir().unwrap();
        let (store, alert, recipients) = configured_store(directory.path(), "normal", 1).await;
        assert!(
            !store
                .alert_definition(alert.key.clone())
                .await
                .unwrap()
                .unwrap()
                .no_recipients
        );
        assert!(matches!(
            store.delete_group(alert.group_ids[0].clone()).await,
            Err(AlertStoreError::GroupInUse(..))
        ));

        let kind_change = store
            .update_recipient(
                recipients[0].id.clone(),
                RecipientInput {
                    kind: "channel".into(),
                    name: "on-call".into(),
                    chat_id: recipients[0].chat_id,
                },
            )
            .await;
        assert!(matches!(
            kind_change,
            Err(AlertStoreError::RecipientKindInUse)
        ));

        assert!(
            store
                .delete_recipient(recipients[0].id.clone())
                .await
                .unwrap()
        );
        assert!(
            store
                .alert_definition(alert.key.clone())
                .await
                .unwrap()
                .unwrap()
                .no_recipients
        );
        let mut detached = input(alert.key.clone(), "normal", vec![]);
        detached.enabled = false;
        store.save_alert_definition(detached).await.unwrap();
        store
            .delete_group(alert.group_ids[0].clone())
            .await
            .unwrap();
        assert!(store.alert_settings().await.unwrap().groups.is_empty());
    }

    #[tokio::test]
    async fn group_changes_preserve_old_recipient_snapshots_and_only_affect_new_events() {
        let directory = tempfile::tempdir().unwrap();
        let (store, alert, recipients) = configured_store(directory.path(), "normal", 2).await;
        let group_id = alert.group_ids[0].clone();
        store
            .save_group(
                Some(group_id.clone()),
                GroupInput {
                    kind: "personal".into(),
                    name: "on-call".into(),
                    recipient_ids: vec![recipients[0].id.clone()],
                },
            )
            .await
            .unwrap();
        let (global, _) = store.alert_rows_for_worker().await.unwrap();
        let firing_at = alert.enabled_at + 60_000;
        store
            .commit_alert_evaluation(
                global.revision,
                alert.clone(),
                reply(firing_at, Some(firing_at - 1_000), 2),
                Url::parse("https://dock.example/").unwrap(),
            )
            .await
            .unwrap();
        let firing_event: String = store
            .call(|conn| {
                conn.query_row(
                    "SELECT id FROM metric_alert_outbox WHERE event_kind='firing'",
                    [],
                    |row| row.get(0),
                )
            })
            .await
            .unwrap();

        store
            .save_group(
                Some(group_id),
                GroupInput {
                    kind: "personal".into(),
                    name: "on-call".into(),
                    recipient_ids: recipients
                        .iter()
                        .map(|recipient| recipient.id.clone())
                        .collect(),
                },
            )
            .await
            .unwrap();
        let old_snapshot: i64 = store
            .call({
                let firing_event = firing_event.clone();
                let original_recipient = recipients[0].id.clone();
                move |conn| {
                    conn.query_row(
                        "SELECT count(*) FROM metric_alert_deliveries WHERE event_id=?1 AND recipient_id=?2 AND status='pending'",
                        params![firing_event,original_recipient],
                        |row| row.get(0),
                    )
                }
            })
            .await
            .unwrap();
        let new_recipient_replay: i64 = store
            .call({
                let firing_event = firing_event.clone();
                let new_recipient = recipients[1].id.clone();
                move |conn| {
                    conn.query_row(
                        "SELECT count(*) FROM metric_alert_deliveries WHERE event_id=?1 AND recipient_id=?2",
                        params![firing_event,new_recipient],
                        |row| row.get(0),
                    )
                }
            })
            .await
            .unwrap();
        assert_eq!(old_snapshot, 1);
        assert_eq!(new_recipient_replay, 0);

        let (global, _) = store.alert_rows_for_worker().await.unwrap();
        store
            .commit_alert_evaluation(
                global.revision,
                alert,
                reply(firing_at + 60_000, Some(firing_at + 59_000), 0),
                Url::parse("https://dock.example/").unwrap(),
            )
            .await
            .unwrap();
        let recipients_on_recovery: i64 = store
            .call(|conn| {
                conn.query_row(
                    "SELECT count(*) FROM metric_alert_deliveries d JOIN metric_alert_outbox o ON o.id=d.event_id WHERE o.event_kind='recovered'",
                    [],
                    |row| row.get(0),
                )
            })
            .await
            .unwrap();
        assert_eq!(recipients_on_recovery, 2);
    }

    #[tokio::test]
    async fn global_disable_cancels_pending_and_blocks_new_enabled_rules() {
        let directory = tempfile::tempdir().unwrap();
        let (store, alert, _) = configured_store(directory.path(), "normal", 1).await;
        let (global, _) = store.alert_rows_for_worker().await.unwrap();
        store
            .commit_alert_evaluation(
                global.revision,
                alert.clone(),
                reply(
                    alert.enabled_at + 60_000,
                    Some(alert.enabled_at + 59_000),
                    2,
                ),
                Url::parse("https://dock.example/").unwrap(),
            )
            .await
            .unwrap();
        assert!(store.set_alert_global(false).await.unwrap());
        assert!(store.next_alert_delivery().await.unwrap().is_none());
        let cancelled: i64 = store
            .call(|conn| {
                conn.query_row(
                    "SELECT count(*) FROM metric_alert_deliveries WHERE status='cancelled'",
                    [],
                    |row| row.get(0),
                )
            })
            .await
            .unwrap();
        assert_eq!(cancelled, 1);

        let mut disabled = input(alert.key.clone(), "normal", alert.group_ids.clone());
        disabled.enabled = false;
        store.save_alert_definition(disabled.clone()).await.unwrap();
        disabled.enabled = true;
        assert!(matches!(
            store.save_alert_definition(disabled).await,
            Err(AlertStoreError::Conflict("global_disabled_or_alert_limit"))
        ));
    }

    #[tokio::test]
    async fn disabling_preserves_state_and_reenable_starts_without_old_recovery() {
        let directory = tempfile::tempdir().unwrap();
        let (store, alert, _) = configured_store(directory.path(), "normal", 1).await;
        let (global, _) = store.alert_rows_for_worker().await.unwrap();
        store
            .commit_alert_evaluation(
                global.revision,
                alert.clone(),
                reply(
                    alert.enabled_at + 60_000,
                    Some(alert.enabled_at + 59_000),
                    2,
                ),
                Url::parse("https://dock.example/").unwrap(),
            )
            .await
            .unwrap();

        let mut disabled = input(alert.key.clone(), "normal", alert.group_ids.clone());
        disabled.enabled = false;
        store.save_alert_definition(disabled.clone()).await.unwrap();
        let paused = store
            .alert_definition(alert.key.clone())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(paused.state, "firing");
        assert!(!paused.enabled);

        disabled.enabled = true;
        store.save_alert_definition(disabled).await.unwrap();
        let resumed = store
            .alert_definition(alert.key.clone())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(resumed.state, "unknown");
        let (global, _) = store.alert_rows_for_worker().await.unwrap();
        store
            .commit_alert_evaluation(
                global.revision,
                resumed.clone(),
                reply(
                    resumed.enabled_at + 60_000,
                    Some(resumed.enabled_at + 59_000),
                    0,
                ),
                Url::parse("https://dock.example/").unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            outbox_rows(&store)
                .await
                .iter()
                .map(|row| row.0.as_str())
                .collect::<Vec<_>>(),
            ["firing"]
        );
    }

    #[tokio::test]
    async fn condition_transitions_are_atomic_fifod_and_stale_revisions_are_discarded() {
        let directory = tempfile::tempdir().unwrap();
        let (store, alert, recipients) = configured_store(directory.path(), "normal", 2).await;
        let public_url = Url::parse("https://dock.example/").unwrap();
        let firing_at = alert.enabled_at + 60_000;
        store
            .commit_alert_evaluation(
                3,
                alert.clone(),
                reply(firing_at, Some(firing_at - 1_000), 2),
                public_url.clone(),
            )
            .await
            .unwrap();
        // The current global revision includes token, recipient, group, alert and global writes.
        let (global, _) = store.alert_rows_for_worker().await.unwrap();
        store
            .commit_alert_evaluation(
                global.revision,
                alert.clone(),
                reply(firing_at, Some(firing_at - 1_000), 2),
                public_url.clone(),
            )
            .await
            .unwrap();
        store
            .commit_alert_evaluation(
                global.revision,
                alert.clone(),
                reply(firing_at + 60_000, Some(firing_at + 59_000), 0),
                public_url.clone(),
            )
            .await
            .unwrap();
        assert_eq!(
            outbox_rows(&store)
                .await
                .iter()
                .map(|row| row.0.as_str())
                .collect::<Vec<_>>(),
            ["firing", "recovered"]
        );
        let deliveries: i64 = store
            .call(|conn| {
                conn.query_row("SELECT count(*) FROM metric_alert_deliveries", [], |row| {
                    row.get(0)
                })
            })
            .await
            .unwrap();
        assert_eq!(deliveries, 4);

        let first = store.next_alert_delivery().await.unwrap().unwrap();
        assert!(
            recipients
                .iter()
                .any(|recipient| recipient.id == first.recipient_id)
        );
        let next = store.next_alert_delivery().await.unwrap().unwrap();
        assert_eq!(first.event_id, next.event_id);
        assert_ne!(first.recipient_id, next.recipient_id);
        let second_recipient_id = next.recipient_id.clone();
        store.finish_alert_delivery(next, Ok(())).await.unwrap();
        let recovery_for_second = store.next_alert_delivery().await.unwrap().unwrap();
        assert_ne!(first.event_id, recovery_for_second.event_id);
        assert_eq!(second_recipient_id, recovery_for_second.recipient_id);
        store
            .finish_alert_delivery(recovery_for_second, Ok(()))
            .await
            .unwrap();
        store
            .finish_alert_delivery(first.clone(), Ok(()))
            .await
            .unwrap();
        let recovery_for_first = store.next_alert_delivery().await.unwrap().unwrap();
        assert_ne!(first.event_id, recovery_for_first.event_id);
        assert_eq!(first.recipient_id, recovery_for_first.recipient_id);
        store
            .finish_alert_delivery(recovery_for_first, Ok(()))
            .await
            .unwrap();

        let mut edited = input(alert.key.clone(), "normal", alert.group_ids.clone());
        edited.triggers[0].threshold = 100.0;
        store.save_alert_definition(edited).await.unwrap();
        let (current, rows) = store.alert_rows_for_worker().await.unwrap();
        let edited_alert = rows.into_iter().find(|row| row.key == alert.key).unwrap();
        store
            .commit_alert_evaluation(
                global.revision,
                edited_alert.clone(),
                reply(firing_at + 120_000, Some(firing_at + 119_000), 2),
                public_url,
            )
            .await
            .unwrap();
        assert_eq!(current.revision, global.revision + 1);
        assert_eq!(outbox_rows(&store).await.len(), 2);
    }

    #[tokio::test]
    async fn loss_waits_five_minutes_freezes_conditions_and_recovers_only_after_registration() {
        let directory = tempfile::tempdir().unwrap();
        let (store, alert, _) = configured_store(directory.path(), "loss", 1).await;
        let url = Url::parse("https://dock.example/").unwrap();
        let as_of = alert.enabled_at + NO_DATA_LOSS_MS - 1;
        let (global, _) = store.alert_rows_for_worker().await.unwrap();
        store
            .commit_alert_evaluation(
                global.revision,
                alert.clone(),
                reply(as_of, None, 0),
                url.clone(),
            )
            .await
            .unwrap();
        assert!(outbox_rows(&store).await.is_empty());

        let as_of = alert.enabled_at + NO_DATA_LOSS_MS;
        store
            .commit_alert_evaluation(
                global.revision,
                alert.clone(),
                reply(as_of, None, 0),
                url.clone(),
            )
            .await
            .unwrap();
        let current = store
            .alert_definition(alert.key.clone())
            .await
            .unwrap()
            .unwrap();
        assert!(current.loss_registered);
        assert_eq!(current.state, "unknown");
        assert_eq!(outbox_rows(&store).await[0].0, "data_loss");

        store
            .commit_alert_evaluation(
                global.revision,
                alert.clone(),
                reply(as_of + 60_000, None, 0),
                url.clone(),
            )
            .await
            .unwrap();
        assert_eq!(outbox_rows(&store).await.len(), 1);

        store
            .commit_alert_evaluation(
                global.revision,
                alert,
                reply(as_of + 120_000, Some(as_of + 119_000), 2),
                url,
            )
            .await
            .unwrap();
        let recovered = outbox_rows(&store).await;
        assert_eq!(
            recovered
                .iter()
                .map(|row| row.0.as_str())
                .collect::<Vec<_>>(),
            ["data_loss", "data_recovered", "firing"]
        );
        assert!(recovered[1].1.contains("Точки метрики снова поступают"));
    }

    #[tokio::test]
    async fn loss_uses_last_known_point_after_core_freshness_is_capped() {
        let directory = tempfile::tempdir().unwrap();
        let (store, alert, _) = configured_store(directory.path(), "loss", 1).await;
        let url = Url::parse("https://dock.example/").unwrap();
        let (global, _) = store.alert_rows_for_worker().await.unwrap();
        let point_at = alert.enabled_at + 60_000;
        store
            .commit_alert_evaluation(
                global.revision,
                alert.clone(),
                reply(point_at + 1_000, Some(point_at), 2),
                url.clone(),
            )
            .await
            .unwrap();

        let before_loss = point_at + NO_DATA_LOSS_MS - 1;
        store
            .commit_alert_evaluation(
                global.revision,
                alert.clone(),
                reply(before_loss, None, 0),
                url.clone(),
            )
            .await
            .unwrap();
        assert_eq!(outbox_rows(&store).await.len(), 1);
        assert!(
            !store
                .alert_definition(alert.key.clone())
                .await
                .unwrap()
                .unwrap()
                .loss_registered
        );

        store
            .commit_alert_evaluation(
                global.revision,
                alert.clone(),
                reply(point_at + NO_DATA_LOSS_MS, None, 0),
                url,
            )
            .await
            .unwrap();
        let rows = outbox_rows(&store).await;
        assert_eq!(
            rows.iter().map(|row| row.0.as_str()).collect::<Vec<_>>(),
            ["firing", "data_loss"]
        );
        let current = store.alert_definition(alert.key).await.unwrap().unwrap();
        assert!(current.loss_registered);
        assert_eq!(current.loss_at, Some(point_at + NO_DATA_LOSS_MS));
        assert_eq!(current.last_point_at, Some(point_at));
    }

    #[tokio::test]
    async fn loss_insufficient_increase_preserves_firing_without_false_recovery() {
        let directory = tempfile::tempdir().unwrap();
        let (store, configured, _) = configured_store(directory.path(), "loss", 1).await;
        let mut definition = input(configured.key.clone(), "loss", configured.group_ids);
        definition.triggers[0] = trigger(60, "increase");
        store.save_alert_definition(definition).await.unwrap();
        let alert = store
            .alert_definition(configured.key)
            .await
            .unwrap()
            .unwrap();
        let url = Url::parse("https://dock.example/").unwrap();
        let (global, _) = store.alert_rows_for_worker().await.unwrap();
        let firing_at = alert.enabled_at + 60_000;
        store
            .commit_alert_evaluation(
                global.revision,
                alert.clone(),
                reply(firing_at, Some(firing_at), 2),
                url.clone(),
            )
            .await
            .unwrap();
        let mut insufficient = reply(firing_at + 60_000, Some(firing_at), 2);
        insufficient.windows[0].increase = None;
        store
            .commit_alert_evaluation(global.revision, alert.clone(), insufficient, url.clone())
            .await
            .unwrap();
        assert_eq!(
            store
                .alert_definition(alert.key.clone())
                .await
                .unwrap()
                .unwrap()
                .state,
            "firing"
        );
        assert_eq!(outbox_rows(&store).await[0].0, "firing");

        let loss_at = firing_at + NO_DATA_LOSS_MS;
        let mut loss = reply(loss_at, Some(firing_at), 2);
        loss.windows[0].increase = None;
        store
            .commit_alert_evaluation(global.revision, alert.clone(), loss, url.clone())
            .await
            .unwrap();
        store
            .commit_alert_evaluation(
                global.revision,
                alert.clone(),
                reply(loss_at + 60_000, Some(loss_at + 59_000), 2),
                url,
            )
            .await
            .unwrap();

        assert_eq!(
            outbox_rows(&store)
                .await
                .iter()
                .map(|row| row.0.as_str())
                .collect::<Vec<_>>(),
            ["firing", "data_loss", "data_recovered"]
        );
        assert_eq!(
            store
                .alert_definition(alert.key)
                .await
                .unwrap()
                .unwrap()
                .state,
            "firing"
        );
    }

    #[tokio::test]
    async fn restart_recovers_sending_delivery_and_settings_never_return_token() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().to_owned();
        let (store, alert, _) = configured_store(&path, "normal", 1).await;
        let (global, _) = store.alert_rows_for_worker().await.unwrap();
        store
            .commit_alert_evaluation(
                global.revision,
                alert.clone(),
                reply(
                    alert.enabled_at + 60_000,
                    Some(alert.enabled_at + 59_000),
                    2,
                ),
                Url::parse("https://dock.example/").unwrap(),
            )
            .await
            .unwrap();
        let settings = store.alert_settings().await.unwrap();
        assert!(
            !serde_json::to_string(&settings)
                .unwrap()
                .contains(BOT_TOKEN)
        );
        let delivery = store.next_alert_delivery().await.unwrap().unwrap();
        assert_eq!(delivery.attempts, 1);
        drop(store);

        let reopened = Store::open(&path).unwrap();
        let status: String = reopened
            .call(|conn| {
                conn.query_row("SELECT status FROM metric_alert_deliveries", [], |row| {
                    row.get(0)
                })
            })
            .await
            .unwrap();
        assert_eq!(status, "pending");
        assert_eq!(
            reopened
                .alert_definition(alert.key)
                .await
                .unwrap()
                .unwrap()
                .state,
            "firing"
        );
    }

    #[tokio::test]
    async fn deleting_recipients_cancels_pending_and_inflight_deliveries_across_restart() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().to_owned();
        let (store, alert, recipients) = configured_store(&path, "normal", 2).await;
        let (global, _) = store.alert_rows_for_worker().await.unwrap();
        store
            .commit_alert_evaluation(
                global.revision,
                alert.clone(),
                reply(
                    alert.enabled_at + 60_000,
                    Some(alert.enabled_at + 59_000),
                    2,
                ),
                Url::parse("https://dock.example/").unwrap(),
            )
            .await
            .unwrap();

        let sending = store.next_alert_delivery().await.unwrap().unwrap();
        let pending_recipient = recipients
            .iter()
            .find(|recipient| recipient.id != sending.recipient_id)
            .unwrap()
            .id
            .clone();
        store
            .delete_recipient(sending.recipient_id.clone())
            .await
            .unwrap();
        store.delete_recipient(pending_recipient).await.unwrap();

        store
            .finish_alert_delivery(sending, Err(TelegramFailure::InvalidToken))
            .await
            .unwrap();
        let statuses: Vec<String> = store
            .call(|conn| {
                let mut statement = conn
                    .prepare("SELECT status FROM metric_alert_deliveries ORDER BY recipient_id")?;
                statement.query_map([], |row| row.get(0))?.collect()
            })
            .await
            .unwrap();
        assert_eq!(statuses, ["cancelled", "cancelled"]);
        assert!(!store.alert_settings().await.unwrap().send_paused);
        assert!(store.next_alert_delivery().await.unwrap().is_none());
        drop(store);

        let reopened = Store::open(&path).unwrap();
        let statuses: Vec<String> = reopened
            .call(|conn| {
                let mut statement = conn
                    .prepare("SELECT status FROM metric_alert_deliveries ORDER BY recipient_id")?;
                statement.query_map([], |row| row.get(0))?.collect()
            })
            .await
            .unwrap();
        assert_eq!(statuses, ["cancelled", "cancelled"]);
        assert!(reopened.next_alert_delivery().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn delivery_queue_handles_global_throttle_permanent_errors_and_partial_success() {
        let directory = tempfile::tempdir().unwrap();
        let (store, alert, recipients) = configured_store(directory.path(), "normal", 2).await;
        let (global, _) = store.alert_rows_for_worker().await.unwrap();
        store
            .commit_alert_evaluation(
                global.revision,
                alert.clone(),
                reply(
                    alert.enabled_at + 60_000,
                    Some(alert.enabled_at + 59_000),
                    2,
                ),
                Url::parse("https://dock.example/").unwrap(),
            )
            .await
            .unwrap();

        let limited = store.next_alert_delivery().await.unwrap().unwrap();
        store
            .finish_alert_delivery(
                limited.clone(),
                Err(TelegramFailure::RateLimited(Duration::from_secs(7))),
            )
            .await
            .unwrap();
        assert!(store.next_alert_delivery().await.unwrap().is_none());
        store
            .call(|conn| {
                conn.execute(
                    "UPDATE metric_alert_global SET throttle_until=0 WHERE singleton=1",
                    [],
                )?;
                conn.execute(
                    "UPDATE metric_alert_deliveries SET next_attempt_at=0 WHERE status='pending'",
                    [],
                )?;
                Ok(())
            })
            .await
            .unwrap();

        let rejected = store.next_alert_delivery().await.unwrap().unwrap();
        assert_eq!(limited.recipient_id, rejected.recipient_id);
        store
            .finish_alert_delivery(rejected, Err(TelegramFailure::PermanentRecipient))
            .await
            .unwrap();
        let accepted = store.next_alert_delivery().await.unwrap().unwrap();
        assert_ne!(limited.recipient_id, accepted.recipient_id);
        store.finish_alert_delivery(accepted, Ok(())).await.unwrap();
        assert!(store.next_alert_delivery().await.unwrap().is_none());

        let statuses: Vec<(String, String)> = store
            .call(|conn| {
                let mut statement = conn.prepare(
                    "SELECT recipient_id,status FROM metric_alert_deliveries ORDER BY recipient_id",
                )?;
                statement
                    .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                    .collect()
            })
            .await
            .unwrap();
        assert_eq!(statuses.len(), 2);
        assert!(statuses.iter().any(|row| row.1 == "permanent_error"));
        assert!(statuses.iter().any(|row| row.1 == "sent"));
        let settings = store.alert_settings().await.unwrap();
        assert!(settings.recipients.iter().any(|recipient| {
            recipients.iter().any(|source| source.id == recipient.id)
                && recipient.last_error.is_some()
        }));
    }

    #[tokio::test]
    async fn core_loss_is_once_per_outage_and_recovery_uses_the_loss_recipient_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        let (store, alert, recipients) = configured_store(directory.path(), "normal", 2).await;
        let url = Url::parse("https://dock.example/").unwrap();
        store
            .record_core_health("busy", Some("overloaded"), None, url.clone())
            .await
            .unwrap();
        assert!(outbox_rows(&store).await.is_empty());
        let loss = format!("Core unavailable\n{}", settings_link(&url));
        store
            .record_core_health(
                "unavailable",
                Some("unavailable"),
                Some(loss.clone()),
                url.clone(),
            )
            .await
            .unwrap();
        store
            .record_core_health("unavailable", Some("unavailable"), Some(loss), url.clone())
            .await
            .unwrap();
        assert_eq!(outbox_rows(&store).await.len(), 1);
        let pending: i64 = store
            .call(|conn| {
                conn.query_row(
                    "SELECT count(*) FROM metric_alert_deliveries WHERE status='pending'",
                    [],
                    |row| row.get(0),
                )
            })
            .await
            .unwrap();
        assert_eq!(pending, 2);
        let added = store
            .create_recipient(RecipientInput {
                kind: "personal".into(),
                name: "later-on-call".into(),
                chat_id: 2000,
            })
            .await
            .unwrap();
        store
            .save_group(
                Some(alert.group_ids[0].clone()),
                GroupInput {
                    kind: "personal".into(),
                    name: "on-call".into(),
                    recipient_ids: vec![recipients[1].id.clone(), added.id.clone()],
                },
            )
            .await
            .unwrap();
        store
            .delete_recipient(recipients[0].id.clone())
            .await
            .unwrap();
        store
            .record_core_health("available", None, None, url)
            .await
            .unwrap();
        let rows = outbox_rows(&store).await;
        assert_eq!(
            rows.iter().map(|row| row.0.as_str()).collect::<Vec<_>>(),
            ["core_unavailable", "core_recovered"]
        );
        let loss_deliveries: Vec<(String, String)> = store
            .call(|conn| {
                let mut statement = conn.prepare(
                    "SELECT d.recipient_id,d.status FROM metric_alert_deliveries d
                     JOIN metric_alert_outbox o ON o.id=d.event_id
                     WHERE o.event_kind='core_unavailable' ORDER BY d.recipient_id",
                )?;
                statement
                    .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                    .collect()
            })
            .await
            .unwrap();
        assert!(loss_deliveries.contains(&(recipients[0].id.clone(), "cancelled".into())));
        assert!(loss_deliveries.contains(&(recipients[1].id.clone(), "pending".into())));
        let recovery_recipients: Vec<String> = store
            .call(|conn| {
                let mut statement = conn.prepare(
                    "SELECT d.recipient_id FROM metric_alert_deliveries d
                     JOIN metric_alert_outbox o ON o.id=d.event_id
                     WHERE o.event_kind='core_recovered' ORDER BY d.recipient_id",
                )?;
                statement.query_map([], |row| row.get(0))?.collect()
            })
            .await
            .unwrap();
        assert_eq!(recovery_recipients, [recipients[1].id.clone()]);
        assert!(!recovery_recipients.contains(&recipients[0].id));
        assert!(!recovery_recipients.contains(&added.id));
    }

    #[derive(Clone, Default)]
    struct MockTelegram {
        replies: Arc<Mutex<VecDeque<(StatusCode, Value)>>>,
        requests: Arc<Mutex<Vec<(String, Value)>>>,
        send_delay: Arc<Mutex<Duration>>,
    }

    async fn get_me(
        State(mock): State<MockTelegram>,
        Path(token): Path<String>,
    ) -> (StatusCode, Json<Value>) {
        mock.requests.lock().unwrap().push((token, Value::Null));
        mock.replies
            .lock()
            .unwrap()
            .pop_front()
            .map(|(status, body)| (status, Json(body)))
            .unwrap_or((StatusCode::OK, Json(json!({"ok":true}))))
    }

    async fn send_message(
        State(mock): State<MockTelegram>,
        Path(token): Path<String>,
        Json(body): Json<Value>,
    ) -> (StatusCode, Json<Value>) {
        mock.requests.lock().unwrap().push((token.clone(), body));
        let delay = *mock.send_delay.lock().unwrap();
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        mock.replies
            .lock()
            .unwrap()
            .pop_front()
            .map(|(status, body)| (status, Json(body)))
            .unwrap_or((StatusCode::OK, Json(json!({"ok":true}))))
    }

    async fn install_mock_telegram(
        app: &mut AppState,
        mock: MockTelegram,
    ) -> tokio::task::JoinHandle<()> {
        let router = Router::new()
            .route("/{token}/getMe", get(get_me))
            .route("/{token}/sendMessage", post(send_message))
            .with_state(mock);
        let (base, server) = serve(router).await;
        Arc::get_mut(app).unwrap().telegram = TelegramClient::with_base(Url::parse(&base).unwrap());
        server
    }

    #[derive(Clone)]
    struct MockCore {
        requests: Arc<AtomicUsize>,
    }

    async fn unavailable_core(
        State(mock): State<MockCore>,
        Json(_request): Json<Value>,
    ) -> (StatusCode, Json<Value>) {
        mock.requests.fetch_add(1, Ordering::SeqCst);
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"storage unavailable"})),
        )
    }

    #[test]
    fn retry_backoff_uses_seconds_and_caps_at_one_hour() {
        assert_eq!(retry_delay(1), 30_000);
        assert_eq!(retry_delay(2), 60_000);
        assert_eq!(retry_delay(7), 1_920_000);
        assert_eq!(retry_delay(8), 3_600_000);
        assert_eq!(retry_delay(u32::MAX), 3_600_000);
    }

    #[tokio::test]
    async fn alerts_router_enforces_origin_auth_group_references_and_missing_token() {
        let directory = tempfile::tempdir().unwrap();
        let app = test_app(directory.path(), "http://127.0.0.1:1/");
        let session = app.sessions.create("admin").await.unwrap();

        let (status, _) = api_request(
            app.clone(),
            Some(&session),
            Some("https://attacker.example"),
            "PUT",
            "/api/alerts/global",
            Some(json!({"enabled":true})),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        let (status, _) = api_request(
            app.clone(),
            None,
            Some("http://localhost"),
            "PUT",
            "/api/alerts/global",
            Some(json!({"enabled":true})),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        let (status, body) = api_request(
            app.clone(),
            Some(&session),
            Some("http://localhost"),
            "PUT",
            "/api/alerts/global",
            Some(json!({"enabled":true})),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"], "bot_token_required");

        app.store.set_alert_token(BOT_TOKEN.into()).await.unwrap();
        assert!(app.store.set_alert_global(true).await.unwrap());
        let recipient = app
            .store
            .create_recipient(RecipientInput {
                kind: "personal".into(),
                name: "on-call".into(),
                chat_id: 1001,
            })
            .await
            .unwrap();
        let group = app
            .store
            .save_group(
                None,
                GroupInput {
                    kind: "personal".into(),
                    name: "on-call".into(),
                    recipient_ids: vec![recipient.id],
                },
            )
            .await
            .unwrap();
        app.store
            .save_alert_definition(input(test_key(), "normal", vec![group.id.clone()]))
            .await
            .unwrap();
        let (status, body) = api_request(
            app.clone(),
            Some(&session),
            Some("http://localhost"),
            "DELETE",
            &format!("/api/alerts/groups/{}", group.id),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"], "group_in_use");
        assert_eq!(body["alerts"][0]["name"], "requests");

        let (status, body) = api_request(
            app,
            Some(&session),
            Some("http://localhost"),
            "GET",
            "/api/alerts",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(!body.to_string().contains(BOT_TOKEN));
    }

    #[tokio::test]
    async fn successful_global_enable_clears_a_paused_token() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = test_app(directory.path(), "http://127.0.0.1:1/");
        app.store.set_alert_token(BOT_TOKEN.into()).await.unwrap();
        assert!(app.store.set_alert_global(true).await.unwrap());
        app.store
            .call(|conn| {
                conn.execute(
                    "UPDATE metric_alert_global SET send_paused=1,send_pause_reason='invalid_token' WHERE singleton=1",
                    [],
                )
            })
            .await
            .unwrap();

        let mock = MockTelegram::default();
        let server = install_mock_telegram(&mut app, mock).await;
        let session = app.sessions.create("admin").await.unwrap();
        let (status, _) = api_request(
            app.clone(),
            Some(&session),
            Some("http://localhost"),
            "PUT",
            "/api/alerts/global",
            Some(json!({"enabled":true})),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let settings = app.store.alert_settings().await.unwrap();
        assert!(!settings.send_paused);
        assert_eq!(settings.send_pause_reason, None);
        server.abort();
    }

    #[tokio::test]
    async fn test_message_requires_session_and_reaches_each_group_chat_once() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = test_app(directory.path(), "http://127.0.0.1:1/");
        let mock = MockTelegram::default();
        mock.replies.lock().unwrap().extend([
            (StatusCode::OK, json!({"ok":true})),
            (
                StatusCode::FORBIDDEN,
                json!({"ok":false,"error_code":403,"description":"Forbidden: bot was blocked by the user"}),
            ),
        ]);
        let server = install_mock_telegram(&mut app, mock.clone()).await;
        let session = app.sessions.create("admin").await.unwrap();
        let body = json!({
            "message": {"kind":"metric","name":"requests","service":"billing","message_fields":["name","service"]},
            "group_ids": [],
        });

        let (status, _) = api_request(
            app.clone(),
            None,
            Some("http://localhost"),
            "POST",
            "/api/alerts/test",
            Some(body.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, _) = api_request(
            app.clone(),
            None,
            Some("http://localhost"),
            "POST",
            "/api/alerts/preview",
            Some(body["message"].clone()),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        let mut recipients = Vec::new();
        for (name, chat_id) in [("on-call", 1001), ("blocked", 1002)] {
            recipients.push(
                app.store
                    .create_recipient(RecipientInput {
                        kind: "personal".into(),
                        name: name.into(),
                        chat_id,
                    })
                    .await
                    .unwrap()
                    .id,
            );
        }
        let mut group_ids = Vec::new();
        for name in ["first", "second"] {
            let group = app
                .store
                .save_group(
                    None,
                    GroupInput {
                        kind: "personal".into(),
                        name: name.into(),
                        recipient_ids: recipients.clone(),
                    },
                )
                .await
                .unwrap();
            group_ids.push(group.id);
        }
        let mut body = body;
        body["group_ids"] = json!(group_ids);

        let (status, response) = api_request(
            app.clone(),
            Some(&session),
            Some("http://localhost"),
            "POST",
            "/api/alerts/test",
            Some(body.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(response["error"], "bot_token_required");

        app.store.set_alert_token(BOT_TOKEN.into()).await.unwrap();
        let (status, response) = api_request(
            app.clone(),
            Some(&session),
            Some("http://localhost"),
            "POST",
            "/api/alerts/test",
            Some(body),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(response["sent"], 1);
        assert_eq!(response["failed"][0]["recipient"], "blocked");
        assert_eq!(response["failed"][0]["error"], "recipient_unavailable");
        let requests = mock.requests.lock().unwrap().clone();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].1["chat_id"], 1001);
        assert_eq!(requests[1].1["chat_id"], 1002);
        assert!(
            requests[0].1["text"]
                .as_str()
                .unwrap()
                .starts_with("🧪 <b>Тестовое сообщение</b>")
        );
        let settings = app.store.alert_settings().await.unwrap();
        assert_eq!(settings.pending_deliveries, 0);
        server.abort();
    }

    #[tokio::test]
    async fn worker_stops_at_first_core_outage_and_sends_pending_outbox() {
        let core_mock = MockCore {
            requests: Arc::new(AtomicUsize::new(0)),
        };
        let core_router = Router::new()
            .route("/v1/series/alerts/query", post(unavailable_core))
            .with_state(core_mock.clone());
        let (core_url, core_server) = serve(core_router).await;
        let directory = tempfile::tempdir().unwrap();
        let mut app = test_app(directory.path(), &core_url);
        let telegram_mock = MockTelegram::default();
        let telegram_server = install_mock_telegram(&mut app, telegram_mock.clone()).await;
        let (alert, _) = configured_alert_store(&app.store, "normal", 1).await;
        let global = app.store.alert_global().await.unwrap();
        app.store
            .commit_alert_evaluation(
                global.revision,
                alert,
                reply(now_ms() + 60_000, Some(now_ms() + 59_000), 2),
                Url::parse("https://dock.example/").unwrap(),
            )
            .await
            .unwrap();
        let mut second_key = test_key();
        second_key.name = "errors".into();
        app.store
            .save_alert_definition(input(second_key, "normal", vec![]))
            .await
            .unwrap();
        let (global, alerts) = app.store.alert_rows_for_worker().await.unwrap();
        assert_eq!(alerts.len(), 2);

        let mut core_unavailable_since = None;
        run_alert_pass(&app, global, alerts, &mut core_unavailable_since).await;

        assert_eq!(core_mock.requests.load(Ordering::SeqCst), 1);
        assert!(core_unavailable_since.is_some());
        assert_eq!(
            app.store.alert_settings().await.unwrap().core_status,
            "unavailable"
        );
        assert_eq!(telegram_mock.requests.lock().unwrap().len(), 1);
        let sent: i64 = app
            .store
            .call(|conn| {
                conn.query_row(
                    "SELECT count(*) FROM metric_alert_deliveries WHERE status='sent'",
                    [],
                    |row| row.get(0),
                )
            })
            .await
            .unwrap();
        assert_eq!(sent, 1);
        core_server.abort();
        telegram_server.abort();
    }

    #[tokio::test]
    async fn delivery_pass_is_limited_by_count_and_deadline() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = test_app(directory.path(), "http://127.0.0.1:1/");
        let mock = MockTelegram::default();
        let telegram_server = install_mock_telegram(&mut app, mock.clone()).await;
        let (alert, _) = configured_alert_store(&app.store, "normal", 3).await;
        let global = app.store.alert_global().await.unwrap();
        app.store
            .commit_alert_evaluation(
                global.revision,
                alert,
                reply(now_ms() + 60_000, Some(now_ms() + 59_000), 2),
                Url::parse("https://dock.example/").unwrap(),
            )
            .await
            .unwrap();

        send_pending_bounded(&app, 2, Duration::from_secs(5)).await;
        assert_eq!(mock.requests.lock().unwrap().len(), 2);
        let pending: i64 = app
            .store
            .call(|conn| {
                conn.query_row(
                    "SELECT count(*) FROM metric_alert_deliveries WHERE status='pending'",
                    [],
                    |row| row.get(0),
                )
            })
            .await
            .unwrap();
        assert_eq!(pending, 1);

        *mock.send_delay.lock().unwrap() = Duration::from_secs(1);
        let started = Instant::now();
        send_pending_bounded(&app, 10, Duration::from_millis(50)).await;
        assert!(started.elapsed() < Duration::from_millis(500));
        assert_eq!(mock.requests.lock().unwrap().len(), 3);
        let (status, attempts): (String, i64) = app
            .store
            .call(|conn| {
                conn.query_row(
                    "SELECT status,attempts FROM metric_alert_deliveries WHERE status='pending'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
            })
            .await
            .unwrap();
        assert_eq!(status, "pending");
        assert_eq!(attempts, 1);
        telegram_server.abort();
    }

    #[tokio::test]
    async fn telegram_client_classifies_partial_errors_without_exposing_token_or_response() {
        let mock = MockTelegram::default();
        mock.replies.lock().unwrap().extend([
            (StatusCode::OK, json!({"ok":true})),
            (
                StatusCode::UNAUTHORIZED,
                json!({"ok":false,"error_code":401}),
            ),
            (
                StatusCode::TOO_MANY_REQUESTS,
                json!({"ok":false,"error_code":429,"parameters":{"retry_after":7}}),
            ),
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({"ok":false,"description":"Telegram is temporarily unavailable"}),
            ),
            (
                StatusCode::TOO_MANY_REQUESTS,
                json!({"ok":false,"error_code":429,"parameters":{"retry_after":7}}),
            ),
            (
                StatusCode::FORBIDDEN,
                json!({"ok":false,"description":"Forbidden: bot was blocked by the user"}),
            ),
            (
                StatusCode::UNAUTHORIZED,
                json!({"ok":false,"error_code":401,"description":"secret token rejected"}),
            ),
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({"ok":false,"description":"do not log this"}),
            ),
        ]);
        let app = Router::new()
            .route("/{token}/getMe", get(get_me))
            .route("/{token}/sendMessage", post(send_message))
            .with_state(mock.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = TelegramClient::with_base(Url::parse(&format!("http://{address}/")).unwrap());
        assert!(client.verify_token(BOT_TOKEN).await.is_ok());
        assert!(matches!(
            client.verify_token(BOT_TOKEN).await,
            Err(TelegramFailure::InvalidToken)
        ));
        assert!(matches!(
            client.verify_token(BOT_TOKEN).await,
            Err(TelegramFailure::RateLimited(wait)) if wait == Duration::from_secs(7)
        ));
        let verification_error = client.verify_token(BOT_TOKEN).await.unwrap_err();
        assert!(matches!(verification_error, TelegramFailure::Transient));
        let rate = client
            .send(BOT_TOKEN, 1001, "plain text", None, None)
            .await
            .unwrap_err();
        assert!(
            matches!(rate, TelegramFailure::RateLimited(wait) if wait == Duration::from_secs(7))
        );
        let permanent = client
            .send(BOT_TOKEN, 1002, "plain text", None, None)
            .await
            .unwrap_err();
        assert!(matches!(permanent, TelegramFailure::PermanentRecipient));
        let invalid = client
            .send(BOT_TOKEN, 1003, "plain text", None, None)
            .await
            .unwrap_err();
        assert!(matches!(invalid, TelegramFailure::InvalidToken));
        let transient = client
            .send(BOT_TOKEN, 1004, "plain text", None, None)
            .await
            .unwrap_err();
        assert!(matches!(transient, TelegramFailure::Transient));
        client
            .send(
                BOT_TOKEN,
                1005,
                "<b>formatted</b>",
                Some("HTML"),
                Some(r#"{"inline_keyboard":[[{"text":"Открыть в Dock","url":"https://dock.example/"}]]}"#),
            )
            .await
            .unwrap();
        assert!(!format!("{invalid:?}").contains(BOT_TOKEN));
        assert!(!format!("{transient:?}").contains("do not log this"));
        let requests = mock.requests.lock().unwrap();
        assert_eq!(requests.len(), 9);
        assert_eq!(requests[4].1["text"], "plain text");
        assert_eq!(requests[4].1["chat_id"], 1001);
        assert_eq!(requests[8].1["parse_mode"], "HTML");
        assert_eq!(
            requests[8].1["reply_markup"]["inline_keyboard"][0][0]["text"],
            "Открыть в Dock"
        );
        server.abort();
    }

    #[tokio::test]
    async fn telegram_send_retries_button_and_parse_errors_within_one_delivery() {
        let mock = MockTelegram::default();
        mock.replies.lock().unwrap().extend([
            (
                StatusCode::BAD_REQUEST,
                json!({"ok":false,"error_code":400,"description":"Bad Request: BUTTON_URL_INVALID"}),
            ),
            (StatusCode::OK, json!({"ok":true})),
            (
                StatusCode::BAD_REQUEST,
                json!({"ok":false,"error_code":400,"description":"Bad Request: can't parse entities: Unsupported start tag"}),
            ),
            (StatusCode::OK, json!({"ok":true})),
            (
                StatusCode::BAD_REQUEST,
                json!({"ok":false,"error_code":400,"description":"Bad Request: chat not found"}),
            ),
        ]);
        let app = Router::new()
            .route("/{token}/getMe", get(get_me))
            .route("/{token}/sendMessage", post(send_message))
            .with_state(mock.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = TelegramClient::with_base(Url::parse(&format!("http://{address}/")).unwrap());
        let markup = r#"{"inline_keyboard":[[{"text":"Открыть в Dock","url":"https://dock.example/?a=1&b=2"}]]}"#;

        client
            .send(
                BOT_TOKEN,
                1001,
                "<b>A &amp; B</b>",
                Some("HTML"),
                Some(markup),
            )
            .await
            .unwrap();
        client
            .send(
                BOT_TOKEN,
                1002,
                "<b>formatted</b>",
                Some("HTML"),
                Some(markup),
            )
            .await
            .unwrap();
        assert!(matches!(
            client
                .send(BOT_TOKEN, 1003, "plain text", None, None,)
                .await,
            Err(TelegramFailure::PermanentRecipient)
        ));

        let requests = mock.requests.lock().unwrap();
        assert_eq!(requests.len(), 5);
        assert!(requests[0].1["reply_markup"].is_object());
        assert!(requests[1].1.get("reply_markup").is_none());
        assert_eq!(requests[1].1["parse_mode"], "HTML");
        assert!(
            requests[1].1["text"]
                .as_str()
                .unwrap()
                .contains("https://dock.example/?a=1&amp;b=2")
        );
        assert!(requests[2].1.get("parse_mode").is_some());
        assert!(requests[3].1.get("parse_mode").is_none());
        assert!(requests[3].1.get("reply_markup").is_some());
        assert!(
            requests[3].1["text"]
                .as_str()
                .unwrap()
                .contains("formatted")
        );
        assert!(!requests[3].1["text"].as_str().unwrap().contains("<b>"));
        server.abort();
    }

    #[tokio::test]
    async fn token_verification_errors_use_telegram_api_codes() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = test_app(directory.path(), "http://127.0.0.1:1/");
        let mock = MockTelegram::default();
        mock.replies.lock().unwrap().extend([
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({"ok":false,"description":"temporary"}),
            ),
            (
                StatusCode::TOO_MANY_REQUESTS,
                json!({"ok":false,"error_code":429,"parameters":{"retry_after":9}}),
            ),
            (
                StatusCode::UNAUTHORIZED,
                json!({"ok":false,"error_code":401}),
            ),
        ]);
        let server = install_mock_telegram(&mut app, mock).await;
        let session = app.sessions.create("admin").await.unwrap();
        for (expected_status, expected_code) in [
            (StatusCode::BAD_GATEWAY, "telegram_unavailable"),
            (StatusCode::TOO_MANY_REQUESTS, "telegram_rate_limited"),
            (StatusCode::BAD_REQUEST, "invalid_bot_token"),
        ] {
            let (status, body) = api_request(
                app.clone(),
                Some(&session),
                Some("http://localhost"),
                "PUT",
                "/api/alerts/token",
                Some(json!({"token":BOT_TOKEN})),
            )
            .await;
            assert_eq!(status, expected_status);
            assert_eq!(body["error"], expected_code);
        }
        server.abort();
    }

    #[tokio::test]
    async fn log_alerts_share_delivery_and_respect_baseline_disable_and_groups() {
        use crate::core_client::{LogAlertReply, LogAlertResult};
        use crate::log_alerts::{LogAlertInput, LogAlertText};

        let directory = tempfile::tempdir().unwrap();
        let (store, metric_alert, _) = configured_store(directory.path(), "normal", 1).await;
        let group = metric_alert.group_ids[0].clone();
        let input = LogAlertInput {
            name: "sudo & cleanup".into(),
            enabled: true,
            query: "service:host sudo".into(),
            filters: BTreeMap::from([("service".into(), vec!["host".into()])]),
            text: Some(LogAlertText {
                query: "sudo".into(),
                mode: "all".into(),
            }),
            threshold: 2,
            cooldown_seconds: 0,
            group_ids: vec![group.clone()],
            message_template: Some("%name%: %count% hits (%query%) %unknown% <tag>".into()),
            message_fields: Some(vec![
                "name".into(),
                "count".into(),
                "query".into(),
                "samples".into(),
            ]),
        };
        let mut invalid = input.clone();
        invalid.filters = BTreeMap::from([("message".into(), vec!["x".into()])]);
        assert!(store.save_log_alert(None, invalid).await.is_err());
        let mut no_words = input.clone();
        no_words.text = Some(LogAlertText {
            query: "!!!".into(),
            mode: "all".into(),
        });
        assert!(store.save_log_alert(None, no_words).await.is_err());

        let id = store.save_log_alert(None, input.clone()).await.unwrap();
        let alerts = store.log_alerts().await.unwrap();
        assert!(!alerts[0].no_recipients);
        let revision = store.alert_global().await.unwrap().revision;
        let url = Url::parse("https://dock.example/").unwrap();
        let reply = |watermark: u64, count: u64| LogAlertReply {
            as_of: "2026-01-01T00:00:00Z".into(),
            watermark,
            results: vec![LogAlertResult {
                id: id.clone(),
                count,
                samples: vec![json!({"timestamp":"2026-01-01T00:00:00Z","server":"mail",
                    "service":"host","level":"info","message":"sudo: eve COMMAND=/tmp/xmrig"})],
            }],
        };

        // Первая проверка только запоминает watermark.
        store
            .commit_log_evaluation(revision, alerts.clone(), reply(10, 5), url.clone())
            .await
            .unwrap();
        assert!(store.next_alert_delivery().await.unwrap().is_none());
        // Ниже порога событий нет.
        store
            .commit_log_evaluation(revision, alerts.clone(), reply(11, 1), url.clone())
            .await
            .unwrap();
        assert!(store.next_alert_delivery().await.unwrap().is_none());
        store
            .commit_log_evaluation(revision, alerts.clone(), reply(13, 2), url.clone())
            .await
            .unwrap();
        let delivery = store.next_alert_delivery().await.unwrap().unwrap();
        assert!(
            delivery
                .message
                .contains("<b>🔴 Алерт на логи сработал</b>")
        );
        assert!(
            delivery
                .message
                .contains("sudo &amp; cleanup: 2 hits (service:host sudo) %unknown% &lt;tag&gt;")
        );
        assert!(delivery.message.contains("sudo: eve COMMAND=/tmp/xmrig"));
        assert_eq!(delivery.parse_mode.as_deref(), Some("HTML"));
        let markup: Value =
            serde_json::from_str(delivery.reply_markup.as_deref().unwrap()).unwrap();
        assert!(
            markup["inline_keyboard"][0][0]["url"]
                .as_str()
                .unwrap()
                .starts_with("https://dock.example/logs?q=service%3Ahost+sudo")
        );

        // Выключенное правило не доставляет уже поставленные события.
        store
            .finish_alert_delivery(delivery, Err(TelegramFailure::Transient))
            .await
            .unwrap();
        store
            .call(|conn| {
                conn.execute("UPDATE metric_alert_deliveries SET next_attempt_at=0", [])?;
                conn.execute("UPDATE log_alerts SET enabled=0", [])
            })
            .await
            .unwrap();
        assert!(store.next_alert_delivery().await.unwrap().is_none());
        store
            .call(|conn| conn.execute("UPDATE log_alerts SET enabled=1", []))
            .await
            .unwrap();
        assert!(store.next_alert_delivery().await.unwrap().is_some());

        // Результат запроса к старой версии правила не применяется.
        store.save_log_alert(Some(id.clone()), input).await.unwrap();
        store
            .commit_log_evaluation(revision, alerts.clone(), reply(20, 9), url.clone())
            .await
            .unwrap();
        let state: Option<i64> = store
            .call(|conn| {
                conn.query_row("SELECT after_sequence FROM log_alert_states", [], |row| {
                    row.get(0)
                })
            })
            .await
            .unwrap();
        assert_eq!(state, None);

        match store.delete_group(group).await {
            Err(AlertStoreError::GroupInUse(_, log_alerts)) => {
                assert_eq!(log_alerts, ["sudo & cleanup"])
            }
            other => panic!("unexpected {other:?}"),
        }
        let settings = store.alert_settings().await.unwrap();
        assert_eq!(settings.groups[0].log_alerts, ["sudo & cleanup"]);
        assert!(store.delete_log_alert(id).await.unwrap());
        assert!(store.log_alerts().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn mcp_log_alert_crud_injects_scope_and_hides_unscoped_or_other_node_rules() {
        use crate::log_alerts::{
            LogAlertInput, LogAlertToolError, mcp_delete_log_alert, mcp_log_alerts,
            mcp_save_log_alert,
        };

        fn input(name: &str, filters: BTreeMap<String, Vec<String>>) -> LogAlertInput {
            LogAlertInput {
                name: name.into(),
                enabled: false,
                query: name.into(),
                filters,
                text: None,
                threshold: 1,
                cooldown_seconds: 0,
                group_ids: vec![],
                message_template: None,
                message_fields: None,
            }
        }

        let core_router = Router::new().route(
            "/v1/admin/agents",
            get(|| async {
                Json(json!({"agents":[{"id":9,"relay_id":4},{"id":10,"relay_id":5}]}))
            }),
        );
        let (core_url, core_server) = serve(core_router).await;
        let directory = tempfile::tempdir().unwrap();
        let app = test_app(directory.path(), &core_url);
        let relay_rule = app
            .store
            .save_log_alert(
                None,
                input(
                    "relay rule",
                    BTreeMap::from([("relay_id".into(), vec!["4".into()])]),
                ),
            )
            .await
            .unwrap();
        let agent_rule = app
            .store
            .save_log_alert(
                None,
                input(
                    "agent rule",
                    BTreeMap::from([("agent_id".into(), vec!["9".into()])]),
                ),
            )
            .await
            .unwrap();
        let other_agent_rule = app
            .store
            .save_log_alert(
                None,
                input(
                    "other agent",
                    BTreeMap::from([("agent_id".into(), vec!["10".into()])]),
                ),
            )
            .await
            .unwrap();
        let global_rule = app
            .store
            .save_log_alert(
                None,
                input(
                    "global rule",
                    BTreeMap::from([("service".into(), vec!["api".into()])]),
                ),
            )
            .await
            .unwrap();
        let mixed_relays = app
            .store
            .save_log_alert(
                None,
                input(
                    "mixed relays",
                    BTreeMap::from([("relay_id".into(), vec!["4".into(), "5".into()])]),
                ),
            )
            .await
            .unwrap();
        let relay_access = TokenAccess {
            username: "alice".into(),
            rights: crate::token_access::Rights {
                logs: true,
                metrics: false,
                alerts_write: true,
            },
            node_scope: Scope::Relay { id: 4 },
        };

        let visible = mcp_log_alerts(&app, &relay_access).await.unwrap();
        assert!(visible.iter().any(|alert| alert.id == relay_rule));
        assert!(visible.iter().any(|alert| alert.id == agent_rule));
        assert!(!visible.iter().any(|alert| alert.id == other_agent_rule));
        assert!(!visible.iter().any(|alert| alert.id == global_rule));
        assert!(!visible.iter().any(|alert| alert.id == mixed_relays));

        let agent_access = TokenAccess {
            node_scope: Scope::Agent { id: 9 },
            ..relay_access.clone()
        };
        let visible = mcp_log_alerts(&app, &agent_access).await.unwrap();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].id, agent_rule);

        let created_agent = mcp_save_log_alert(
            &app,
            &agent_access,
            None,
            input(
                "created by agent",
                BTreeMap::from([("service".into(), vec!["api".into()])]),
            ),
        )
        .await
        .unwrap();
        assert_eq!(
            created_agent.filters.get("agent_id"),
            Some(&vec!["9".into()])
        );
        assert!(matches!(
            mcp_save_log_alert(
                &app,
                &agent_access,
                None,
                input(
                    "agent override",
                    BTreeMap::from([("agent_id".into(), vec!["10".into()])]),
                ),
            )
            .await,
            Err(LogAlertToolError::Invalid(_))
        ));

        let created = mcp_save_log_alert(
            &app,
            &relay_access,
            None,
            input(
                "created by relay",
                BTreeMap::from([("service".into(), vec!["api".into()])]),
            ),
        )
        .await
        .unwrap();
        assert_eq!(created.filters.get("relay_id"), Some(&vec!["4".into()]));

        let invalid_scope = mcp_save_log_alert(
            &app,
            &relay_access,
            None,
            input(
                "override",
                BTreeMap::from([("relay_id".into(), vec!["5".into()])]),
            ),
        )
        .await
        .unwrap_err();
        assert!(matches!(invalid_scope, LogAlertToolError::Invalid(_)));
        let invalid_rule = mcp_save_log_alert(
            &app,
            &relay_access,
            None,
            input("missing filter", BTreeMap::new()),
        )
        .await
        .unwrap_err();
        assert!(matches!(invalid_rule, LogAlertToolError::Invalid(_)));

        let updated = mcp_save_log_alert(
            &app,
            &relay_access,
            Some(agent_rule.clone()),
            input("updated agent rule", BTreeMap::new()),
        )
        .await
        .unwrap();
        assert_eq!(updated.name, "updated agent rule");
        assert_eq!(updated.revision, 2);
        assert_eq!(updated.filters.get("agent_id"), Some(&vec!["9".into()]));
        assert!(!updated.filters.contains_key("relay_id"));

        let all_access = TokenAccess {
            node_scope: Scope::All,
            ..relay_access.clone()
        };
        let updated_relay = mcp_save_log_alert(
            &app,
            &all_access,
            Some(relay_rule.clone()),
            input("updated relay rule", BTreeMap::new()),
        )
        .await
        .unwrap();
        assert_eq!(
            updated_relay.filters.get("relay_id"),
            Some(&vec!["4".into()])
        );
        assert!(!updated_relay.filters.contains_key("agent_id"));
        assert!(matches!(
            mcp_save_log_alert(
                &app,
                &all_access,
                Some(relay_rule.clone()),
                input(
                    "change node restriction",
                    BTreeMap::from([("relay_id".into(), vec!["5".into()])]),
                ),
            )
            .await,
            Err(LogAlertToolError::Invalid(_))
        ));
        assert!(matches!(
            mcp_save_log_alert(
                &app,
                &all_access,
                Some(global_rule.clone()),
                input(
                    "add node restriction",
                    BTreeMap::from([("relay_id".into(), vec!["4".into()])]),
                ),
            )
            .await,
            Err(LogAlertToolError::Invalid(_))
        ));
        assert!(matches!(
            mcp_save_log_alert(
                &app,
                &relay_access,
                Some(global_rule.clone()),
                input("must not update", BTreeMap::new()),
            )
            .await,
            Err(LogAlertToolError::NotFound)
        ));
        assert!(matches!(
            mcp_delete_log_alert(&app, &relay_access, other_agent_rule.clone()).await,
            Err(LogAlertToolError::NotFound)
        ));
        mcp_delete_log_alert(&app, &relay_access, created.id.clone())
            .await
            .unwrap();
        assert!(
            app.store
                .log_alerts()
                .await
                .unwrap()
                .iter()
                .all(|alert| alert.id != created.id)
        );
        core_server.abort();
    }

    #[tokio::test]
    async fn stale_log_alert_revisions_conflict_after_browser_updates() {
        use crate::log_alerts::{LogAlertError, LogAlertInput};

        fn input(name: &str) -> LogAlertInput {
            LogAlertInput {
                name: name.into(),
                enabled: false,
                query: "service:api".into(),
                filters: BTreeMap::from([("service".into(), vec!["api".into()])]),
                text: None,
                threshold: 1,
                cooldown_seconds: 0,
                group_ids: vec![],
                message_template: None,
                message_fields: None,
            }
        }

        async fn global_revision(store: &crate::store::Store) -> i64 {
            store
                .call(|conn| {
                    conn.query_row(
                        "SELECT revision FROM metric_alert_global WHERE singleton=1",
                        [],
                        |row| row.get(0),
                    )
                })
                .await
                .unwrap()
        }

        let directory = tempfile::tempdir().unwrap();
        let app = test_app(directory.path(), "http://127.0.0.1:1");
        let id = app
            .store
            .save_log_alert(None, input("initial"))
            .await
            .unwrap();

        let before_save = app.store.log_alerts().await.unwrap().remove(0);
        app.store
            .save_log_alert(Some(id.clone()), input("browser save"))
            .await
            .unwrap();
        let after_browser_save = app.store.log_alerts().await.unwrap().remove(0);
        let revision_before_stale_save = global_revision(&app.store).await;
        assert!(matches!(
            app.store
                .save_log_alert_checked(id.clone(), input("stale MCP save"), before_save.revision,)
                .await,
            Err(LogAlertError::Conflict("log_alert_changed"))
        ));
        let after_stale_save = app.store.log_alerts().await.unwrap().remove(0);
        assert_eq!(after_stale_save.name, "browser save");
        assert_eq!(after_stale_save.revision, after_browser_save.revision);
        assert_eq!(
            global_revision(&app.store).await,
            revision_before_stale_save
        );

        app.store
            .save_log_alert(Some(id.clone()), input("browser update before delete"))
            .await
            .unwrap();
        let before_browser_delete = app.store.log_alerts().await.unwrap().remove(0);
        app.store
            .save_log_alert(Some(id.clone()), input("browser update"))
            .await
            .unwrap();
        let after_browser_update = app.store.log_alerts().await.unwrap().remove(0);
        let revision_before_stale_delete = global_revision(&app.store).await;
        assert!(matches!(
            app.store
                .delete_log_alert_checked(id.clone(), before_browser_delete.revision)
                .await,
            Err(LogAlertError::Conflict("log_alert_changed"))
        ));
        let after_stale_delete = app.store.log_alerts().await.unwrap().remove(0);
        assert_eq!(after_stale_delete.name, "browser update");
        assert_eq!(after_stale_delete.revision, after_browser_update.revision);
        assert_eq!(
            global_revision(&app.store).await,
            revision_before_stale_delete
        );
    }
}
