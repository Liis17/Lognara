//! Персональные учётные данные. Plaintext токена существует только при создании или перевыпуске.
use std::{
    collections::HashMap,
    path::Path,
    sync::{Mutex, RwLock},
};

use rand::{TryRngCore, rngs::OsRng};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{
    model::now_nanos,
    telemetry::{RelayHeartbeat, before},
};

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
    /// Relay и агент хоста управляются только через hosts API.
    #[error("host_node")]
    HostNode,
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
    /// `relay` или `host` (хост-агент, отправляющий прямо в core).
    pub kind: String,
    pub created_at: String,
    /// Последний heartbeat; поле отсутствует, пока heartbeat не приходил.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heartbeat: Option<serde_json::Value>,
}
#[derive(Clone, Serialize)]
pub struct Agent {
    pub id: i64,
    pub name: String,
    pub relay_id: i64,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heartbeat: Option<serde_json::Value>,
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
/// Хост: `relay.kind == "host"` и его встроенный агент с тем же токеном.
#[derive(Serialize)]
pub struct CreatedHost {
    pub relay: Relay,
    pub agent: Agent,
    pub token: String,
}

pub struct Access {
    connection: Mutex<Connection>,
    relays: RwLock<HashMap<[u8; 32], i64>>,
    agent_nodes: RwLock<HashMap<[u8; 32], crate::model::Node>>,
}

impl Access {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let connection = Connection::open(path)?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        let version: u32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        anyhow::ensure!(version <= 3, "unsupported access schema version {version}");
        connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;
            CREATE TABLE IF NOT EXISTS relays(id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, token_hash BLOB NOT NULL UNIQUE, created_at TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS agents(id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, relay_id INTEGER NOT NULL REFERENCES relays(id), token_hash BLOB NOT NULL UNIQUE, created_at TEXT NOT NULL, UNIQUE(relay_id,name));
            CREATE TABLE IF NOT EXISTS heartbeats(kind TEXT NOT NULL CHECK(kind IN ('relay','agent')), node_id INTEGER NOT NULL, received_at INTEGER NOT NULL, telemetry TEXT NOT NULL, PRIMARY KEY(kind,node_id));
            CREATE TABLE IF NOT EXISTS retired_agent_tokens(token_hash BLOB PRIMARY KEY, agent_id INTEGER NOT NULL REFERENCES agents(id) ON DELETE CASCADE);")?;
        // Версия 3: хост-агент - relay вида host с единственным встроенным агентом того же токена.
        let has_kind: bool = connection.query_row(
            "SELECT count(*) FROM pragma_table_info('relays') WHERE name='kind'",
            [],
            |row| row.get(0),
        )?;
        if !has_kind {
            connection.execute_batch(
                "BEGIN;
                 ALTER TABLE relays ADD COLUMN kind TEXT NOT NULL DEFAULT 'relay' CHECK(kind IN ('relay','host'));
                 PRAGMA user_version=3;
                 COMMIT;",
            )?;
        } else {
            connection.execute_batch("PRAGMA user_version=3;")?;
        }
        crate::wal::sync_dir(path.parent().expect("access has a parent"))?;
        let access = Self {
            connection: Mutex::new(connection),
            relays: RwLock::new(HashMap::new()),
            agent_nodes: RwLock::new(HashMap::new()),
        };
        access.reload_tokens(&access.connection.lock().unwrap())?;
        Ok(access)
    }

    fn reload_tokens(&self, connection: &Connection) -> Result<()> {
        let mut statement = connection.prepare("SELECT token_hash,id FROM relays")?;
        let relays = statement
            .query_map([], |row| {
                Ok((row.get::<_, [u8; 32]>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<std::result::Result<HashMap<_, _>, _>>()?;
        *self.relays.write().unwrap() = relays;
        let mut statement = connection.prepare(
            "SELECT token_hash,id,relay_id FROM agents
             UNION ALL
             SELECT retired.token_hash,agents.id,agents.relay_id
             FROM retired_agent_tokens AS retired JOIN agents ON agents.id=retired.agent_id",
        )?;
        let agent_nodes = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, [u8; 32]>(0)?,
                    crate::model::Node {
                        agent_id: row.get(1)?,
                        relay_id: row.get(2)?,
                    },
                ))
            })?
            .collect::<std::result::Result<HashMap<_, _>, _>>()?;
        *self.agent_nodes.write().unwrap() = agent_nodes;
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
            kind: "relay".into(),
            created_at,
            heartbeat: None,
        };
        self.reload_tokens(&connection)?;
        Ok(CreatedRelay { relay, token })
    }

    /// Хост-агент отправляет пачки прямо в core: он аутентифицируется как relay и
    /// подписывает группы хешем того же токена, поэтому события получают relay_id/agent_id.
    pub fn create_host(&self, name: &str) -> Result<CreatedHost> {
        let name = valid_name(name)?;
        let mut connection = self.connection.lock().unwrap();
        let transaction = connection.transaction()?;
        if transaction
            .query_row("SELECT id FROM relays WHERE name=?1", [&name], |row| {
                row.get::<_, i64>(0)
            })
            .optional()?
            .is_some()
        {
            return Err(AccessError::NameTaken);
        }
        let (token, hash, created_at) = credentials("lnh_")?;
        transaction.execute(
            "INSERT INTO relays(name,token_hash,created_at,kind) VALUES(?1,?2,?3,'host')",
            params![name, hash.as_slice(), created_at],
        )?;
        let relay_id = transaction.last_insert_rowid();
        transaction.execute(
            "INSERT INTO agents(name,relay_id,token_hash,created_at) VALUES(?1,?2,?3,?4)",
            params![name, relay_id, hash.as_slice(), created_at],
        )?;
        let agent_id = transaction.last_insert_rowid();
        transaction.commit()?;
        self.reload_tokens(&connection)?;
        Ok(CreatedHost {
            relay: Relay {
                id: relay_id,
                name: name.clone(),
                kind: "host".into(),
                created_at: created_at.clone(),
                heartbeat: None,
            },
            agent: Agent {
                id: agent_id,
                name,
                relay_id,
                created_at,
                heartbeat: None,
            },
            token,
        })
    }

    /// Новый токен хоста; relay_id/agent_id сохраняются. Старый токен сразу перестаёт
    /// авторизовать, а его хеш в уже подготовленных пачках относится к тому же узлу.
    pub fn rotate_host_token(&self, relay_id: i64) -> Result<CreatedHost> {
        let mut connection = self.connection.lock().unwrap();
        let transaction = connection.transaction()?;
        let (relay, agent, old_hash) = transaction
            .query_row(
                "SELECT r.id,r.name,r.created_at,rh.telemetry,a.id,a.name,a.created_at,ah.telemetry,a.token_hash
                 FROM relays r JOIN agents a ON a.relay_id=r.id
                 LEFT JOIN heartbeats rh ON rh.kind='relay' AND rh.node_id=r.id
                 LEFT JOIN heartbeats ah ON ah.kind='agent' AND ah.node_id=a.id
                 WHERE r.id=?1 AND r.kind='host'",
                [relay_id],
                |row| {
                    Ok((
                        Relay {
                            id: row.get(0)?,
                            name: row.get(1)?,
                            kind: "host".into(),
                            created_at: row.get(2)?,
                            heartbeat: telemetry(row.get(3)?),
                        },
                        Agent {
                            id: row.get(4)?,
                            name: row.get(5)?,
                            relay_id,
                            created_at: row.get(6)?,
                            heartbeat: telemetry(row.get(7)?),
                        },
                        row.get::<_, [u8; 32]>(8)?,
                    ))
                },
            )
            .optional()?
            .ok_or(AccessError::NotFound)?;
        let (token, hash, _) = credentials("lnh_")?;
        transaction.execute(
            "INSERT INTO retired_agent_tokens(token_hash,agent_id) VALUES(?1,?2)",
            params![old_hash.as_slice(), agent.id],
        )?;
        transaction.execute(
            "UPDATE agents SET token_hash=?1 WHERE id=?2",
            params![hash.as_slice(), agent.id],
        )?;
        transaction.execute(
            "UPDATE relays SET token_hash=?1 WHERE id=?2",
            params![hash.as_slice(), relay_id],
        )?;
        transaction.commit()?;
        self.reload_tokens(&connection)?;
        Ok(CreatedHost {
            relay,
            agent,
            token,
        })
    }

    pub fn delete_host(&self, relay_id: i64) -> Result<()> {
        let mut connection = self.connection.lock().unwrap();
        let transaction = connection.transaction()?;
        let agent_id = transaction
            .query_row(
                "SELECT a.id FROM relays r JOIN agents a ON a.relay_id=r.id WHERE r.id=?1 AND r.kind='host'",
                [relay_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .ok_or(AccessError::NotFound)?;
        transaction.execute("DELETE FROM agents WHERE id=?1", [agent_id])?;
        transaction.execute("DELETE FROM relays WHERE id=?1", [relay_id])?;
        transaction.execute(
            "DELETE FROM heartbeats WHERE (kind='agent' AND node_id=?1) OR (kind='relay' AND node_id=?2)",
            params![agent_id, relay_id],
        )?;
        transaction.commit()?;
        self.reload_tokens(&connection)
    }

    fn relay_kind(connection: &Connection, relay_id: i64) -> Result<Option<String>> {
        Ok(connection
            .query_row("SELECT kind FROM relays WHERE id=?1", [relay_id], |row| {
                row.get(0)
            })
            .optional()?)
    }

    fn agent_on_host(connection: &Connection, agent_id: i64) -> Result<bool> {
        Ok(connection
            .query_row(
                "SELECT 1 FROM agents a JOIN relays r ON r.id=a.relay_id WHERE a.id=?1 AND r.kind='host'",
                [agent_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    pub fn create_agent(&self, name: &str, relay_id: i64) -> Result<CreatedAgent> {
        let name = valid_name(name)?;
        let connection = self.connection.lock().unwrap();
        match Self::relay_kind(&connection, relay_id)?.as_deref() {
            None => return Err(AccessError::RelayNotFound),
            Some("host") => return Err(AccessError::HostNode),
            Some(_) => {}
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
            heartbeat: None,
        };
        self.reload_tokens(&connection)?;
        Ok(CreatedAgent { agent, token })
    }

    pub fn rotate_agent_token(&self, id: i64) -> Result<CreatedAgent> {
        let mut connection = self.connection.lock().unwrap();
        if Self::agent_on_host(&connection, id)? {
            return Err(AccessError::HostNode);
        }
        let transaction = connection.transaction()?;
        let (agent, old_hash) = transaction
            .query_row(
                "SELECT id,name,relay_id,created_at,telemetry,token_hash FROM agents
                 LEFT JOIN heartbeats ON kind='agent' AND node_id=id WHERE id=?1",
                [id],
                |row| {
                    Ok((
                        Agent {
                            id: row.get(0)?,
                            name: row.get(1)?,
                            relay_id: row.get(2)?,
                            created_at: row.get(3)?,
                            heartbeat: telemetry(row.get(4)?),
                        },
                        row.get::<_, [u8; 32]>(5)?,
                    ))
                },
            )
            .optional()?
            .ok_or(AccessError::NotFound)?;
        let (token, hash, _) = credentials("lna_")?;
        transaction.execute(
            "INSERT INTO retired_agent_tokens(token_hash,agent_id) VALUES(?1,?2)",
            params![old_hash.as_slice(), id],
        )?;
        transaction.execute(
            "UPDATE agents SET token_hash=?1 WHERE id=?2",
            params![hash.as_slice(), id],
        )?;
        transaction.commit()?;
        self.reload_tokens(&connection)?;
        Ok(CreatedAgent { agent, token })
    }

    pub fn list_relays(&self) -> Result<Vec<Relay>> {
        let connection = self.connection.lock().unwrap();
        let mut statement = connection.prepare(
            "SELECT relays.id,relays.name,relays.created_at,heartbeats.telemetry,relays.kind FROM relays
             LEFT JOIN heartbeats ON heartbeats.kind='relay' AND heartbeats.node_id=relays.id ORDER BY relays.id",
        )?;
        Ok(statement
            .query_map([], |row| {
                Ok(Relay {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    kind: row.get(4)?,
                    created_at: row.get(2)?,
                    heartbeat: telemetry(row.get(3)?),
                })
            })?
            .collect::<std::result::Result<_, _>>()?)
    }

    pub fn list_agents(&self) -> Result<Vec<Agent>> {
        let connection = self.connection.lock().unwrap();
        let mut statement = connection.prepare(
            "SELECT id,name,relay_id,created_at,telemetry FROM agents
             LEFT JOIN heartbeats ON kind='agent' AND node_id=id ORDER BY id",
        )?;
        Ok(statement
            .query_map([], |row| {
                Ok(Agent {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    relay_id: row.get(2)?,
                    created_at: row.get(3)?,
                    heartbeat: telemetry(row.get(4)?),
                })
            })?
            .collect::<std::result::Result<_, _>>()?)
    }

    pub fn delete_relay(&self, id: i64) -> Result<()> {
        let connection = self.connection.lock().unwrap();
        if Self::relay_kind(&connection, id)?.as_deref() == Some("host") {
            return Err(AccessError::HostNode);
        }
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
        connection.execute(
            "DELETE FROM heartbeats WHERE kind='relay' AND node_id=?1",
            [id],
        )?;
        self.reload_tokens(&connection)
    }

    pub fn delete_agent(&self, id: i64) -> Result<()> {
        let connection = self.connection.lock().unwrap();
        if Self::agent_on_host(&connection, id)? {
            return Err(AccessError::HostNode);
        }
        if connection.execute("DELETE FROM agents WHERE id=?1", [id])? == 0 {
            return Err(AccessError::NotFound);
        }
        connection.execute(
            "DELETE FROM heartbeats WHERE kind='agent' AND node_id=?1",
            [id],
        )?;
        self.reload_tokens(&connection)
    }

    pub fn relay_for_token(&self, hash: &[u8; 32]) -> Option<i64> {
        self.relays.read().unwrap().get(hash).copied()
    }

    pub fn agent_node(&self, hash: &crate::wire::AgentHash) -> Option<crate::model::Node> {
        self.agent_nodes.read().unwrap().get(&hash.0).copied()
    }

    pub fn agent_relay(&self, hash: &crate::wire::AgentHash) -> Option<i64> {
        self.agent_node(hash).map(|node| node.relay_id)
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

impl Access {
    /// Сохраняет heartbeat relay и его агентов одной транзакцией. Агент учитывается, только
    /// если хеш его токена зарегистрирован за этим relay; чужие и отозванные пропускаются.
    /// Более старый heartbeat не заменяет новый. Возвращает число принятых агентов.
    pub fn record_heartbeat(&self, relay_id: i64, heartbeat: RelayHeartbeat) -> Result<usize> {
        let now = now_nanos();
        let mut connection = self.connection.lock().unwrap();
        let transaction = connection.transaction()?;
        let upsert = |kind: &str, id: i64, received_at: i64, state: String| {
            transaction.execute(
                "INSERT INTO heartbeats VALUES(?1,?2,?3,?4) ON CONFLICT(kind,node_id) DO UPDATE
                 SET received_at=excluded.received_at, telemetry=excluded.telemetry
                 WHERE excluded.received_at>=heartbeats.received_at",
                params![kind, id, received_at, state],
            )
        };
        let state =
            serde_json::to_string(&heartbeat.relay.state(now)).map_err(anyhow::Error::from)?;
        upsert("relay", relay_id, now, state)?;
        let mut accepted = 0;
        for agent in heartbeat.agents {
            let Some(hash) = agent.hash() else { continue };
            let Some(id) = transaction
                .query_row(
                    "SELECT id FROM agents WHERE relay_id=?1 AND token_hash=?2",
                    params![relay_id, hash.as_slice()],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?
            else {
                continue;
            };
            let received_at = before(now, agent.heartbeat_ms_ago);
            let state = serde_json::to_string(&agent.telemetry.state(received_at))
                .map_err(anyhow::Error::from)?;
            upsert("agent", id, received_at, state)?;
            accepted += 1;
        }
        transaction.commit()?;
        Ok(accepted)
    }
}

fn telemetry(json: Option<String>) -> Option<serde_json::Value> {
    serde_json::from_str(&json?).ok()
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
