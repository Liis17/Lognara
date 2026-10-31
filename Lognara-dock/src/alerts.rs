//! Общие настройки, правила, состояние и durable-доставка алертов метрик.

use std::{
    collections::{BTreeMap, BTreeSet},
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
};

const MAX_ENABLED: i64 = 100;
const MAX_TRIGGERS: usize = 20;
const ALERT_SEND_PASS_LIMIT: usize = 10;
const ALERT_SEND_PASS_BUDGET: Duration = Duration::from_secs(30);
const NO_DATA_LOSS_MS: i64 = 5 * 60 * 1000;
const OUTBOX_TTL_MS: i64 = 24 * 60 * 60 * 1000;
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
    pub enabled: bool,
    pub environment: Option<String>,
    pub no_data: String,
    pub combine: String,
    pub group_ids: Vec<String>,
    pub triggers: Vec<TriggerInput>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AlertDefinition {
    pub id: String,
    pub key: MetricKey,
    pub enabled: bool,
    pub environment: Option<String>,
    pub no_data: String,
    pub combine: String,
    pub group_ids: Vec<String>,
    pub triggers: Vec<TriggerInput>,
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
    GroupInUse(Vec<MetricKey>),
    #[error("group recipients must match its type")]
    GroupKindMismatch,
    #[error("recipient type is used by a group")]
    RecipientKindInUse,
}

fn validate_key(key: &MetricKey) -> Result<(), AlertStoreError> {
    if key.server.is_empty()
        || key.server.len() > 128
        || key.service.is_empty()
        || key.service.len() > 128
        || key.name.is_empty()
        || key.name.len() > 128
        || !matches!(key.origin.as_str(), "counter" | "gauge" | "rule")
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
        .is_some_and(|value| value.len() > 128)
    {
        return Err(AlertStoreError::Invalid("environment is too long"));
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
                    a.enabled_at,s.loss_at,s.last_point_at,s.last_as_of
             FROM metric_alerts a LEFT JOIN metric_alert_states s ON s.alert_id=a.id
             ORDER BY a.server,a.service,a.name,a.origin",
        )?;
        for row in statement.query_map([], |row| {
            let id: String = row.get(0)?;
            let key = MetricKey {
                server: row.get(1)?,
                service: row.get(2)?,
                name: row.get(3)?,
                origin: row.get(4)?,
            };
            Ok((
                id.clone(),
                AlertDefinition {
                    id,
                    key,
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
                    groups.push(Group { id, kind, name, recipient_ids, alerts });
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

    async fn alert_definition(
        &self,
        key: MetricKey,
    ) -> Result<Option<AlertDefinition>, AlertStoreError> {
        validate_key(&key)?;
        self.call(move |conn| {
            Ok(read_alerts(conn)?
                .into_iter()
                .find(|alert| alert.key == key))
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

    async fn save_alert_definition(
        &self,
        input: DefinitionInput,
    ) -> Result<String, AlertStoreError> {
        validate_definition(&input)?;
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
                    return Err(rusqlite::Error::QueryReturnedNoRows);
                }
            }
            let old: Option<(String, i64, i64)> = tx
                .query_row(
                    "SELECT id,revision,enabled FROM metric_alerts
                     WHERE server=?1 AND service=?2 AND name=?3 AND origin=?4",
                    params![input.key.server,input.key.service,input.key.name,input.key.origin],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?;
            let reset_state = old.is_none() || input.enabled;
            if input.enabled
                && !global.enabled
                && old.as_ref().is_none_or(|(_, _, was_enabled)| *was_enabled == 0)
            {
                return Err(rusqlite::Error::InvalidQuery);
            }
            let current_enabled: i64 = tx.query_row(
                "SELECT count(*) FROM metric_alerts WHERE enabled=1",
                [],
                |row| row.get(0),
            )?;
            let next_enabled = current_enabled
                - old.as_ref().filter(|(_, _, enabled)| *enabled != 0).map_or(0, |_| 1)
                + if input.enabled { 1 } else { 0 };
            if next_enabled > MAX_ENABLED {
                tx.commit()?;
                return Err(rusqlite::Error::InvalidQuery);
            }
            let now = now_ms();
            let id = match old {
                Some((id, revision, _)) => {
                    cancel_alert_pending(&tx, &id, now)?;
                    tx.execute(
                        "UPDATE metric_alerts SET enabled=?2,environment=?3,no_data=?4,
                         combine_mode=?5,revision=?6,enabled_at=?7 WHERE id=?1",
                        params![id,if input.enabled { 1 } else { 0 },input.environment,input.no_data,input.combine,revision+1,now],
                    )?;
                    id
                }
                None => {
                    let id = Uuid::new_v4().to_string();
                    tx.execute(
                        "INSERT INTO metric_alerts(id,server,service,name,origin,enabled,environment,
                         no_data,combine_mode,revision,enabled_at)
                         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,1,?10)",
                        params![id,input.key.server,input.key.service,input.key.name,input.key.origin,
                            if input.enabled { 1 } else { 0 },input.environment,input.no_data,input.combine,now],
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
            Ok(id)
        })
        .await
        .map_err(|error| match error {
            StoreError::Sqlite(rusqlite::Error::InvalidQuery) => {
                AlertStoreError::Conflict("global_disabled_or_alert_limit")
            }
            StoreError::Sqlite(rusqlite::Error::QueryReturnedNoRows) => {
                AlertStoreError::Conflict("group_not_found")
            }
            error => AlertStoreError::Store(error),
        })
    }

    async fn delete_alert_definition(&self, key: MetricKey) -> Result<bool, AlertStoreError> {
        validate_key(&key)?;
        self.call(move |conn| {
            let tx = conn.transaction()?;
            let changed = tx.execute(
                "DELETE FROM metric_alerts WHERE server=?1 AND service=?2 AND name=?3 AND origin=?4",
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
                tx.commit()?;
                Ok(Group { id, kind: input.kind, name: input.name, recipient_ids: input.recipient_ids, alerts })
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
            if !alerts.is_empty() {
                tx.commit()?;
                return Ok(Err(alerts));
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
        .map_err(AlertStoreError::GroupInUse)
    }
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

fn finish_outbox(conn: &Connection, now: i64) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE metric_alert_outbox SET finished_at=?1 WHERE finished_at IS NULL
         AND NOT EXISTS(SELECT 1 FROM metric_alert_deliveries d
                       WHERE d.event_id=metric_alert_outbox.id AND d.status='pending')",
        [now],
    )?;
    Ok(())
}

impl From<AlertStoreError> for ApiError {
    fn from(error: AlertStoreError) -> Self {
        match error {
            AlertStoreError::Store(error) => error.into(),
            AlertStoreError::Invalid(message) => ApiError::Invalid(message.to_owned()),
            AlertStoreError::Conflict(code) => ApiError::Conflict(code),
            AlertStoreError::NotFound => ApiError::NotFound,
            AlertStoreError::GroupInUse(alerts) => ApiError::GroupInUse(alerts),
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
        Err(AlertStoreError::GroupInUse(alerts)) => (
            StatusCode::CONFLICT,
            Json(json!({"error":"group_in_use","alerts":alerts})),
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
        let windows: Vec<u64> = alert
            .triggers
            .iter()
            .map(|trigger| trigger.window_seconds)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let query = AlertSeriesQuery {
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
        };
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
            let row: Option<(String, String, i64, String, String, i64)> = tx.query_row(
                "SELECT d.event_id,d.recipient_id,d.recipient_chat_id,o.message,g.telegram_token,d.attempts
                 FROM metric_alert_deliveries d
                 JOIN metric_alert_recipients r ON r.id=d.recipient_id AND r.chat_id=d.recipient_chat_id
                 JOIN metric_alert_outbox o ON o.id=d.event_id
                 JOIN metric_alert_global g ON g.singleton=1
                 WHERE d.status='pending' AND d.next_attempt_at<=?1 AND o.expires_at>?1
                   AND (o.alert_id IS NULL OR EXISTS(
                       SELECT 1 FROM metric_alerts a WHERE a.id=o.alert_id AND a.enabled=1
                         AND a.revision=o.alert_revision))
                   AND NOT EXISTS(
                     SELECT 1 FROM metric_alert_deliveries earlier
                     JOIN metric_alert_outbox eo ON eo.id=earlier.event_id
                     WHERE earlier.recipient_id=d.recipient_id
                       AND COALESCE(eo.alert_id,'')=COALESCE(o.alert_id,'')
                       AND earlier.status IN ('pending','sending')
                       AND (eo.created_at<o.created_at OR (eo.created_at=o.created_at AND eo.id<o.id)))
                 ORDER BY o.created_at,o.id,d.recipient_id LIMIT 1",
                [now],
                |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?)),
            ).optional()?;
            let Some((event_id, recipient_id, chat_id, message, token, attempts)) = row else {
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

async fn send_pending(app: &AppState) {
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
                app.telegram.send(&delivery.token, delivery.chat_id, &delivery.message),
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
    message: &str,
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
        "INSERT INTO metric_alert_outbox(id,alert_id,alert_revision,event_kind,message,created_at,expires_at)
         VALUES(?1,?2,?3,?4,?5,?6,?7)",
        params![event_id,alert.id,alert.revision,kind,message,created_at,created_at+OUTBOX_TTL_MS],
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
        "SELECT max(created_at) FROM metric_alert_outbox WHERE alert_id IS NULL",
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
         WHERE a.enabled=1 ORDER BY r.id",
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

fn insert_deliveries(
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

fn finish_single_outbox(
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

fn parse_timestamp_ms(value: &str) -> Option<i64> {
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

fn alert_message(
    alert: &AlertDefinition,
    kind: &str,
    windows: &BTreeMap<u64, &AlertSeriesWindow>,
    as_of: &str,
    last_point: &Option<String>,
    public_url: &Url,
) -> String {
    let title = match kind {
        "firing" => "Алерт метрики сработал",
        "recovered" => "Алерт метрики восстановился",
        "data_loss" => "Потеряны новые точки метрики",
        "data_recovered" => "Точки метрики снова поступают",
        _ => "Состояние алерта метрики изменилось",
    };
    let mut body = format!(
        "{title}\nМетрика: {} / {} / {} ({})\nОкружение: {}\nВремя Core: {}\nПоследняя точка: {}\nУсловие: {}\n",
        alert.key.server,
        alert.key.service,
        alert.key.name,
        alert.key.origin,
        alert.environment.as_deref().unwrap_or("все"),
        as_of,
        last_point.as_deref().unwrap_or("нет данных"),
        alert.combine.to_uppercase(),
    );
    for trigger in &alert.triggers {
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
        body.push_str(&format!(
            "{} за {} сек {} {}: {}\n",
            trigger.aggregate,
            trigger.window_seconds,
            trigger.comparator,
            trigger.threshold,
            value.map_or_else(|| String::from("нет значения"), |value| value.to_string()),
        ));
    }
    append_message_link(body, metric_link(public_url, alert))
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

fn settings_link(public_url: &Url) -> String {
    public_url
        .join("settings#alerts")
        .map_or_else(|_| "/settings#alerts".into(), |url| url.to_string())
}

fn append_message_link(mut body: String, link: String) -> String {
    let suffix = format!("\nОткрыть: {link}");
    let budget = 4096usize.saturating_sub(suffix.chars().count());
    if body.chars().count() > budget {
        body = body
            .chars()
            .take(budget.saturating_sub(2))
            .collect::<String>();
        body.push_str("…\n");
    }
    body.push_str(&suffix);
    body
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

    async fn send(&self, token: &str, chat_id: i64, text: &str) -> Result<(), TelegramFailure> {
        let url = self.endpoint(token, "sendMessage")?;
        let response = self
            .http
            .post(url)
            .json(&json!({"chat_id": chat_id, "text": text}))
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
        if status == StatusCode::BAD_REQUEST || status == StatusCode::FORBIDDEN {
            return Err(TelegramFailure::PermanentRecipient);
        }
        Err(TelegramFailure::Transient)
    }
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

    fn input(key: MetricKey, no_data: &str, group_ids: Vec<String>) -> DefinitionInput {
        DefinitionInput {
            key,
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
        }
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
            Err(AlertStoreError::GroupInUse(_))
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
                json!({"ok":false,"description":"private details"}),
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
            .send(BOT_TOKEN, 1001, "plain text")
            .await
            .unwrap_err();
        assert!(
            matches!(rate, TelegramFailure::RateLimited(wait) if wait == Duration::from_secs(7))
        );
        let permanent = client
            .send(BOT_TOKEN, 1002, "plain text")
            .await
            .unwrap_err();
        assert!(matches!(permanent, TelegramFailure::PermanentRecipient));
        let invalid = client
            .send(BOT_TOKEN, 1003, "plain text")
            .await
            .unwrap_err();
        assert!(matches!(invalid, TelegramFailure::InvalidToken));
        let transient = client
            .send(BOT_TOKEN, 1004, "plain text")
            .await
            .unwrap_err();
        assert!(matches!(transient, TelegramFailure::Transient));
        assert!(!format!("{invalid:?}").contains(BOT_TOKEN));
        assert!(!format!("{transient:?}").contains("do not log this"));
        let requests = mock.requests.lock().unwrap();
        assert_eq!(requests.len(), 8);
        assert_eq!(requests[4].1["text"], "plain text");
        assert_eq!(requests[4].1["chat_id"], 1001);
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
}
