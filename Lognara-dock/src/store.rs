//! SQLite в каталоге данных: сессии и passkey. Каталог блокируется одним процессом.

use std::fs::{self, File};
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use rusqlite::Connection;

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
];

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
