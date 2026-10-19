//! Индекс источников и серий по серверам; восстановление старого каталога до готовности core.
use crate::{
    catalog::Catalog,
    definitions::{Definition, RuleSet},
    journal::Journal,
    model::StoredEvent,
    series::Point,
};
use anyhow::{Result, ensure};
use datafusion::arrow::array::{Array, RecordBatch, StringArray, TimestampNanosecondArray};
use parquet::arrow::{ProjectionMask, arrow_reader::ParquetRecordBatchReaderBuilder};
use rusqlite::params;
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    path::Path,
};

pub type ScopedName = (String, String, String, String, String);

#[derive(Clone, Default)]
pub struct Metadata {
    pub sources: BTreeSet<(String, String)>,
    pub names: BTreeMap<ScopedName, (i64, i64)>,
}

#[derive(Serialize)]
pub struct Source {
    pub server: String,
    pub service: String,
}

impl Metadata {
    pub fn rows(&mut self, rows: &[StoredEvent]) {
        self.sources.extend(
            rows.iter()
                .map(|r| (r.source.server.clone(), r.source.service.clone())),
        );
    }

    pub fn point(&mut self, point: &Point) {
        self.observe(
            point.source.server.clone(),
            point.source.service.clone(),
            point.name.clone(),
            point.origin.as_str().into(),
            point.source.environment.clone().unwrap_or_default(),
            point.timestamp,
        );
    }

    fn observe(
        &mut self,
        server: String,
        service: String,
        name: String,
        origin: String,
        environment: String,
        ts: i64,
    ) {
        self.sources.insert((server.clone(), service.clone()));
        self.names
            .entry((server, service, name, origin, environment))
            .and_modify(|(first, last)| {
                *first = (*first).min(ts);
                *last = (*last).max(ts);
            })
            .or_insert((ts, ts));
    }

    pub fn save(&self, catalog: &Catalog) -> Result<()> {
        let tx = catalog.connection.unchecked_transaction()?;
        for (server, service) in &self.sources {
            tx.execute(
                "INSERT OR IGNORE INTO metric_sources VALUES(?1,?2)",
                params![server, service],
            )?;
        }
        for ((server, service, name, origin, environment), (first, last)) in &self.names {
            tx.execute("INSERT INTO scoped_series VALUES(?1,?2,?3,?4,?5,?6,?7)
                ON CONFLICT(server,service,name,origin,environment) DO UPDATE SET first_seen=MIN(first_seen,excluded.first_seen),last_seen=MAX(last_seen,excluded.last_seen)",
                params![server,service,name,origin,environment,first,last])?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn load(catalog: &Catalog) -> Result<Self> {
        let mut result = Self::default();
        let mut statement = catalog
            .connection
            .prepare("SELECT server,service FROM metric_sources")?;
        for row in statement.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))? {
            result.sources.insert(row?);
        }
        let mut statement = catalog.connection.prepare(
            "SELECT server,service,name,origin,environment,first_seen,last_seen FROM scoped_series",
        )?;
        for row in statement.query_map([], |r| {
            Ok((
                (r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?),
                (r.get(5)?, r.get(6)?),
            ))
        })? {
            let (key, value) = row?;
            result.names.insert(key, value);
        }
        Ok(result)
    }

    pub fn merge(&mut self, other: &Self) {
        self.sources.extend(other.sources.iter().cloned());
        for ((server, service, name, origin, env), (first, last)) in &other.names {
            self.observe(
                server.clone(),
                service.clone(),
                name.clone(),
                origin.clone(),
                env.clone(),
                *first,
            );
            self.observe(
                server.clone(),
                service.clone(),
                name.clone(),
                origin.clone(),
                env.clone(),
                *last,
            );
        }
    }

    fn batch(&mut self, batch: &RecordBatch, metrics: bool) -> Result<()> {
        let strings = |name: &str| -> Result<&StringArray> {
            batch
                .column_by_name(name)
                .and_then(|a| a.as_any().downcast_ref())
                .ok_or_else(|| anyhow::anyhow!("missing {name}"))
        };
        let servers = strings("server")?;
        let services = strings("service")?;
        for row in 0..batch.num_rows() {
            let server = servers.value(row).to_owned();
            let service = services.value(row).to_owned();
            self.sources.insert((server.clone(), service.clone()));
            if metrics {
                let env = strings("environment")?;
                let ts = batch
                    .column_by_name("timestamp")
                    .and_then(|a| a.as_any().downcast_ref::<TimestampNanosecondArray>())
                    .ok_or_else(|| anyhow::anyhow!("missing timestamp"))?;
                self.observe(
                    server,
                    service,
                    strings("name")?.value(row).into(),
                    strings("origin")?.value(row).into(),
                    if env.is_null(row) {
                        String::new()
                    } else {
                        env.value(row).into()
                    },
                    ts.value(row),
                );
            }
        }
        Ok(())
    }
}

fn backfill_file(catalog: &Catalog, root: &Path, path: &str, metrics: bool) -> Result<()> {
    let done: bool = catalog.connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM metrics_backfill WHERE path=?1)",
        [path],
        |r| r.get(0),
    )?;
    if done {
        return Ok(());
    }
    let builder = ParquetRecordBatchReaderBuilder::try_new(File::open(root.join(path))?)?;
    let columns = if metrics {
        vec![
            "server",
            "service",
            "name",
            "origin",
            "environment",
            "timestamp",
        ]
    } else {
        vec!["server", "service"]
    };
    let indices = columns
        .iter()
        .map(|name| builder.schema().index_of(name))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mask = ProjectionMask::roots(builder.parquet_schema(), indices);
    // Bounded batches; progress is durable per file. Replaying an unfinished file is idempotent.
    for batch in builder
        .with_projection(mask)
        .with_batch_size(4096)
        .build()?
    {
        let mut metadata = Metadata::default();
        metadata.batch(&batch?, metrics)?;
        metadata.save(catalog)?;
    }
    catalog
        .connection
        .execute("INSERT INTO metrics_backfill VALUES(?1)", [path])?;
    Ok(())
}

pub fn heartbeat_sources(journal: &Journal, metadata: &mut Metadata) -> Result<()> {
    for agent in journal.access.list_agents()? {
        if let Some(source) = agent.heartbeat.as_ref().and_then(|h| h.get("source"))
            && let (Some(server), Some(service)) = (
                source.get("server").and_then(|v| v.as_str()),
                source.get("service").and_then(|v| v.as_str()),
            )
        {
            metadata.sources.insert((server.into(), service.into()));
        }
    }
    Ok(())
}

pub fn migrate(journal: &Journal) -> Result<()> {
    let catalog = journal.catalog.lock().unwrap();
    let done: bool = catalog.connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM metrics_migration)",
        [],
        |r| r.get(0),
    )?;
    if done {
        return Ok(());
    }
    for segment in catalog.segments("ready")? {
        backfill_file(
            &catalog,
            &journal.config.data_dir,
            &format!("{}/logs.parquet", segment.path),
            false,
        )?;
    }
    for file in catalog.series_files("ready")? {
        backfill_file(&catalog, &journal.config.data_dir, &file.path, true)?;
    }
    let mut metadata = Metadata::load(&catalog)?;
    heartbeat_sources(journal, &mut metadata)?;
    {
        let wal = journal.wal.lock().unwrap();
        let (mut position, _, _) = catalog.progress()?;
        while let Some(receipt) = wal.after(position) {
            let body = wal.read(&receipt)?;
            let batch = crate::wire::decode_with_budget(
                &body,
                journal.config.max_decoded_bytes,
                journal.config.max_model_bytes,
            )?;
            for group in batch.groups {
                metadata
                    .sources
                    .insert((group.source.server, group.source.service));
            }
            position = receipt.end();
        }
    }
    metadata.save(&catalog)?;
    let mut statement = catalog
        .connection
        .prepare("SELECT body FROM legacy_series_definitions")?;
    let legacy = statement
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut definitions = vec![];
    for json in legacy {
        let original: Definition = serde_json::from_str(&json)?;
        for (server, service) in &metadata.sources {
            if service == &original.service {
                let mut copy = original.clone();
                copy.server = server.clone();
                copy.legacy_rule_id = Some(original.id.to_string());
                copy.id = uuid::Uuid::new_v4();
                definitions.push(copy);
            }
        }
    }
    ensure!(
        definitions.iter().filter(|d| d.rule.is_some()).count() <= 64,
        "server migration exceeds 64 metric rules; reduce legacy rules before upgrading"
    );
    RuleSet::load(&definitions)?;
    let tx = catalog.connection.unchecked_transaction()?;
    for d in definitions {
        tx.execute(
            "INSERT INTO series_definitions VALUES(?1,?2,?3,?4,?5)",
            params![
                d.id.to_string(),
                d.server,
                d.service,
                d.name,
                serde_json::to_string(&d)?
            ],
        )?;
    }
    tx.execute_batch(
        "DROP TABLE legacy_series_definitions; INSERT INTO metrics_migration VALUES(1);",
    )?;
    tx.commit()?;
    Ok(())
}

pub async fn sources(
    core: std::sync::Arc<crate::storage::Core>,
) -> Result<Vec<Source>, crate::query::QueryError> {
    let snapshot = core.snapshot();
    let mut metadata = tokio::task::spawn_blocking(move || -> Result<Metadata> {
        let mut heartbeat = Metadata::default();
        heartbeat_sources(&core.journal, &mut heartbeat)?;
        let catalog = core.journal.catalog.lock().unwrap();
        heartbeat.save(&catalog)?;
        Metadata::load(&catalog)
    })
    .await
    .map_err(|e| crate::query::QueryError::Storage(e.into()))??;
    if let Some(active) = &snapshot.active {
        metadata.merge(&active.scoped);
    }
    Ok(metadata
        .sources
        .into_iter()
        .map(|(server, service)| Source { server, service })
        .collect())
}

pub async fn list(
    core: std::sync::Arc<crate::storage::Core>,
) -> Result<crate::series_query::ListResponse, crate::query::QueryError> {
    use crate::{
        model::format_timestamp,
        series_query::{ListResponse, Metric},
    };
    let snapshot = core.snapshot();
    let (mut metadata, definitions) = tokio::task::spawn_blocking(move || -> Result<_> {
        let catalog = core.journal.catalog.lock().unwrap();
        Ok((Metadata::load(&catalog)?, catalog.definitions()?))
    })
    .await
    .map_err(|e| crate::query::QueryError::Storage(e.into()))??;
    if let Some(active) = &snapshot.active {
        metadata.merge(&active.scoped);
    }
    let mut metrics: BTreeMap<(String, String, String, String), Metric> = BTreeMap::new();
    let mut ranges: BTreeMap<(String, String, String, String), (i64, i64)> = BTreeMap::new();
    for ((server, service, name, origin, environment), (first, last)) in metadata.names {
        let item = metrics
            .entry((
                server.clone(),
                service.clone(),
                name.clone(),
                origin.clone(),
            ))
            .or_insert_with(|| Metric {
                server: Some(server),
                service,
                name,
                origin,
                first_seen: Some(format_timestamp(first)),
                last_seen: Some(format_timestamp(last)),
                definition: None,
                environments: vec![],
            });
        let range = ranges
            .entry((
                item.server.clone().unwrap(),
                item.service.clone(),
                item.name.clone(),
                item.origin.clone(),
            ))
            .or_insert((first, last));
        range.0 = range.0.min(first);
        range.1 = range.1.max(last);
        item.first_seen = Some(format_timestamp(range.0));
        item.last_seen = Some(format_timestamp(range.1));
        if !environment.is_empty() {
            item.environments.push(environment);
        }
    }
    for definition in definitions {
        let origin = if definition.rule.is_some() {
            "rule"
        } else if definition.kind == crate::definitions::Kind::Gauge {
            "gauge"
        } else {
            "counter"
        };
        let mut found = false;
        for ((server, service, name, _), metric) in &mut metrics {
            if server == &definition.server
                && service == &definition.service
                && name == &definition.name
            {
                metric.definition = Some(definition.clone());
                found = true;
            }
        }
        if !found {
            metrics.insert(
                (
                    definition.server.clone(),
                    definition.service.clone(),
                    definition.name.clone(),
                    origin.into(),
                ),
                Metric {
                    server: Some(definition.server.clone()),
                    service: definition.service.clone(),
                    name: definition.name.clone(),
                    origin: origin.into(),
                    first_seen: None,
                    last_seen: None,
                    environments: vec![],
                    definition: Some(definition),
                },
            );
        }
    }
    Ok(ListResponse {
        metrics: metrics.into_values().collect(),
    })
}
