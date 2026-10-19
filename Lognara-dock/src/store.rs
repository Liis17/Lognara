//! SQLite в каталоге данных: сессии, passkey, скрытые паттерны, статусы проблем и избранное метрик.
//! Каталог блокируется одним процессом.

use std::fs::{self, File};
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use rusqlite::{Connection, OptionalExtension};

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
    conn.pragma_update(None, "foreign_keys", true)?;
    let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(version as usize) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", index as i64 + 1)?;
        tx.commit()?;
    }
    Ok(())
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
}
