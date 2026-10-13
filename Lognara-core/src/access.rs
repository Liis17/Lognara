//! Персональные учётные данные. Plaintext токена существует только при создании.
use std::{
    collections::HashMap,
    path::Path,
    sync::{Mutex, RwLock},
};

use rand::{TryRngCore, rngs::OsRng};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Debug, thiserror::Error)]
pub enum AccessError {
    #[error("invalid_name")]
    InvalidName,
    #[error("name_taken")]
    NameTaken,
    #[error("relay_not_found")]
    RelayNotFound,
    #[error("not_found")]
    NotFound,
    #[error("relay_has_agents")]
    RelayHasAgents,
    #[error(transparent)]
    Storage(#[from] anyhow::Error),
}

impl From<rusqlite::Error> for AccessError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Storage(error.into())
    }
}

type Result<T> = std::result::Result<T, AccessError>;

#[derive(Clone, Serialize)]
pub struct Relay {
    pub id: i64,
    pub name: String,
    pub created_at: String,
}
#[derive(Clone, Serialize)]
pub struct Agent {
    pub id: i64,
    pub name: String,
    pub relay_id: i64,
    pub created_at: String,
}
#[derive(Serialize)]
pub struct CreatedRelay {
    pub relay: Relay,
    pub token: String,
}
#[derive(Serialize)]
pub struct CreatedAgent {
    pub agent: Agent,
    pub token: String,
}

pub struct Access {
    connection: Mutex<Connection>,
    relays: RwLock<HashMap<[u8; 32], i64>>,
}

impl Access {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let connection = Connection::open(path)?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        let version: u32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        anyhow::ensure!(version <= 1, "unsupported access schema version {version}");
        connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;
            CREATE TABLE IF NOT EXISTS relays(id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, token_hash BLOB NOT NULL UNIQUE, created_at TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS agents(id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, relay_id INTEGER NOT NULL REFERENCES relays(id), token_hash BLOB NOT NULL UNIQUE, created_at TEXT NOT NULL, UNIQUE(relay_id,name));
            PRAGMA user_version=1;")?;
        crate::wal::sync_dir(path.parent().expect("access has a parent"))?;
        let access = Self {
            connection: Mutex::new(connection),
            relays: RwLock::new(HashMap::new()),
        };
        access.reload_relays(&access.connection.lock().unwrap())?;
        Ok(access)
    }

    fn reload_relays(&self, connection: &Connection) -> Result<()> {
        let mut statement = connection.prepare("SELECT token_hash,id FROM relays")?;
        let relays = statement
            .query_map([], |row| {
                Ok((row.get::<_, [u8; 32]>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<std::result::Result<HashMap<_, _>, _>>()?;
        *self.relays.write().unwrap() = relays;
        Ok(())
    }

    pub fn create_relay(&self, name: &str) -> Result<CreatedRelay> {
        let name = valid_name(name)?;
        let connection = self.connection.lock().unwrap();
        if connection
            .query_row("SELECT id FROM relays WHERE name=?1", [&name], |row| {
                row.get::<_, i64>(0)
            })
            .optional()?
            .is_some()
        {
            return Err(AccessError::NameTaken);
        }
        let (token, hash, created_at) = credentials("lnr_")?;
        connection.execute(
            "INSERT INTO relays(name,token_hash,created_at) VALUES(?1,?2,?3)",
            params![name, hash.as_slice(), created_at],
        )?;
        let relay = Relay {
            id: connection.last_insert_rowid(),
            name,
            created_at,
        };
        self.reload_relays(&connection)?;
        Ok(CreatedRelay { relay, token })
    }

    pub fn create_agent(&self, name: &str, relay_id: i64) -> Result<CreatedAgent> {
        let name = valid_name(name)?;
        let connection = self.connection.lock().unwrap();
        if connection
            .query_row("SELECT id FROM relays WHERE id=?1", [relay_id], |row| {
                row.get::<_, i64>(0)
            })
            .optional()?
            .is_none()
        {
            return Err(AccessError::RelayNotFound);
        }
        if connection
            .query_row(
                "SELECT id FROM agents WHERE relay_id=?1 AND name=?2",
                params![relay_id, name],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .is_some()
        {
            return Err(AccessError::NameTaken);
        }
        let (token, hash, created_at) = credentials("lna_")?;
        connection.execute(
            "INSERT INTO agents(name,relay_id,token_hash,created_at) VALUES(?1,?2,?3,?4)",
            params![name, relay_id, hash.as_slice(), created_at],
        )?;
        let agent = Agent {
            id: connection.last_insert_rowid(),
            name,
            relay_id,
            created_at,
        };
        self.reload_relays(&connection)?;
        Ok(CreatedAgent { agent, token })
    }

    pub fn list_relays(&self) -> Result<Vec<Relay>> {
        let connection = self.connection.lock().unwrap();
        let mut statement =
            connection.prepare("SELECT id,name,created_at FROM relays ORDER BY id")?;
        Ok(statement
            .query_map([], |row| {
                Ok(Relay {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    created_at: row.get(2)?,
                })
            })?
            .collect::<std::result::Result<_, _>>()?)
    }

    pub fn list_agents(&self) -> Result<Vec<Agent>> {
        let connection = self.connection.lock().unwrap();
        let mut statement =
            connection.prepare("SELECT id,name,relay_id,created_at FROM agents ORDER BY id")?;
        Ok(statement
            .query_map([], |row| {
                Ok(Agent {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    relay_id: row.get(2)?,
                    created_at: row.get(3)?,
                })
            })?
            .collect::<std::result::Result<_, _>>()?)
    }

    pub fn delete_relay(&self, id: i64) -> Result<()> {
        let connection = self.connection.lock().unwrap();
        if connection
            .query_row(
                "SELECT id FROM agents WHERE relay_id=?1 LIMIT 1",
                [id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .is_some()
        {
            return Err(AccessError::RelayHasAgents);
        }
        if connection.execute("DELETE FROM relays WHERE id=?1", [id])? == 0 {
            return Err(AccessError::NotFound);
        }
        self.reload_relays(&connection)
    }

    pub fn delete_agent(&self, id: i64) -> Result<()> {
        let connection = self.connection.lock().unwrap();
        if connection.execute("DELETE FROM agents WHERE id=?1", [id])? == 0 {
            return Err(AccessError::NotFound);
        }
        self.reload_relays(&connection)
    }

    pub fn relay_for_token(&self, hash: &[u8; 32]) -> Option<i64> {
        self.relays.read().unwrap().get(hash).copied()
    }

    pub fn agent_hashes(&self, relay_id: i64) -> Result<Vec<String>> {
        let connection = self.connection.lock().unwrap();
        let mut statement = connection
            .prepare("SELECT token_hash FROM agents WHERE relay_id=?1 ORDER BY token_hash")?;
        let hashes = statement
            .query_map([relay_id], |row| row.get::<_, [u8; 32]>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(hashes.into_iter().map(hex::encode).collect())
    }
}

fn valid_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 64 || name.chars().any(char::is_control) {
        return Err(AccessError::InvalidName);
    }
    Ok(name.into())
}

fn credentials(prefix: &str) -> Result<(String, [u8; 32], String)> {
    let mut bytes = [0u8; 32];
    OsRng.try_fill_bytes(&mut bytes).map_err(|error| {
        AccessError::Storage(anyhow::anyhow!("token generation failed: {error}"))
    })?;
    let token = format!("{prefix}{}", hex::encode(bytes));
    let hash = Sha256::digest(token.as_bytes()).into();
    let created_at = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|error| AccessError::Storage(error.into()))?;
    Ok((token, hash, created_at))
}
