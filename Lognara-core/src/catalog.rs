//! Каталог и checkpoint фиксируются одной FULL-синхронной транзакцией SQLite.
use crate::{
    definitions::Definition,
    series::SeriesFileMeta,
    wal::{Position, Receipt, sync_dir},
};
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SegmentMeta {
    pub version: u32,
    pub id: String,
    pub path: String,
    pub rows: usize,
    pub first_sequence: u64,
    pub last_sequence: u64,
    pub min_timestamp: i64,
    pub max_timestamp: i64,
    pub max_received_at: i64,
    pub uncompressed_bytes: usize,
    pub closed_at: i64,
    pub checkpoint: Position,
}

/// Имя метрики в сервисе с origin; first_seen и last_seen - время точек.
#[derive(Debug, Clone, PartialEq)]
pub struct SeriesName {
    pub service: String,
    pub name: String,
    pub origin: String,
    pub first_seen: i64,
    pub last_seen: i64,
}

/// Окружение метрики: (service, name, environment).
pub type SeriesEnvironment = (String, String, String);

pub struct Catalog {
    connection: Connection,
}

impl Catalog {
    pub fn open(path: &Path) -> Result<Self> {
        let connection = Connection::open(path)?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        let version: u32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        ensure!(version <= 2, "unsupported catalog schema version {version}");
        connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS progress (singleton INTEGER PRIMARY KEY CHECK(singleton=1), batch_id INTEGER NOT NULL, offset INTEGER NOT NULL, next_batch INTEGER NOT NULL, next_sequence INTEGER NOT NULL);
            INSERT OR IGNORE INTO progress VALUES(1,0,0,1,1);
            CREATE TABLE IF NOT EXISTS segments (id TEXT PRIMARY KEY, metadata TEXT NOT NULL, state TEXT NOT NULL CHECK(state IN ('ready','deleting')));
            CREATE TABLE IF NOT EXISTS receipts (hash BLOB PRIMARY KEY, batch_id INTEGER NOT NULL, first_sequence INTEGER NOT NULL, events INTEGER NOT NULL, received_at INTEGER NOT NULL, body_bytes INTEGER NOT NULL);
            CREATE INDEX IF NOT EXISTS receipts_age ON receipts(received_at);
            CREATE TABLE IF NOT EXISTS series_files (id TEXT PRIMARY KEY, metadata TEXT NOT NULL, state TEXT NOT NULL CHECK(state IN ('ready','deleting')));
            CREATE TABLE IF NOT EXISTS series_names (service TEXT NOT NULL, name TEXT NOT NULL, origin TEXT NOT NULL, first_seen INTEGER NOT NULL, last_seen INTEGER NOT NULL, PRIMARY KEY(service, name, origin));
            CREATE TABLE IF NOT EXISTS series_environments (service TEXT NOT NULL, name TEXT NOT NULL, environment TEXT NOT NULL, PRIMARY KEY(service, name, environment));
            CREATE TABLE IF NOT EXISTS series_definitions (id TEXT PRIMARY KEY, service TEXT NOT NULL, name TEXT NOT NULL, body TEXT NOT NULL, UNIQUE(service, name));
            PRAGMA user_version=2;")?;
        sync_dir(path.parent().expect("catalog has a parent"))?;
        Ok(Self { connection })
    }

    pub fn progress(&self) -> Result<(Position, u64, u64)> {
        Ok(self.connection.query_row(
            "SELECT batch_id,offset,next_batch,next_sequence FROM progress WHERE singleton=1",
            [],
            |row| {
                Ok((
                    Position {
                        batch_id: row.get(0)?,
                        offset: row.get(1)?,
                    },
                    row.get(2)?,
                    row.get(3)?,
                ))
            },
        )?)
    }

    pub fn segments(&self, state: &str) -> Result<Vec<SegmentMeta>> {
        let mut statement = self
            .connection
            .prepare("SELECT metadata FROM segments WHERE state=?1 ORDER BY id")?;
        let json = statement
            .query_map([state], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        json.into_iter()
            .map(|json| {
                let meta: SegmentMeta = serde_json::from_str(&json)?;
                ensure!(meta.version == 1, "unsupported segment version");
                Ok(meta)
            })
            .collect()
    }

    pub fn series_files(&self, state: &str) -> Result<Vec<SeriesFileMeta>> {
        let mut statement = self
            .connection
            .prepare("SELECT metadata FROM series_files WHERE state=?1 ORDER BY id")?;
        let json = statement
            .query_map([state], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        json.into_iter()
            .map(|json| {
                let meta: SeriesFileMeta = serde_json::from_str(&json)?;
                ensure!(meta.version == 1, "unsupported series version");
                Ok(meta)
            })
            .collect()
    }

    pub fn series_names(&self) -> Result<Vec<SeriesName>> {
        let mut statement = self.connection.prepare(
            "SELECT service,name,origin,first_seen,last_seen FROM series_names ORDER BY service,name,origin",
        )?;
        Ok(statement
            .query_map([], |row| {
                Ok(SeriesName {
                    service: row.get(0)?,
                    name: row.get(1)?,
                    origin: row.get(2)?,
                    first_seen: row.get(3)?,
                    last_seen: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?)
    }

    /// Окружения метрик (service, name, environment) из точек, сохранённых при запечатывании.
    pub fn series_environments(&self) -> Result<Vec<SeriesEnvironment>> {
        let mut statement = self.connection.prepare(
            "SELECT service,name,environment FROM series_environments ORDER BY service,name,environment",
        )?;
        Ok(statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect::<Result<Vec<_>, _>>()?)
    }

    /// Есть ли у (service, name) native-точки counter или gauge; точки правил не считаются.
    pub fn has_native_name(&self, service: &str, name: &str) -> Result<bool> {
        Ok(self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM series_names WHERE service=?1 AND name=?2 AND origin IN ('counter','gauge'))",
            params![service, name],
            |row| row.get(0),
        )?)
    }

    pub fn definitions(&self) -> Result<Vec<Definition>> {
        let mut statement = self
            .connection
            .prepare("SELECT body FROM series_definitions ORDER BY service,name")?;
        let json = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        json.iter()
            .map(|json| Ok(serde_json::from_str(json)?))
            .collect()
    }

    pub fn insert_definition(&self, definition: &Definition) -> Result<()> {
        self.connection.execute(
            "INSERT INTO series_definitions VALUES(?1,?2,?3,?4)",
            params![
                definition.id.to_string(),
                definition.service,
                definition.name,
                serde_json::to_string(definition)?
            ],
        )?;
        Ok(())
    }

    /// false - определения с таким id нет.
    pub fn update_definition(&self, definition: &Definition) -> Result<bool> {
        Ok(self.connection.execute(
            "UPDATE series_definitions SET service=?2,name=?3,body=?4 WHERE id=?1",
            params![
                definition.id.to_string(),
                definition.service,
                definition.name,
                serde_json::to_string(definition)?
            ],
        )? == 1)
    }

    pub fn delete_definition(&self, id: &str) -> Result<bool> {
        Ok(self
            .connection
            .execute("DELETE FROM series_definitions WHERE id=?1", [id])?
            == 1)
    }

    pub fn receipt(&self, hash: &[u8; 32]) -> Result<Option<Receipt>> {
        Ok(self.connection.query_row("SELECT batch_id,first_sequence,events,received_at,body_bytes FROM receipts WHERE hash=?1", [hash.as_slice()], |row| {
            Ok(Receipt { hash: *hash, batch_id: row.get(0)?, first_sequence: row.get(1)?, events: row.get(2)?, received_at: row.get(3)?, body_bytes: row.get(4)? })
        }).optional()?)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn publish(
        &mut self,
        segment: Option<&SegmentMeta>,
        series: Option<&SeriesFileMeta>,
        names: &[SeriesName],
        environments: &[SeriesEnvironment],
        checkpoint: Position,
        receipts: &[Receipt],
        next_ids: (u64, u64),
    ) -> Result<()> {
        let transaction = self.connection.transaction()?;
        if let Some(meta) = segment {
            transaction.execute(
                "INSERT INTO segments VALUES(?1,?2,'ready')",
                params![meta.id, serde_json::to_string(meta)?],
            )?;
        }
        if let Some(meta) = series {
            transaction.execute(
                "INSERT INTO series_files VALUES(?1,?2,'ready')",
                params![meta.id, serde_json::to_string(meta)?],
            )?;
        }
        for name in names {
            transaction.execute(
                "INSERT INTO series_names VALUES(?1,?2,?3,?4,?5)
                ON CONFLICT(service,name,origin) DO UPDATE SET first_seen=min(first_seen,excluded.first_seen),last_seen=max(last_seen,excluded.last_seen)",
                params![
                    name.service,
                    name.name,
                    name.origin,
                    name.first_seen,
                    name.last_seen
                ],
            )?;
        }
        for (service, name, environment) in environments {
            transaction.execute(
                "INSERT OR IGNORE INTO series_environments VALUES(?1,?2,?3)",
                params![service, name, environment],
            )?;
        }
        for receipt in receipts {
            transaction.execute(
                "INSERT OR IGNORE INTO receipts VALUES(?1,?2,?3,?4,?5,?6)",
                params![
                    receipt.hash.as_slice(),
                    receipt.batch_id,
                    receipt.first_sequence,
                    receipt.events,
                    receipt.received_at,
                    receipt.body_bytes
                ],
            )?;
        }
        transaction.execute("UPDATE progress SET batch_id=?1,offset=?2,next_batch=?3,next_sequence=?4 WHERE singleton=1",
            params![checkpoint.batch_id, checkpoint.offset, next_ids.0, next_ids.1])?;
        transaction.commit()?;
        Ok(())
    }

    /// Слияние серий: новый файл становится ready, исходные - deleting, одной транзакцией.
    pub fn replace_series(&mut self, new: &SeriesFileMeta, old_ids: &[String]) -> Result<()> {
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "INSERT INTO series_files VALUES(?1,?2,'ready')",
            params![new.id, serde_json::to_string(new)?],
        )?;
        for id in old_ids {
            transaction.execute("UPDATE series_files SET state='deleting' WHERE id=?1", [id])?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn mark_deleting(&mut self, ids: &[String]) -> Result<()> {
        let transaction = self.connection.transaction()?;
        for id in ids {
            transaction.execute("UPDATE segments SET state='deleting' WHERE id=?1", [id])?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn finish_deleting(&self, id: &str) -> Result<()> {
        self.connection.execute(
            "DELETE FROM segments WHERE id=?1 AND state='deleting'",
            [id],
        )?;
        Ok(())
    }

    pub fn finish_deleting_series(&self, id: &str) -> Result<()> {
        self.connection.execute(
            "DELETE FROM series_files WHERE id=?1 AND state='deleting'",
            [id],
        )?;
        Ok(())
    }

    pub fn prune_receipts(&self, before: i64, first_retained: u64) -> Result<()> {
        self.connection.execute(
            "DELETE FROM receipts WHERE received_at<?1 AND first_sequence+events<=?2",
            params![before, first_retained],
        )?;
        Ok(())
    }
}
