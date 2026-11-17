//! SQLite в каталоге данных: сессии, passkey, скрытые паттерны, статусы проблем и избранное метрик.
//! Каталог блокируется одним процессом.

use std::fs::{self, File};
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use rusqlite::{Connection, OptionalExtension};

use crate::token_access::{Permissions, Rights, Scope, TokenAccess, is_granted_scope};

const MIGRATIONS: &[&str] = &[
    "
    CREATE TABLE sessions (
        token_hash BLOB PRIMARY KEY,
        username TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        last_seen INTEGER NOT NULL
    );
    CREATE INDEX sessions_username ON sessions(username);
",
    "
    CREATE TABLE users (
        username TEXT PRIMARY KEY,
        webauthn_id BLOB NOT NULL UNIQUE
    );
    CREATE TABLE passkeys (
        id INTEGER PRIMARY KEY,
        username TEXT NOT NULL REFERENCES users(username) ON DELETE CASCADE,
        credential_id BLOB NOT NULL UNIQUE,
        name TEXT NOT NULL,
        passkey TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        last_used_at INTEGER
    );
    CREATE INDEX passkeys_username ON passkeys(username);
",
    "
    CREATE TABLE hidden_patterns (
        username TEXT NOT NULL,
        pattern_id TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        PRIMARY KEY (username, pattern_id)
    );
",
    "
    CREATE TABLE issue_states (
        fingerprint TEXT PRIMARY KEY,
        status TEXT NOT NULL CHECK (status IN ('open', 'resolved', 'regressed')),
        resolved_at INTEGER,
        resolved_by TEXT,
        resolved_checkpoint INTEGER,
        updated_at INTEGER NOT NULL
    );
    CREATE TABLE issue_history (
        id INTEGER PRIMARY KEY,
        fingerprint TEXT NOT NULL,
        action TEXT NOT NULL CHECK (action IN ('resolved', 'reopened', 'regressed')),
        actor TEXT,
        at INTEGER NOT NULL,
        event_at TEXT
    );
    CREATE INDEX issue_history_fingerprint ON issue_history(fingerprint, id);
    CREATE TABLE issue_watch (
        singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
        checkpoint INTEGER NOT NULL
    );
",
    "CREATE TABLE favorite_metrics (
        server TEXT NOT NULL, service TEXT NOT NULL, name TEXT NOT NULL, origin TEXT NOT NULL,
        PRIMARY KEY(server, service, name, origin)
    );",
    "
    CREATE TABLE mcp_personal_tokens (
        id TEXT PRIMARY KEY,
        username TEXT NOT NULL,
        name TEXT NOT NULL,
        token_hash BLOB NOT NULL UNIQUE,
        created_at INTEGER NOT NULL,
        expires_at INTEGER NOT NULL,
        last_used_at INTEGER,
        revoked_at INTEGER
    );
    CREATE INDEX mcp_personal_tokens_user ON mcp_personal_tokens(username, created_at);
    CREATE TABLE oauth_pending (
        id TEXT PRIMARY KEY,
        username TEXT NOT NULL,
        client_id TEXT NOT NULL,
        client_name TEXT NOT NULL,
        redirect_uri TEXT NOT NULL,
        state TEXT NOT NULL,
        scope TEXT NOT NULL,
        resource TEXT NOT NULL,
        code_challenge TEXT NOT NULL,
        expires_at INTEGER NOT NULL
    );
    CREATE TABLE oauth_codes (
        code_hash BLOB PRIMARY KEY,
        username TEXT NOT NULL,
        client_id TEXT NOT NULL,
        client_name TEXT NOT NULL,
        redirect_uri TEXT NOT NULL,
        scope TEXT NOT NULL,
        resource TEXT NOT NULL,
        code_challenge TEXT NOT NULL,
        expires_at INTEGER NOT NULL,
        used_at INTEGER
    );
    CREATE TABLE oauth_grants (
        id TEXT PRIMARY KEY,
        username TEXT NOT NULL,
        client_id TEXT NOT NULL,
        client_name TEXT NOT NULL,
        scope TEXT NOT NULL,
        resource TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        last_used_at INTEGER,
        revoked_at INTEGER
    );
    CREATE INDEX oauth_grants_user ON oauth_grants(username, created_at);
    CREATE TABLE oauth_tokens (
        token_hash BLOB PRIMARY KEY,
        grant_id TEXT NOT NULL REFERENCES oauth_grants(id) ON DELETE CASCADE,
        kind TEXT NOT NULL CHECK(kind IN ('access', 'refresh')),
        expires_at INTEGER NOT NULL,
        created_at INTEGER NOT NULL,
        used_at INTEGER,
        last_used_at INTEGER,
        revoked_at INTEGER
    );
    CREATE INDEX oauth_tokens_grant ON oauth_tokens(grant_id, kind);
    ",
    "
    CREATE TABLE metric_alert_global (
        singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
        enabled INTEGER NOT NULL DEFAULT 0 CHECK(enabled IN (0, 1)),
        telegram_token TEXT,
        send_paused INTEGER NOT NULL DEFAULT 0 CHECK(send_paused IN (0, 1)),
        send_pause_reason TEXT,
        throttle_until INTEGER NOT NULL DEFAULT 0,
        revision INTEGER NOT NULL DEFAULT 0,
        core_status TEXT NOT NULL DEFAULT 'unknown',
        core_checked_at INTEGER,
        core_error TEXT,
        core_loss_registered INTEGER NOT NULL DEFAULT 0 CHECK(core_loss_registered IN (0, 1)),
        core_loss_event_id TEXT
    );
    INSERT INTO metric_alert_global(singleton) VALUES(1);
    CREATE TABLE metric_alert_recipients (
        id TEXT PRIMARY KEY,
        kind TEXT NOT NULL CHECK(kind IN ('personal', 'channel')),
        name TEXT NOT NULL,
        chat_id INTEGER NOT NULL UNIQUE,
        last_error TEXT,
        created_at INTEGER NOT NULL
    );
    CREATE TABLE metric_alert_groups (
        id TEXT PRIMARY KEY,
        kind TEXT NOT NULL CHECK(kind IN ('personal', 'channel')),
        name TEXT NOT NULL,
        created_at INTEGER NOT NULL
    );
    CREATE TABLE metric_alert_group_recipients (
        group_id TEXT NOT NULL REFERENCES metric_alert_groups(id) ON DELETE CASCADE,
        recipient_id TEXT NOT NULL REFERENCES metric_alert_recipients(id) ON DELETE CASCADE,
        PRIMARY KEY(group_id, recipient_id)
    );
    CREATE TABLE metric_alerts (
        id TEXT PRIMARY KEY,
        server TEXT NOT NULL,
        service TEXT NOT NULL,
        name TEXT NOT NULL,
        origin TEXT NOT NULL CHECK(origin IN ('counter', 'gauge', 'rule')),
        enabled INTEGER NOT NULL CHECK(enabled IN (0, 1)),
        environment TEXT,
        no_data TEXT NOT NULL CHECK(no_data IN ('normal', 'loss')),
        combine_mode TEXT NOT NULL CHECK(combine_mode IN ('and', 'or')),
        revision INTEGER NOT NULL,
        enabled_at INTEGER NOT NULL,
        UNIQUE(server, service, name, origin)
    );
    CREATE INDEX metric_alerts_enabled ON metric_alerts(enabled, server, service, name, origin);
    CREATE TABLE metric_alert_triggers (
        alert_id TEXT NOT NULL REFERENCES metric_alerts(id) ON DELETE CASCADE,
        position INTEGER NOT NULL,
        window_seconds INTEGER NOT NULL CHECK(window_seconds BETWEEN 60 AND 604800),
        aggregate TEXT NOT NULL CHECK(aggregate IN ('avg', 'min', 'max', 'sum', 'count', 'increase')),
        comparator TEXT NOT NULL CHECK(comparator IN ('>', '<')),
        threshold REAL NOT NULL,
        PRIMARY KEY(alert_id, position)
    );
    CREATE TABLE metric_alert_groups_used (
        alert_id TEXT NOT NULL REFERENCES metric_alerts(id) ON DELETE CASCADE,
        group_id TEXT NOT NULL REFERENCES metric_alert_groups(id),
        PRIMARY KEY(alert_id, group_id)
    );
    CREATE TABLE metric_alert_states (
        alert_id TEXT PRIMARY KEY REFERENCES metric_alerts(id) ON DELETE CASCADE,
        condition_state TEXT NOT NULL DEFAULT 'unknown' CHECK(condition_state IN ('unknown', 'ok', 'firing')),
        loss_registered INTEGER NOT NULL DEFAULT 0 CHECK(loss_registered IN (0, 1)),
        loss_at INTEGER,
        last_point_at INTEGER,
        last_as_of INTEGER
    );
    CREATE TABLE metric_alert_outbox (
        id TEXT PRIMARY KEY,
        alert_id TEXT REFERENCES metric_alerts(id) ON DELETE CASCADE,
        alert_revision INTEGER,
        event_kind TEXT NOT NULL,
        message TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        expires_at INTEGER NOT NULL,
        finished_at INTEGER
    );
    CREATE INDEX metric_alert_outbox_created ON metric_alert_outbox(created_at, id);
    CREATE TABLE metric_alert_deliveries (
        event_id TEXT NOT NULL REFERENCES metric_alert_outbox(id) ON DELETE CASCADE,
        recipient_id TEXT NOT NULL,
        recipient_name TEXT NOT NULL,
        recipient_chat_id INTEGER NOT NULL,
        status TEXT NOT NULL CHECK(status IN ('pending', 'sending', 'sent', 'permanent_error', 'expired', 'cancelled')),
        attempts INTEGER NOT NULL DEFAULT 0,
        next_attempt_at INTEGER NOT NULL,
        last_error TEXT,
        sent_at INTEGER,
        PRIMARY KEY(event_id, recipient_id)
    );
    CREATE INDEX metric_alert_deliveries_pending ON metric_alert_deliveries(status, next_attempt_at, event_id);
    ",
    // Существующие токены и гранты получают прежние права: логи и метрики, без записи, без скоупа.
    "
    ALTER TABLE mcp_personal_tokens ADD COLUMN logs INTEGER NOT NULL DEFAULT 1 CHECK(logs IN (0, 1));
    ALTER TABLE mcp_personal_tokens ADD COLUMN metrics INTEGER NOT NULL DEFAULT 1 CHECK(metrics IN (0, 1));
    ALTER TABLE mcp_personal_tokens ADD COLUMN alerts_write INTEGER NOT NULL DEFAULT 0 CHECK(alerts_write IN (0, 1));
    ALTER TABLE mcp_personal_tokens ADD COLUMN scope_kind TEXT NOT NULL DEFAULT 'all' CHECK(scope_kind IN ('all', 'relay', 'agent'));
    ALTER TABLE mcp_personal_tokens ADD COLUMN scope_id INTEGER NOT NULL DEFAULT 0 CHECK(scope_id >= 0);
    ALTER TABLE oauth_grants ADD COLUMN logs INTEGER NOT NULL DEFAULT 1 CHECK(logs IN (0, 1));
    ALTER TABLE oauth_grants ADD COLUMN metrics INTEGER NOT NULL DEFAULT 1 CHECK(metrics IN (0, 1));
    ALTER TABLE oauth_grants ADD COLUMN alerts_write INTEGER NOT NULL DEFAULT 0 CHECK(alerts_write IN (0, 1));
    ALTER TABLE oauth_grants ADD COLUMN scope_kind TEXT NOT NULL DEFAULT 'all' CHECK(scope_kind IN ('all', 'relay', 'agent'));
    ALTER TABLE oauth_grants ADD COLUMN scope_id INTEGER NOT NULL DEFAULT 0 CHECK(scope_id >= 0);
    ALTER TABLE oauth_codes ADD COLUMN logs INTEGER NOT NULL DEFAULT 1 CHECK(logs IN (0, 1));
    ALTER TABLE oauth_codes ADD COLUMN metrics INTEGER NOT NULL DEFAULT 1 CHECK(metrics IN (0, 1));
    ALTER TABLE oauth_codes ADD COLUMN alerts_write INTEGER NOT NULL DEFAULT 0 CHECK(alerts_write IN (0, 1));
    ALTER TABLE oauth_codes ADD COLUMN scope_kind TEXT NOT NULL DEFAULT 'all' CHECK(scope_kind IN ('all', 'relay', 'agent'));
    ALTER TABLE oauth_codes ADD COLUMN scope_id INTEGER NOT NULL DEFAULT 0 CHECK(scope_id >= 0);
    ",
    // Пересборка без потери дочерних строк: выполняется с выключенными внешними ключами.
    // UNIQUE учитывает скоуп; NULL здесь не используется, поэтому глобальные правила уникальны.
    "
    CREATE TABLE metric_alerts_scoped (
        id TEXT PRIMARY KEY,
        server TEXT NOT NULL,
        service TEXT NOT NULL,
        name TEXT NOT NULL,
        origin TEXT NOT NULL CHECK(origin IN ('counter', 'gauge', 'rule')),
        enabled INTEGER NOT NULL CHECK(enabled IN (0, 1)),
        environment TEXT,
        no_data TEXT NOT NULL CHECK(no_data IN ('normal', 'loss')),
        combine_mode TEXT NOT NULL CHECK(combine_mode IN ('and', 'or')),
        revision INTEGER NOT NULL,
        enabled_at INTEGER NOT NULL,
        scope_kind TEXT NOT NULL DEFAULT 'all' CHECK(scope_kind IN ('all', 'relay', 'agent')),
        scope_id INTEGER NOT NULL DEFAULT 0 CHECK(scope_id >= 0),
        UNIQUE(server, service, name, origin, scope_kind, scope_id)
    );
    INSERT INTO metric_alerts_scoped(id,server,service,name,origin,enabled,environment,no_data,combine_mode,revision,enabled_at)
        SELECT id,server,service,name,origin,enabled,environment,no_data,combine_mode,revision,enabled_at FROM metric_alerts;
    DROP TABLE metric_alerts;
    ALTER TABLE metric_alerts_scoped RENAME TO metric_alerts;
    CREATE INDEX metric_alerts_enabled ON metric_alerts(enabled, server, service, name, origin);
    ",
    // Алерты на логи: общие получатели, группы, outbox и Telegram с алертами метрик.
    "
    CREATE TABLE log_alerts (
        id TEXT PRIMARY KEY,
        name TEXT NOT NULL,
        enabled INTEGER NOT NULL CHECK(enabled IN (0, 1)),
        query TEXT NOT NULL,
        filters TEXT NOT NULL,
        text TEXT,
        text_mode TEXT NOT NULL CHECK(text_mode IN ('all', 'phrase')),
        threshold INTEGER NOT NULL CHECK(threshold BETWEEN 1 AND 1000000),
        cooldown_seconds INTEGER NOT NULL CHECK(cooldown_seconds BETWEEN 0 AND 86400),
        revision INTEGER NOT NULL,
        created_at INTEGER NOT NULL
    );
    CREATE TABLE log_alert_groups_used (
        alert_id TEXT NOT NULL REFERENCES log_alerts(id) ON DELETE CASCADE,
        group_id TEXT NOT NULL REFERENCES metric_alert_groups(id),
        PRIMARY KEY(alert_id, group_id)
    );
    CREATE TABLE log_alert_states (
        alert_id TEXT PRIMARY KEY REFERENCES log_alerts(id) ON DELETE CASCADE,
        after_sequence INTEGER,
        suppressed INTEGER NOT NULL DEFAULT 0,
        last_sent_at INTEGER,
        last_count INTEGER NOT NULL DEFAULT 0,
        last_match_at INTEGER,
        last_checked_at INTEGER
    );
    ALTER TABLE metric_alert_outbox ADD COLUMN log_alert_id TEXT REFERENCES log_alerts(id) ON DELETE CASCADE;
    ALTER TABLE metric_alert_global ADD COLUMN log_core_status TEXT NOT NULL DEFAULT 'unknown';
    ",
    // Параметры текста и формат уже подготовленных уведомлений.
    "
    ALTER TABLE metric_alerts ADD COLUMN title TEXT;
    ALTER TABLE metric_alerts ADD COLUMN description TEXT;
    ALTER TABLE metric_alerts ADD COLUMN message_template TEXT;
    ALTER TABLE metric_alerts ADD COLUMN message_fields TEXT NOT NULL DEFAULT '[\"name\",\"description\",\"service\",\"server\",\"origin\",\"node\",\"environment\",\"condition\",\"value\",\"time\",\"last_point\",\"status\"]';
    ALTER TABLE log_alerts ADD COLUMN message_template TEXT;
    ALTER TABLE log_alerts ADD COLUMN message_fields TEXT NOT NULL DEFAULT '[\"name\",\"count\",\"query\",\"samples\",\"time\",\"status\"]';
    ALTER TABLE metric_alert_outbox ADD COLUMN parse_mode TEXT;
    ALTER TABLE metric_alert_outbox ADD COLUMN reply_markup TEXT;
    ",
];

/// Предел строк статусов: Dock передаёт решённые и вернувшиеся fingerprint в core.
pub const MAX_ISSUE_STATES: i64 = 10_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueState {
    pub status: String,
    pub resolved_at: Option<i64>,
    pub resolved_by: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueAction {
    pub action: String,
    pub actor: Option<String>,
    pub at: i64,
    pub event_at: Option<String>,
}

/// Совпадение после checkpoint: fingerprint, время первого случая в мс и RFC3339.
pub struct Recurrence {
    pub fingerprint: String,
    pub first_ms: i64,
    pub first_seen: String,
}

/// `expires_at` бессрочного персонального MCP-токена: проверка `expires_at>now` всегда проходит.
pub const NEVER_EXPIRES: i64 = i64::MAX;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PersonalTokenInfo {
    pub id: String,
    pub name: String,
    pub created_at: i64,
    /// `None` у бессрочного токена.
    pub expires_at: Option<i64>,
    pub last_used_at: Option<i64>,
    #[serde(flatten)]
    pub permissions: Permissions,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct OAuthGrantInfo {
    pub id: String,
    pub client_name: String,
    pub scope: String,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
    #[serde(flatten)]
    pub permissions: Permissions,
}

#[derive(Debug, Clone)]
pub struct PendingAuthorization {
    pub id: String,
    pub username: String,
    pub client_id: String,
    pub client_name: String,
    pub redirect_uri: String,
    pub state: String,
    pub scope: String,
    pub resource: String,
    pub code_challenge: String,
    pub expires_at: i64,
}

#[derive(Debug, Clone)]
pub struct IssuedOAuthTokens {
    pub username: String,
    pub grant_id: String,
    pub scope: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshOutcome {
    Issued,
    Replay,
    Invalid,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("database task failed")]
    Task,
}

#[derive(Clone)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
    _lock: Arc<File>,
}

impl Store {
    pub async fn favorite_metrics(&self) -> Result<Vec<crate::series::MetricKey>, StoreError> {
        self.call(|conn| {
            conn.prepare("SELECT server,service,name,origin FROM favorite_metrics ORDER BY server,service,name,origin")?
                .query_map([], |r| Ok(crate::series::MetricKey { server:r.get(0)?, service:r.get(1)?, name:r.get(2)?, origin:r.get(3)? }))?.collect()
        }).await
    }
    pub async fn set_favorite_metric(
        &self,
        key: crate::series::MetricKey,
        favorite: bool,
    ) -> Result<(), StoreError> {
        self.call(move |conn| {
            let sql = if favorite { "INSERT OR IGNORE INTO favorite_metrics VALUES(?1,?2,?3,?4)" }
                else { "DELETE FROM favorite_metrics WHERE server=?1 AND service=?2 AND name=?3 AND origin=?4" };
            conn.execute(sql, rusqlite::params![key.server,key.service,key.name,key.origin])?;
            Ok(())
        }).await
    }
    pub async fn hidden_patterns(&self, username: String) -> Result<Vec<String>, StoreError> {
        self.call(move |conn| {
            let mut statement = conn.prepare(
                "SELECT pattern_id FROM hidden_patterns WHERE username = ?1 ORDER BY pattern_id",
            )?;
            statement.query_map([username], |row| row.get(0))?.collect()
        })
        .await
    }

    /// Проверка лимита и запись атомарны; повторное скрытие остаётся успешным.
    pub async fn hide_pattern(
        &self,
        username: String,
        pattern_id: String,
    ) -> Result<bool, StoreError> {
        self.call(move |conn| {
            let tx = conn.transaction()?;
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM hidden_patterns WHERE username = ?1 AND pattern_id = ?2)",
                (&username, &pattern_id), |row| row.get(0),
            )?;
            if exists {
                return Ok(true);
            }
            let count: i64 = tx.query_row(
                "SELECT count(*) FROM hidden_patterns WHERE username = ?1",
                [&username], |row| row.get(0),
            )?;
            if count >= 1000 {
                return Ok(false);
            }
            tx.execute(
                "INSERT INTO hidden_patterns(username, pattern_id, created_at) VALUES (?1, ?2, ?3)",
                (&username, &pattern_id, now_ms()),
            )?;
            tx.commit()?;
            Ok(true)
        })
        .await
    }

    pub async fn show_pattern(
        &self,
        username: String,
        pattern_id: String,
    ) -> Result<(), StoreError> {
        self.call(move |conn| {
            conn.execute(
                "DELETE FROM hidden_patterns WHERE username = ?1 AND pattern_id = ?2",
                (username, pattern_id),
            )?;
            Ok(())
        })
        .await
    }

    pub async fn issue_states(
        &self,
    ) -> Result<std::collections::HashMap<String, IssueState>, StoreError> {
        self.call(|conn| {
            let mut statement = conn.prepare(
                "SELECT fingerprint, status, resolved_at, resolved_by FROM issue_states",
            )?;
            statement
                .query_map([], |row| {
                    Ok((
                        row.get(0)?,
                        IssueState {
                            status: row.get(1)?,
                            resolved_at: row.get(2)?,
                            resolved_by: row.get(3)?,
                        },
                    ))
                })?
                .collect()
        })
        .await
    }

    /// Статус и последние 50 действий, новые первыми.
    pub async fn issue(
        &self,
        fingerprint: String,
    ) -> Result<(Option<IssueState>, Vec<IssueAction>), StoreError> {
        self.call(move |conn| {
            let state = conn
                .query_row(
                    "SELECT status, resolved_at, resolved_by FROM issue_states WHERE fingerprint = ?1",
                    [&fingerprint],
                    |row| {
                        Ok(IssueState {
                            status: row.get(0)?,
                            resolved_at: row.get(1)?,
                            resolved_by: row.get(2)?,
                        })
                    },
                )
                .optional()?;
            let mut statement = conn.prepare(
                "SELECT action, actor, at, event_at FROM issue_history WHERE fingerprint = ?1 ORDER BY id DESC LIMIT 50",
            )?;
            let history = statement
                .query_map([&fingerprint], |row| {
                    Ok(IssueAction {
                        action: row.get(0)?,
                        actor: row.get(1)?,
                        at: row.get(2)?,
                        event_at: row.get(3)?,
                    })
                })?
                .collect::<rusqlite::Result<_>>()?;
            Ok((state, history))
        })
        .await
    }

    /// Возврат учитывает только публикации после checkpoint наблюдателя на момент решения.
    /// Повторное решение уже решённой проблемы ничего не меняет; false — лимит строк.
    pub async fn resolve_issue(
        &self,
        fingerprint: String,
        username: String,
    ) -> Result<bool, StoreError> {
        self.call(move |conn| {
            let tx = conn.transaction()?;
            let status: Option<String> = tx
                .query_row(
                    "SELECT status FROM issue_states WHERE fingerprint = ?1",
                    [&fingerprint],
                    |row| row.get(0),
                )
                .optional()?;
            match status.as_deref() {
                Some("resolved") => return Ok(true),
                None => {
                    let count: i64 =
                        tx.query_row("SELECT count(*) FROM issue_states", [], |row| row.get(0))?;
                    if count >= MAX_ISSUE_STATES {
                        return Ok(false);
                    }
                }
                Some(_) => {}
            }
            let checkpoint: i64 = tx
                .query_row("SELECT checkpoint FROM issue_watch", [], |row| row.get(0))
                .optional()?
                .unwrap_or(0);
            let now = now_ms();
            tx.execute(
                "INSERT INTO issue_states(fingerprint, status, resolved_at, resolved_by, resolved_checkpoint, updated_at)
                 VALUES (?1, 'resolved', ?2, ?3, ?4, ?2)
                 ON CONFLICT(fingerprint) DO UPDATE SET status = 'resolved', resolved_at = ?2,
                 resolved_by = ?3, resolved_checkpoint = ?4, updated_at = ?2",
                (&fingerprint, now, &username, checkpoint),
            )?;
            tx.execute(
                "INSERT INTO issue_history(fingerprint, action, actor, at) VALUES (?1, 'resolved', ?2, ?3)",
                (&fingerprint, &username, now),
            )?;
            tx.commit()?;
            Ok(true)
        })
        .await
    }

    /// Открывает решённую проблему; для остальных статусов ничего не меняет.
    pub async fn reopen_issue(
        &self,
        fingerprint: String,
        username: String,
    ) -> Result<(), StoreError> {
        self.call(move |conn| {
            let tx = conn.transaction()?;
            let now = now_ms();
            let changed = tx.execute(
                "UPDATE issue_states SET status = 'open', resolved_at = NULL, resolved_by = NULL,
                 resolved_checkpoint = NULL, updated_at = ?2 WHERE fingerprint = ?1 AND status = 'resolved'",
                (&fingerprint, now),
            )?;
            if changed > 0 {
                tx.execute(
                    "INSERT INTO issue_history(fingerprint, action, actor, at) VALUES (?1, 'reopened', ?2, ?3)",
                    (&fingerprint, &username, now),
                )?;
            }
            tx.commit()
        })
        .await
    }

    /// Checkpoint наблюдателя и решённые проблемы с моментом решения в мс.
    pub async fn issue_watch(&self) -> Result<(Option<u64>, Vec<(String, i64)>), StoreError> {
        self.call(|conn| {
            let checkpoint: Option<i64> = conn
                .query_row("SELECT checkpoint FROM issue_watch", [], |row| row.get(0))
                .optional()?;
            let mut statement = conn.prepare(
                "SELECT fingerprint, resolved_at FROM issue_states WHERE status = 'resolved' ORDER BY fingerprint",
            )?;
            let resolved = statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            Ok((checkpoint.map(|checkpoint| checkpoint as u64), resolved))
        })
        .await
    }

    /// Переводит решённые в «возвращается» и сдвигает checkpoint одной транзакцией.
    /// Проблема, решённая позже `after` или позже случая, не меняется.
    pub async fn record_recurrences(
        &self,
        after: u64,
        watermark: u64,
        found: Vec<Recurrence>,
    ) -> Result<Vec<String>, StoreError> {
        self.call(move |conn| {
            let tx = conn.transaction()?;
            let now = now_ms();
            let mut regressed = vec![];
            for item in found {
                let changed = tx.execute(
                    "UPDATE issue_states SET status = 'regressed', updated_at = ?4
                     WHERE fingerprint = ?1 AND status = 'resolved'
                     AND resolved_checkpoint <= ?2 AND resolved_at <= ?3",
                    (&item.fingerprint, after as i64, item.first_ms, now),
                )?;
                if changed > 0 {
                    tx.execute(
                        "INSERT INTO issue_history(fingerprint, action, at, event_at) VALUES (?1, 'regressed', ?2, ?3)",
                        (&item.fingerprint, now, &item.first_seen),
                    )?;
                    regressed.push(item.fingerprint);
                }
            }
            if watermark < after {
                // Core начал хранилище заново: прежние sequence больше не сравнимы.
                tx.execute(
                    "UPDATE issue_states SET resolved_checkpoint = ?1 WHERE resolved_checkpoint > ?1",
                    [watermark as i64],
                )?;
            }
            tx.execute(
                "INSERT INTO issue_watch(singleton, checkpoint) VALUES (1, ?1)
                 ON CONFLICT(singleton) DO UPDATE SET checkpoint = ?1",
                [watermark as i64],
            )?;
            tx.commit()?;
            Ok(regressed)
        })
        .await
    }

    pub async fn create_personal_token(
        &self,
        id: String,
        username: String,
        name: String,
        token_hash: [u8; 32],
        expires_at: i64,
        permissions: Permissions,
    ) -> Result<PersonalTokenInfo, StoreError> {
        self.call(move |conn| {
            let created_at = now_ms();
            let (logs, metrics, alerts_write, kind, scope_id) = permission_values(permissions);
            conn.execute(
                "INSERT INTO mcp_personal_tokens(id,username,name,token_hash,created_at,expires_at,logs,metrics,alerts_write,scope_kind,scope_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                rusqlite::params![id, username, name, token_hash, created_at, expires_at, logs, metrics, alerts_write, kind, scope_id],
            )?;
            let expires_at = (expires_at != NEVER_EXPIRES).then_some(expires_at);
            Ok(PersonalTokenInfo { id, name, created_at, expires_at, last_used_at: None, permissions })
        }).await
    }

    pub async fn personal_tokens(
        &self,
        username: String,
    ) -> Result<Vec<PersonalTokenInfo>, StoreError> {
        self.call(move |conn| {
            let mut statement = conn.prepare("SELECT id,name,created_at,expires_at,last_used_at,logs,metrics,alerts_write,scope_kind,scope_id FROM mcp_personal_tokens WHERE username=?1 AND revoked_at IS NULL ORDER BY created_at DESC,id")?;
            statement.query_map([username], |row| Ok(PersonalTokenInfo {
                id: row.get(0)?, name: row.get(1)?, created_at: row.get(2)?,
                expires_at: Some(row.get::<_, i64>(3)?).filter(|value| *value != NEVER_EXPIRES), last_used_at: row.get(4)?,
                permissions: permissions_from_row(row, 5)?,
            }))?.collect()
        }).await
    }

    pub async fn revoke_personal_token(
        &self,
        username: String,
        id: String,
    ) -> Result<bool, StoreError> {
        self.call(move |conn| Ok(conn.execute(
            "UPDATE mcp_personal_tokens SET revoked_at=?3 WHERE username=?1 AND id=?2 AND revoked_at IS NULL",
            rusqlite::params![username,id,now_ms()],
        )? > 0)).await
    }

    pub async fn oauth_grants(&self, username: String) -> Result<Vec<OAuthGrantInfo>, StoreError> {
        self.call(move |conn| {
            let mut statement = conn.prepare("SELECT id,client_name,scope,created_at,last_used_at,logs,metrics,alerts_write,scope_kind,scope_id FROM oauth_grants WHERE username=?1 AND revoked_at IS NULL ORDER BY created_at DESC,id")?;
            statement.query_map([username], |row| Ok(OAuthGrantInfo {
                id: row.get(0)?, client_name: row.get(1)?, scope: row.get(2)?, created_at: row.get(3)?, last_used_at: row.get(4)?,
                permissions: permissions_from_row(row, 5)?,
            }))?.collect()
        }).await
    }

    pub async fn revoke_oauth_grant(
        &self,
        username: String,
        id: String,
    ) -> Result<bool, StoreError> {
        self.call(move |conn| {
            let tx = conn.transaction()?;
            let now = now_ms();
            let changed = tx.execute("UPDATE oauth_grants SET revoked_at=?3 WHERE username=?1 AND id=?2 AND revoked_at IS NULL", rusqlite::params![username,id,now])?;
            if changed > 0 {
                tx.execute("UPDATE oauth_tokens SET revoked_at=?2 WHERE grant_id=?1 AND revoked_at IS NULL", rusqlite::params![id,now])?;
            }
            tx.commit()?;
            Ok(changed > 0)
        }).await
    }

    pub async fn revoke_user_mcp_credentials(&self, username: String) -> Result<(), StoreError> {
        self.call(move |conn| {
            let tx = conn.transaction()?;
            let now = now_ms();
            tx.execute("UPDATE mcp_personal_tokens SET revoked_at=?2 WHERE username=?1 AND revoked_at IS NULL", rusqlite::params![username,now])?;
            tx.execute("UPDATE oauth_grants SET revoked_at=?2 WHERE username=?1 AND revoked_at IS NULL", rusqlite::params![username,now])?;
            tx.execute("UPDATE oauth_tokens SET revoked_at=?2 WHERE grant_id IN (SELECT id FROM oauth_grants WHERE username=?1) AND revoked_at IS NULL", rusqlite::params![username,now])?;
            tx.commit()
        }).await
    }

    pub async fn authenticate_mcp_token(
        &self,
        token_hash: [u8; 32],
        resource: String,
    ) -> Result<Option<TokenAccess>, StoreError> {
        self.call(move |conn| {
            let tx = conn.transaction()?;
            let now = now_ms();
            let personal: Option<(String, Permissions)> = tx.query_row(
                "SELECT username,logs,metrics,alerts_write,scope_kind,scope_id FROM mcp_personal_tokens WHERE token_hash=?1 AND expires_at>?2 AND revoked_at IS NULL",
                rusqlite::params![token_hash,now], |row| Ok((row.get(0)?, permissions_from_row(row, 1)?)),
            ).optional()?;
            if let Some((username, permissions)) = personal {
                tx.execute("UPDATE mcp_personal_tokens SET last_used_at=?2 WHERE token_hash=?1", rusqlite::params![token_hash,now])?;
                tx.commit()?;
                return Ok(Some(access(username, permissions)));
            }
            let oauth: Option<(String, String, Permissions)> = tx.query_row(
                "SELECT g.username,g.id,g.logs,g.metrics,g.alerts_write,g.scope_kind,g.scope_id FROM oauth_tokens t JOIN oauth_grants g ON g.id=t.grant_id WHERE t.token_hash=?1 AND t.kind='access' AND t.expires_at>?2 AND t.revoked_at IS NULL AND g.revoked_at IS NULL AND g.resource=?3",
                rusqlite::params![token_hash,now,resource], |row| Ok((row.get(0)?, row.get(1)?, permissions_from_row(row, 2)?)),
            ).optional()?;
            if let Some((username, grant_id, permissions)) = oauth {
                tx.execute("UPDATE oauth_tokens SET last_used_at=?2 WHERE token_hash=?1", rusqlite::params![token_hash,now])?;
                tx.execute("UPDATE oauth_grants SET last_used_at=?2 WHERE id=?1", rusqlite::params![grant_id,now])?;
                tx.commit()?;
                return Ok(Some(access(username, permissions)));
            }
            tx.commit()?;
            Ok(None)
        }).await
    }

    pub async fn create_oauth_pending(
        &self,
        pending: PendingAuthorization,
    ) -> Result<(), StoreError> {
        self.call(move |conn| {
            conn.execute(
                "INSERT INTO oauth_pending(id,username,client_id,client_name,redirect_uri,state,scope,resource,code_challenge,expires_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                rusqlite::params![pending.id,pending.username,pending.client_id,pending.client_name,pending.redirect_uri,pending.state,pending.scope,pending.resource,pending.code_challenge,pending.expires_at],
            )?;
            Ok(())
        }).await
    }

    pub async fn oauth_pending(
        &self,
        id: String,
        username: String,
    ) -> Result<Option<PendingAuthorization>, StoreError> {
        self.call(move |conn| {
            let now = now_ms();
            conn.execute("DELETE FROM oauth_pending WHERE expires_at<=?1", [now])?;
            conn.query_row(
                "SELECT id,username,client_id,client_name,redirect_uri,state,scope,resource,code_challenge,expires_at FROM oauth_pending WHERE id=?1 AND username=?2 AND expires_at>?3",
                rusqlite::params![id,username,now],
                |row| Ok(PendingAuthorization {
                    id: row.get(0)?, username: row.get(1)?, client_id: row.get(2)?, client_name: row.get(3)?,
                    redirect_uri: row.get(4)?, state: row.get(5)?, scope: row.get(6)?, resource: row.get(7)?,
                    code_challenge: row.get(8)?, expires_at: row.get(9)?,
                }),
            ).optional()
        }).await
    }

    /// Завершает согласие: при `decision` создаёт код с выбранными правами и scope.
    pub async fn complete_oauth_pending(
        &self,
        id: String,
        username: String,
        decision: Option<([u8; 32], Permissions)>,
    ) -> Result<Option<PendingAuthorization>, StoreError> {
        self.call(move |conn| {
            let tx = conn.transaction()?;
            let now = now_ms();
            let pending = tx.query_row(
                "SELECT id,username,client_id,client_name,redirect_uri,state,scope,resource,code_challenge,expires_at FROM oauth_pending WHERE id=?1 AND username=?2 AND expires_at>?3",
                rusqlite::params![id,username,now],
                |row| Ok(PendingAuthorization {
                    id: row.get(0)?, username: row.get(1)?, client_id: row.get(2)?, client_name: row.get(3)?,
                    redirect_uri: row.get(4)?, state: row.get(5)?, scope: row.get(6)?, resource: row.get(7)?,
                    code_challenge: row.get(8)?, expires_at: row.get(9)?,
                }),
            ).optional()?;
            let Some(pending) = pending else { return Ok(None); };
            tx.execute("DELETE FROM oauth_pending WHERE id=?1", [&pending.id])?;
            if let Some((code_hash, permissions)) = decision {
                let (logs, metrics, alerts_write, kind, scope_id) = permission_values(permissions);
                let granted = permissions.rights.oauth_scope();
                tx.execute(
                    "INSERT INTO oauth_codes(code_hash,username,client_id,client_name,redirect_uri,scope,resource,code_challenge,expires_at,logs,metrics,alerts_write,scope_kind,scope_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
                    rusqlite::params![code_hash,pending.username,pending.client_id,pending.client_name,pending.redirect_uri,granted,pending.resource,pending.code_challenge,now+300_000,logs,metrics,alerts_write,kind,scope_id],
                )?;
            }
            tx.commit()?;
            Ok(Some(pending))
        }).await
    }

    pub async fn redeem_oauth_code(
        &self,
        code_hash: [u8; 32],
        client_id: String,
        redirect_uri: String,
        resource: String,
        code_challenge: String,
        token_hashes: ([u8; 32], [u8; 32]),
    ) -> Result<Option<IssuedOAuthTokens>, StoreError> {
        let (access_hash, refresh_hash) = token_hashes;
        self.call(move |conn| {
            let tx = conn.transaction()?;
            let now = now_ms();
            let code = tx.query_row(
                "SELECT username,client_id,client_name,redirect_uri,scope,resource,code_challenge,expires_at,used_at FROM oauth_codes WHERE code_hash=?1",
                [code_hash],
                |row| Ok((
                    row.get::<_,String>(0)?, row.get::<_,String>(1)?, row.get::<_,String>(2)?,
                    row.get::<_,String>(3)?, row.get::<_,String>(4)?, row.get::<_,String>(5)?,
                    row.get::<_,String>(6)?, row.get::<_,i64>(7)?, row.get::<_,Option<i64>>(8)?,
                )),
            ).optional()?;
            let Some((username,stored_client,_client_name,stored_redirect,scope,stored_resource,stored_challenge,expires_at,used_at)) = code else {
                return Ok(None);
            };
            if expires_at <= now || used_at.is_some() || !is_granted_scope(&scope)
                || client_id != stored_client || redirect_uri != stored_redirect
                || resource != stored_resource || code_challenge != stored_challenge
            {
                return Ok(None);
            }
            if tx.execute("UPDATE oauth_codes SET used_at=?2 WHERE code_hash=?1 AND used_at IS NULL", rusqlite::params![code_hash,now])? != 1 {
                return Ok(None);
            }
            let grant_id = uuid::Uuid::new_v4().to_string();
            // Права и скоуп переносятся из кода, который выбрал пользователь на согласии.
            tx.execute(
                "INSERT INTO oauth_grants(id,username,client_id,client_name,scope,resource,created_at,logs,metrics,alerts_write,scope_kind,scope_id)
                 SELECT ?1,username,client_id,client_name,scope,resource,?2,logs,metrics,alerts_write,scope_kind,scope_id FROM oauth_codes WHERE code_hash=?3",
                rusqlite::params![grant_id,now,code_hash],
            )?;
            tx.execute(
                "INSERT INTO oauth_tokens(token_hash,grant_id,kind,expires_at,created_at) VALUES(?1,?2,'access',?3,?4),(?5,?2,'refresh',?6,?4)",
                rusqlite::params![access_hash,grant_id,now+3_600_000,now,refresh_hash,now+2_592_000_000],
            )?;
            tx.commit()?;
            Ok(Some(IssuedOAuthTokens { username, grant_id, scope }))
        }).await
    }

    pub async fn rotate_oauth_refresh(
        &self,
        refresh_hash: [u8; 32],
        client_id: String,
        resource: String,
        access_hash: [u8; 32],
        next_refresh_hash: [u8; 32],
    ) -> Result<(RefreshOutcome, Option<IssuedOAuthTokens>), StoreError> {
        self.call(move |conn| {
            let tx = conn.transaction()?;
            let now = now_ms();
            let token = tx.query_row(
                "SELECT t.grant_id,t.kind,t.expires_at,t.used_at,t.revoked_at,g.username,g.client_id,g.scope,g.resource,g.revoked_at FROM oauth_tokens t JOIN oauth_grants g ON g.id=t.grant_id WHERE t.token_hash=?1",
                [refresh_hash],
                |row| Ok((
                    row.get::<_,String>(0)?, row.get::<_,String>(1)?, row.get::<_,i64>(2)?,
                    row.get::<_,Option<i64>>(3)?, row.get::<_,Option<i64>>(4)?, row.get::<_,String>(5)?,
                    row.get::<_,String>(6)?, row.get::<_,String>(7)?, row.get::<_,String>(8)?,
                    row.get::<_,Option<i64>>(9)?,
                )),
            ).optional()?;
            let Some((grant_id,kind,expires_at,used_at,token_revoked,username,stored_client,scope,stored_resource,grant_revoked)) = token else {
                tx.commit()?;
                return Ok((RefreshOutcome::Invalid,None));
            };
            if kind != "refresh" || client_id != stored_client || resource != stored_resource
                || !is_granted_scope(&scope) || token_revoked.is_some() || grant_revoked.is_some()
            {
                tx.commit()?;
                return Ok((RefreshOutcome::Invalid,None));
            }
            if used_at.is_some() {
                tx.execute("UPDATE oauth_grants SET revoked_at=?2 WHERE id=?1", rusqlite::params![grant_id,now])?;
                tx.execute("UPDATE oauth_tokens SET revoked_at=?2 WHERE grant_id=?1 AND revoked_at IS NULL", rusqlite::params![grant_id,now])?;
                tx.commit()?;
                return Ok((RefreshOutcome::Replay,None));
            }
            if expires_at <= now || tx.execute("UPDATE oauth_tokens SET used_at=?2 WHERE token_hash=?1 AND used_at IS NULL", rusqlite::params![refresh_hash,now])? != 1 {
                tx.commit()?;
                return Ok((RefreshOutcome::Invalid,None));
            }
            tx.execute(
                "INSERT INTO oauth_tokens(token_hash,grant_id,kind,expires_at,created_at) VALUES(?1,?2,'access',?3,?4),(?5,?2,'refresh',?6,?4)",
                rusqlite::params![access_hash,grant_id,now+3_600_000,now,next_refresh_hash,now+2_592_000_000],
            )?;
            tx.execute("UPDATE oauth_grants SET last_used_at=?2 WHERE id=?1", rusqlite::params![grant_id,now])?;
            tx.commit()?;
            Ok((RefreshOutcome::Issued,Some(IssuedOAuthTokens { username, grant_id, scope })))
        }).await
    }

    /// Создаёт каталог, берёт эксклюзивную блокировку и применяет миграции.
    pub fn open(dir: &Path) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        let lock = File::create(dir.join("dock.lock"))?;
        lock.try_lock_exclusive().map_err(|err| {
            io::Error::new(
                err.kind(),
                format!("{} is used by another lognara-dock: {err}", dir.display()),
            )
        })?;
        let mut conn = Connection::open(dir.join("dock.sqlite3")).map_err(io::Error::other)?;
        migrate(&mut conn).map_err(io::Error::other)?;
        conn.execute(
            "UPDATE metric_alert_deliveries SET status='pending' WHERE status='sending'",
            [],
        )
        .map_err(io::Error::other)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            _lock: Arc::new(lock),
        })
    }

    /// Выполняет `f` с соединением в blocking-потоке.
    pub async fn call<T, F>(&self, f: F) -> Result<T, StoreError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> rusqlite::Result<T> + Send + 'static,
    {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || f(&mut conn.lock().unwrap()))
            .await
            .map_err(|_| StoreError::Task)?
            .map_err(StoreError::from)
    }
}

fn migrate(conn: &mut Connection) -> rusqlite::Result<()> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "FULL")?;
    // Пересборка таблицы с дочерними строками возможна только без проверки внешних ключей,
    // поэтому ключи выключены на время миграций и включаются после проверки.
    conn.pragma_update(None, "foreign_keys", false)?;
    let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(version as usize) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        // Проверка до commit: при нарушении миграция откатывается, user_version не растёт.
        if tx.prepare("PRAGMA foreign_key_check")?.exists([])? {
            return Err(rusqlite::Error::InvalidQuery);
        }
        tx.pragma_update(None, "user_version", index as i64 + 1)?;
        tx.commit()?;
    }
    conn.pragma_update(None, "foreign_keys", true)?;
    Ok(())
}

/// Значения колонок прав: логи, метрики, запись алертов, тип и id скоупа.
fn permission_values(permissions: Permissions) -> (i64, i64, i64, &'static str, i64) {
    let rights = permissions.rights;
    let (kind, id) = permissions.node_scope.columns();
    (
        rights.logs as i64,
        rights.metrics as i64,
        rights.alerts_write as i64,
        kind,
        id,
    )
}

/// Читает права из колонок `logs, metrics, alerts_write, scope_kind, scope_id` начиная с `offset`.
fn permissions_from_row(row: &rusqlite::Row<'_>, offset: usize) -> rusqlite::Result<Permissions> {
    let kind: String = row.get(offset + 3)?;
    let node_scope =
        Scope::from_columns(&kind, row.get(offset + 4)?).ok_or(rusqlite::Error::InvalidQuery)?;
    Ok(Permissions {
        rights: Rights {
            logs: row.get::<_, i64>(offset)? != 0,
            metrics: row.get::<_, i64>(offset + 1)? != 0,
            alerts_write: row.get::<_, i64>(offset + 2)? != 0,
        },
        node_scope,
    })
}

fn access(username: String, permissions: Permissions) -> TokenAccess {
    TokenAccess {
        username,
        rights: permissions.rights,
        node_scope: permissions.node_scope,
    }
}

/// Текущее время в миллисекундах Unix.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    fn hash(value: &str) -> [u8; 32] {
        Sha256::digest(value.as_bytes()).into()
    }

    fn personal_permissions() -> Permissions {
        Permissions {
            rights: Rights::default(),
            node_scope: Scope::All,
        }
    }

    fn oauth_permissions() -> Permissions {
        Permissions {
            rights: Rights {
                logs: true,
                metrics: true,
                alerts_write: true,
            },
            node_scope: Scope::All,
        }
    }

    fn pending(id: &str) -> PendingAuthorization {
        PendingAuthorization {
            id: id.into(),
            username: "alice".into(),
            client_id: "https://chatgpt.com/oauth/client.json".into(),
            client_name: "ChatGPT".into(),
            redirect_uri: "https://chatgpt.com/connector_platform_oauth_redirect".into(),
            state: "state-1".into(),
            scope: "lognara.read".into(),
            resource: "https://dock.example.com/mcp".into(),
            code_challenge: "challenge-1".into(),
            expires_at: now_ms() + 300_000,
        }
    }

    async fn redeem(
        store: &Store,
        code_hash: [u8; 32],
        client_id: String,
        redirect: String,
        resource: String,
        challenge: String,
        token_hashes: ([u8; 32], [u8; 32]),
    ) -> Result<Option<IssuedOAuthTokens>, StoreError> {
        let (access_hash, refresh_hash) = token_hashes;
        store
            .redeem_oauth_code(
                code_hash,
                client_id,
                redirect,
                resource,
                challenge,
                (access_hash, refresh_hash),
            )
            .await
    }

    #[tokio::test]
    async fn personal_tokens_are_hashed_expire_revoke_and_survive_restart() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let token_hash = hash("personal-secret");
        store
            .create_personal_token(
                "pat-1".into(),
                "alice".into(),
                "Codex workstation".into(),
                token_hash,
                now_ms() + 30 * 24 * 60 * 60 * 1000,
                personal_permissions(),
            )
            .await
            .unwrap();
        store
            .create_personal_token(
                "pat-expired".into(),
                "alice".into(),
                "Expired".into(),
                hash("expired-secret"),
                now_ms() - 1,
                personal_permissions(),
            )
            .await
            .unwrap();
        assert_eq!(
            store
                .authenticate_mcp_token(token_hash, "https://wrong.example/mcp".into())
                .await
                .unwrap()
                .map(|access| access.username),
            Some("alice".to_owned())
        );
        assert!(
            store
                .authenticate_mcp_token(
                    hash("expired-secret"),
                    "https://dock.example.com/mcp".into()
                )
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .personal_tokens("alice".into())
                .await
                .unwrap()
                .iter()
                .find(|token| token.id == "pat-1")
                .unwrap()
                .last_used_at
                .is_some()
        );
        assert!(
            store
                .authenticate_mcp_token(
                    hash("missing-secret"),
                    "https://dock.example.com/mcp".into()
                )
                .await
                .unwrap()
                .is_none()
        );
        drop(store);

        let store = Store::open(dir.path()).unwrap();
        assert!(
            store
                .authenticate_mcp_token(token_hash, "https://dock.example.com/mcp".into())
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            store
                .revoke_personal_token("alice".into(), "pat-1".into())
                .await
                .unwrap()
        );
        assert!(
            store
                .authenticate_mcp_token(token_hash, "https://dock.example.com/mcp".into())
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn oauth_code_pkce_audience_replay_refresh_rotation_and_restart_are_enforced() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let code_hash = hash("one-use-code");
        store
            .create_oauth_pending(pending("pending-1"))
            .await
            .unwrap();
        let pending = store
            .complete_oauth_pending(
                "pending-1".into(),
                "alice".into(),
                Some((code_hash, oauth_permissions())),
            )
            .await
            .unwrap()
            .unwrap();
        let now = now_ms();
        let code_expiry: i64 = store
            .call(move |conn| {
                conn.query_row(
                    "SELECT expires_at FROM oauth_codes WHERE code_hash=?1",
                    [code_hash],
                    |row| row.get(0),
                )
            })
            .await
            .unwrap();
        assert!((now + 299_000..=now + 300_000).contains(&code_expiry));

        let access_hash = hash("access-1");
        let refresh_hash = hash("refresh-1");
        assert!(
            redeem(
                &store,
                code_hash,
                "https://evil.example/client.json".into(),
                pending.redirect_uri.clone(),
                pending.resource.clone(),
                pending.code_challenge.clone(),
                (access_hash, refresh_hash),
            )
            .await
            .unwrap()
            .is_none()
        );
        assert!(
            redeem(
                &store,
                code_hash,
                pending.client_id.clone(),
                pending.redirect_uri.clone(),
                "https://wrong.example/mcp".into(),
                pending.code_challenge.clone(),
                (access_hash, refresh_hash),
            )
            .await
            .unwrap()
            .is_none()
        );
        assert!(
            redeem(
                &store,
                code_hash,
                pending.client_id.clone(),
                pending.redirect_uri.clone(),
                pending.resource.clone(),
                "wrong-challenge".into(),
                (access_hash, refresh_hash),
            )
            .await
            .unwrap()
            .is_none()
        );
        let issued = redeem(
            &store,
            code_hash,
            pending.client_id.clone(),
            pending.redirect_uri.clone(),
            pending.resource.clone(),
            pending.code_challenge.clone(),
            (access_hash, refresh_hash),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(
            redeem(
                &store,
                code_hash,
                pending.client_id,
                pending.redirect_uri,
                pending.resource,
                pending.code_challenge,
                (access_hash, refresh_hash),
            )
            .await
            .unwrap()
            .is_none()
        );
        assert!(
            store
                .authenticate_mcp_token(access_hash, "https://wrong.example/mcp".into())
                .await
                .unwrap()
                .is_none()
        );
        drop(store);

        let store = Store::open(dir.path()).unwrap();
        assert_eq!(
            store
                .authenticate_mcp_token(access_hash, "https://dock.example.com/mcp".into())
                .await
                .unwrap()
                .map(|access| access.username)
                .as_deref(),
            Some("alice")
        );
        let access_expiry: i64 = store
            .call(move |conn| {
                conn.query_row(
                    "SELECT expires_at FROM oauth_tokens WHERE token_hash=?1",
                    [access_hash],
                    |row| row.get(0),
                )
            })
            .await
            .unwrap();
        let refresh_expiry: i64 = store
            .call(move |conn| {
                conn.query_row(
                    "SELECT expires_at FROM oauth_tokens WHERE token_hash=?1",
                    [refresh_hash],
                    |row| row.get(0),
                )
            })
            .await
            .unwrap();
        let now = now_ms();
        assert!((now + 3_599_000..=now + 3_600_000).contains(&access_expiry));
        assert!((now + 2_591_999_000..=now + 2_592_000_000).contains(&refresh_expiry));

        let next_access = hash("access-2");
        let next_refresh = hash("refresh-2");
        assert_eq!(
            store
                .rotate_oauth_refresh(
                    refresh_hash,
                    "https://chatgpt.com/oauth/client.json".into(),
                    "https://dock.example.com/mcp".into(),
                    next_access,
                    next_refresh,
                )
                .await
                .unwrap()
                .0,
            RefreshOutcome::Issued
        );
        assert_eq!(
            store
                .rotate_oauth_refresh(
                    refresh_hash,
                    "https://chatgpt.com/oauth/client.json".into(),
                    "https://dock.example.com/mcp".into(),
                    hash("access-3"),
                    hash("refresh-3"),
                )
                .await
                .unwrap()
                .0,
            RefreshOutcome::Replay
        );
        assert!(
            store
                .authenticate_mcp_token(next_access, "https://dock.example.com/mcp".into())
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(issued.username, "alice");
    }

    #[tokio::test]
    async fn favorite_metrics_survive_reopen_and_migration() {
        let dir = tempfile::tempdir().unwrap();
        {
            let conn = Connection::open(dir.path().join("dock.sqlite3")).unwrap();
            for sql in &MIGRATIONS[..MIGRATIONS.len() - 1] {
                conn.execute_batch(sql).unwrap();
            }
            conn.pragma_update(None, "user_version", MIGRATIONS.len() as i64 - 1)
                .unwrap();
        }
        let key = crate::series::MetricKey {
            server: "a".into(),
            service: "api".into(),
            name: "runs".into(),
            origin: "counter".into(),
        };
        let store = Store::open(dir.path()).unwrap();
        assert!(store.favorite_metrics().await.unwrap().is_empty());
        store.set_favorite_metric(key.clone(), true).await.unwrap();
        drop(store);
        let reopened = Store::open(dir.path()).unwrap();
        assert_eq!(reopened.favorite_metrics().await.unwrap(), vec![key]);
    }

    #[tokio::test]
    async fn existing_tokens_keep_read_access_and_alert_children_survive_rebuild() {
        let dir = tempfile::tempdir().unwrap();
        {
            let conn = Connection::open(dir.path().join("dock.sqlite3")).unwrap();
            // Схема до прав токенов и скоупа алертов: без них и последующих миграций.
            let before = MIGRATIONS.len() - 4;
            for sql in &MIGRATIONS[..before] {
                conn.execute_batch(sql).unwrap();
            }
            conn.pragma_update(None, "user_version", before as i64)
                .unwrap();
            conn.execute(
                "INSERT INTO mcp_personal_tokens(id,username,name,token_hash,created_at,expires_at) VALUES('pat-old','alice','old',?1,1,?2)",
                rusqlite::params![hash("old-pat"), i64::MAX],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO oauth_grants(id,username,client_id,client_name,scope,resource,created_at) VALUES('grant-old','alice','https://chatgpt.com/oauth/client.json','ChatGPT','lognara.read','https://dock.example.com/mcp',1)",
                [],
            )
            .unwrap();
            conn.execute_batch(
                "INSERT INTO metric_alerts(id,server,service,name,origin,enabled,environment,no_data,combine_mode,revision,enabled_at)
                     VALUES('alert-1','s','api','runs','counter',1,NULL,'normal','and',1,1);
                 INSERT INTO metric_alert_triggers(alert_id,position,window_seconds,aggregate,comparator,threshold)
                     VALUES('alert-1',0,60,'sum','>',1.0);
                 INSERT INTO metric_alert_states(alert_id) VALUES('alert-1');
                 INSERT INTO metric_alert_groups(id,kind,name,created_at) VALUES('group-1','personal','Team',1);
                 INSERT INTO metric_alert_groups_used(alert_id,group_id) VALUES('alert-1','group-1');
                 INSERT INTO metric_alert_outbox(id,alert_id,alert_revision,event_kind,message,created_at,expires_at)
                     VALUES('event-1','alert-1',1,'firing','text',1,2);",
            )
            .unwrap();
        }

        let store = Store::open(dir.path()).unwrap();
        let tokens = store.personal_tokens("alice".into()).await.unwrap();
        assert_eq!(tokens[0].permissions, personal_permissions());
        let grants = store.oauth_grants("alice".into()).await.unwrap();
        assert_eq!(grants[0].permissions, personal_permissions());
        let counts: (i64, i64, i64, i64) = store
            .call(|conn| {
                conn.query_row(
                    "SELECT (SELECT count(*) FROM metric_alert_triggers WHERE alert_id='alert-1'),
                            (SELECT count(*) FROM metric_alert_states WHERE alert_id='alert-1'),
                            (SELECT count(*) FROM metric_alert_groups_used WHERE alert_id='alert-1'),
                            (SELECT count(*) FROM metric_alert_outbox WHERE alert_id='alert-1')",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
            })
            .await
            .unwrap();
        assert_eq!(counts, (1, 1, 1, 1));
        // Глобальный и скоупленный алерт с одним ключом не конфликтуют; дубль глобального отклоняется.
        store
            .call(|conn| {
                conn.execute(
                    "INSERT INTO metric_alerts(id,server,service,name,origin,enabled,no_data,combine_mode,revision,enabled_at,scope_kind,scope_id)
                     VALUES('alert-2','s','api','runs','counter',0,'normal','and',1,1,'relay',1)",
                    [],
                )
            })
            .await
            .unwrap();
        let duplicate = store
            .call(|conn| {
                conn.execute(
                    "INSERT INTO metric_alerts(id,server,service,name,origin,enabled,no_data,combine_mode,revision,enabled_at)
                     VALUES('alert-3','s','api','runs','counter',0,'normal','and',1,1)",
                    [],
                )
            })
            .await;
        assert!(duplicate.is_err());
    }

    #[test]
    fn second_process_cannot_open_same_dir() {
        let dir = tempfile::tempdir().unwrap();
        let _first = Store::open(dir.path()).unwrap();
        assert!(Store::open(dir.path()).is_err());
    }

    #[test]
    fn migrations_are_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        drop(Store::open(dir.path()).unwrap());
        drop(Store::open(dir.path()).unwrap());
    }

    #[test]
    fn alert_message_migration_keeps_prepared_outbox_and_defaults_old_rules() {
        let dir = tempfile::tempdir().unwrap();
        {
            let conn = Connection::open(dir.path().join("dock.sqlite3")).unwrap();
            for sql in &MIGRATIONS[..MIGRATIONS.len() - 1] {
                conn.execute_batch(sql).unwrap();
            }
            conn.execute_batch(
                "INSERT INTO metric_alerts(id,server,service,name,origin,enabled,environment,no_data,combine_mode,revision,enabled_at)
                     VALUES('metric-1','s','api','runs','counter',1,NULL,'normal','and',1,1);
                 INSERT INTO log_alerts(id,name,enabled,query,filters,text,text_mode,threshold,cooldown_seconds,revision,created_at)
                     VALUES('logs-1','errors',1,'level:error','{}',NULL,'all',1,300,1,1);
                 INSERT INTO metric_alert_outbox(id,alert_id,alert_revision,event_kind,message,created_at,expires_at)
                     VALUES('event-1','metric-1',1,'firing','old & <message>',1,1000);",
            )
            .unwrap();
            conn.pragma_update(None, "user_version", MIGRATIONS.len() as i64 - 1)
                .unwrap();
        }

        let _store = Store::open(dir.path()).unwrap();
        let conn = Connection::open(dir.path().join("dock.sqlite3")).unwrap();
        let version: i64 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, MIGRATIONS.len() as i64);
        let (metric_fields, log_fields, message, parse_mode, reply_markup): (
            String,
            String,
            String,
            Option<String>,
            Option<String>,
        ) = conn
            .query_row(
                "SELECT a.message_fields,l.message_fields,o.message,o.parse_mode,o.reply_markup
                 FROM metric_alerts a,log_alerts l,metric_alert_outbox o
                 WHERE a.id='metric-1' AND l.id='logs-1' AND o.id='event-1'",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Vec<String>>(&metric_fields).unwrap(),
            crate::alerts::default_metric_message_fields()
        );
        assert_eq!(
            serde_json::from_str::<Vec<String>>(&log_fields).unwrap(),
            crate::alerts::default_log_message_fields()
        );
        assert_eq!(message, "old & <message>");
        assert_eq!(parse_mode, None);
        assert_eq!(reply_markup, None);
    }
}
