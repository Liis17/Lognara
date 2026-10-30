//! Read-only, paginated discovery of source names present in retained log segments.
use std::{collections::BTreeSet, fs::File, sync::Arc};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use datafusion::arrow::{
    array::{Array, StringArray, UInt64Array},
    record_batch::RecordBatch,
};
use hmac::{Hmac, Mac};
use parquet::arrow::{ProjectionMask, arrow_reader::ParquetRecordBatchReaderBuilder};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    model::{format_timestamp, now_nanos},
    query::QueryError,
    storage::{Core, Snapshot},
};

const DEFAULT_LIMIT: usize = 100;
const MAX_LIMIT: usize = 1000;
const CURSOR_TTL_NANOS: i64 = 600_000_000_000;

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceField {
    Service,
    Server,
    Environment,
}

impl SourceField {
    fn name(self) -> &'static str {
        match self {
            Self::Service => "service",
            Self::Server => "server",
            Self::Environment => "environment",
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceFilters {
    pub service: Option<String>,
    pub server: Option<String>,
    pub environment: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRequest {
    pub field: SourceField,
    #[serde(default)]
    pub filters: SourceFilters,
    #[serde(default = "default_limit")]
    pub limit: usize,
    #[serde(default)]
    pub cursor: Option<String>,
}

#[derive(Serialize)]
pub struct SourcePage {
    pub values: Vec<String>,
    pub next_cursor: Option<String>,
    pub as_of: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    version: u8,
    instance: Uuid,
    generation: u64,
    watermark: u64,
    query: [u8; 32],
    after: String,
    issued_at: i64,
    as_of: i64,
}

struct CancelOnDrop(CancellationToken);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

fn default_limit() -> usize {
    DEFAULT_LIMIT
}

pub async fn list(core: Arc<Core>, request: SourceRequest) -> Result<SourcePage, QueryError> {
    if !(1..=MAX_LIMIT).contains(&request.limit) {
        return Err(QueryError::Invalid(format!("limit must be 1..{MAX_LIMIT}")));
    }
    for value in [
        request.filters.service.as_deref(),
        request.filters.server.as_deref(),
        request.filters.environment.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        if value.len() > 4096 {
            return Err(QueryError::Invalid(
                "source filter exceeds 4096 bytes".into(),
            ));
        }
    }

    let query: [u8; 32] = Sha256::digest(
        serde_json::to_vec(&(request.field, &request.filters)).map_err(anyhow::Error::from)?,
    )
    .into();
    let snapshot = core.snapshot();
    let cursor = request
        .cursor
        .as_deref()
        .map(|text| decode_cursor(&core, text))
        .transpose()?;
    if let Some(cursor) = &cursor {
        if cursor.version != 1
            || cursor.instance != core.instance
            || cursor.generation != snapshot.generation
            || cursor.watermark > snapshot.watermark
            || cursor.issued_at > now_nanos()
            || now_nanos().saturating_sub(cursor.issued_at) > CURSOR_TTL_NANOS
        {
            return Err(QueryError::Gone);
        }
        if cursor.query != query {
            return Err(QueryError::Invalid(
                "cursor does not match source query".into(),
            ));
        }
    }

    let cancel = CancellationToken::new();
    let _cancel_on_drop = CancelOnDrop(cancel.clone());
    let field = request.field;
    let filters = request.filters;
    let limit = request.limit;
    let after = cursor.as_ref().map(|value| value.after.clone());
    let watermark = cursor
        .as_ref()
        .map_or(snapshot.watermark, |value| value.watermark);
    let scan_snapshot = snapshot.clone();
    let values = tokio::task::spawn_blocking(move || {
        scan(
            &scan_snapshot,
            field,
            &filters,
            after.as_deref(),
            limit,
            &cancel,
            watermark,
        )
    })
    .await
    .map_err(|error| QueryError::Storage(error.into()))??;

    let as_of = cursor
        .as_ref()
        .map_or(snapshot.published_at, |value| value.as_of);
    let has_more = values.len() > limit;
    let mut values = values;
    values.truncate(limit);
    let next_cursor = if has_more {
        let last = values.last().expect("a further page has a preceding value");
        Some(encode_cursor(
            &core,
            &Cursor {
                version: 1,
                instance: core.instance,
                generation: snapshot.generation,
                watermark,
                query,
                after: last.clone(),
                issued_at: cursor
                    .as_ref()
                    .map_or_else(now_nanos, |value| value.issued_at),
                as_of,
            },
        )?)
    } else {
        None
    };
    Ok(SourcePage {
        values,
        next_cursor,
        as_of: format_timestamp(as_of),
    })
}

fn scan(
    snapshot: &Snapshot,
    field: SourceField,
    filters: &SourceFilters,
    after: Option<&str>,
    limit: usize,
    cancel: &CancellationToken,
    watermark: u64,
) -> Result<Vec<String>, QueryError> {
    let capacity = limit + 1;
    let mut values = BTreeSet::new();
    for segment in &snapshot.closed {
        if cancel.is_cancelled() {
            return Err(QueryError::Timeout);
        }
        let path = segment.path.join("logs.parquet");
        let builder = ParquetRecordBatchReaderBuilder::try_new(
            File::open(path).map_err(anyhow::Error::from)?,
        )
        .map_err(anyhow::Error::from)?;
        let indices = ["server", "service", "environment", "sequence"]
            .iter()
            .map(|name| builder.schema().index_of(name))
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(anyhow::Error::from)?;
        let mask = ProjectionMask::roots(builder.parquet_schema(), indices);
        for batch in builder
            .with_projection(mask)
            .with_batch_size(4096)
            .build()
            .map_err(anyhow::Error::from)?
        {
            if cancel.is_cancelled() {
                return Err(QueryError::Timeout);
            }
            add_batch(
                &mut values,
                &batch.map_err(anyhow::Error::from)?,
                field,
                filters,
                after,
                capacity,
                watermark,
            )?;
        }
    }
    if let Some(active) = &snapshot.active {
        for batch in &active.batches {
            if cancel.is_cancelled() {
                return Err(QueryError::Timeout);
            }
            add_batch(
                &mut values,
                batch,
                field,
                filters,
                after,
                capacity,
                watermark,
            )?;
        }
    }
    Ok(values.into_iter().collect())
}

fn add_batch(
    values: &mut BTreeSet<String>,
    batch: &RecordBatch,
    field: SourceField,
    filters: &SourceFilters,
    after: Option<&str>,
    capacity: usize,
    watermark: u64,
) -> Result<(), QueryError> {
    let sequence = batch
        .column_by_name("sequence")
        .and_then(|array| array.as_any().downcast_ref::<UInt64Array>())
        .ok_or_else(|| QueryError::Invalid("missing source column sequence".into()))?;
    let server = strings(batch, "server")?;
    let service = strings(batch, "service")?;
    let environment = strings(batch, "environment")?;
    let selected = strings(batch, field.name())?;
    for row in 0..batch.num_rows() {
        if sequence.value(row) > watermark
            || filters
                .server
                .as_deref()
                .is_some_and(|v| server.value(row) != v)
            || filters
                .service
                .as_deref()
                .is_some_and(|v| service.value(row) != v)
            || filters
                .environment
                .as_deref()
                .is_some_and(|v| environment.is_null(row) || environment.value(row) != v)
            || selected.is_null(row)
        {
            continue;
        }
        let value = selected.value(row);
        if after.is_some_and(|after| value <= after) {
            continue;
        }
        if values.len() < capacity || values.last().is_some_and(|last| value < last.as_str()) {
            values.insert(value.to_owned());
            if values.len() > capacity {
                values.pop_last();
            }
        }
    }
    Ok(())
}

fn strings<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a StringArray, QueryError> {
    batch
        .column_by_name(name)
        .and_then(|array| array.as_any().downcast_ref())
        .ok_or_else(|| QueryError::Invalid(format!("missing source column {name}")))
}

fn encode_cursor(core: &Core, cursor: &Cursor) -> Result<String, QueryError> {
    let mut bytes = serde_json::to_vec(cursor).map_err(anyhow::Error::from)?;
    let mut mac = Hmac::<Sha256>::new_from_slice(&core.cursor_key).expect("HMAC accepts 32 bytes");
    mac.update(&bytes);
    bytes.extend_from_slice(&mac.finalize().into_bytes());
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn decode_cursor(core: &Core, text: &str) -> Result<Cursor, QueryError> {
    if text.len() > 4096 {
        return Err(QueryError::Gone);
    }
    let bytes = URL_SAFE_NO_PAD.decode(text).map_err(|_| QueryError::Gone)?;
    if bytes.len() < 32 {
        return Err(QueryError::Gone);
    }
    let (payload, signature) = bytes.split_at(bytes.len() - 32);
    let mut mac = Hmac::<Sha256>::new_from_slice(&core.cursor_key).expect("HMAC accepts 32 bytes");
    mac.update(payload);
    mac.verify_slice(signature).map_err(|_| QueryError::Gone)?;
    serde_json::from_slice(payload).map_err(|_| QueryError::Gone)
}

#[cfg(test)]
mod tests {
    use super::*;
    use datafusion::arrow::{
        array::StringArray,
        datatypes::{DataType, Field, Schema},
    };

    fn batch(rows: &[(&str, &str, Option<&str>)]) -> RecordBatch {
        RecordBatch::try_new(
            Arc::new(Schema::new(vec![
                Field::new("server", DataType::Utf8, false),
                Field::new("service", DataType::Utf8, false),
                Field::new("environment", DataType::Utf8, true),
                Field::new("sequence", DataType::UInt64, false),
            ])),
            vec![
                Arc::new(StringArray::from_iter_values(rows.iter().map(|r| r.0))),
                Arc::new(StringArray::from_iter_values(rows.iter().map(|r| r.1))),
                Arc::new(StringArray::from_iter(rows.iter().map(|r| r.2))),
                Arc::new(UInt64Array::from_iter_values(1..=rows.len() as u64)),
            ],
        )
        .unwrap()
    }

    #[test]
    fn snapshot_watermark_excludes_new_source_names() {
        let rows = batch(&[
            ("server", "app-a", None),
            ("server", "app-c", None),
            ("server", "app-b", None),
        ]);
        let mut values = BTreeSet::new();
        add_batch(
            &mut values,
            &rows,
            SourceField::Service,
            &SourceFilters::default(),
            Some("app-a"),
            100,
            2,
        )
        .unwrap();
        assert_eq!(values.into_iter().collect::<Vec<_>>(), ["app-c"]);
    }

    #[test]
    fn collects_exact_filtered_distinct_names_in_lexical_pages() {
        let rows = batch(&[
            ("srv-b", "app", Some("prod")),
            ("srv-a", "app", Some("prod")),
            ("srv-a", "app", Some("prod")),
            ("srv-z", "other", Some("prod")),
            ("srv-c", "app", None),
        ]);
        let mut values = BTreeSet::new();
        add_batch(
            &mut values,
            &rows,
            SourceField::Server,
            &SourceFilters {
                service: Some("app".into()),
                ..Default::default()
            },
            None,
            2,
            u64::MAX,
        )
        .unwrap();
        assert_eq!(values.into_iter().collect::<Vec<_>>(), ["srv-a", "srv-b"]);

        let mut environments = BTreeSet::new();
        add_batch(
            &mut environments,
            &rows,
            SourceField::Environment,
            &SourceFilters {
                service: Some("app".into()),
                ..Default::default()
            },
            Some("prod"),
            2,
            u64::MAX,
        )
        .unwrap();
        assert!(environments.is_empty());
    }

    #[test]
    fn paginates_more_than_one_thousand_distinct_sources_without_skipping() {
        let rows = (0..1105)
            .map(|index| ("server", format!("service-{index:04}"), Some("prod")))
            .collect::<Vec<_>>();
        let batch = RecordBatch::try_new(
            Arc::new(Schema::new(vec![
                Field::new("server", DataType::Utf8, false),
                Field::new("service", DataType::Utf8, false),
                Field::new("environment", DataType::Utf8, true),
                Field::new("sequence", DataType::UInt64, false),
            ])),
            vec![
                Arc::new(StringArray::from_iter_values(rows.iter().map(|r| r.0))),
                Arc::new(StringArray::from_iter_values(
                    rows.iter().map(|r| r.1.as_str()),
                )),
                Arc::new(StringArray::from_iter(rows.iter().map(|r| r.2))),
                Arc::new(UInt64Array::from_iter_values(1..=rows.len() as u64)),
            ],
        )
        .unwrap();
        let mut first = BTreeSet::new();
        add_batch(
            &mut first,
            &batch,
            SourceField::Service,
            &SourceFilters::default(),
            None,
            1001,
            u64::MAX,
        )
        .unwrap();
        let first = first.into_iter().collect::<Vec<_>>();
        assert_eq!(first.len(), 1001);
        assert_eq!(first[999], "service-0999");

        let mut second = BTreeSet::new();
        add_batch(
            &mut second,
            &batch,
            SourceField::Service,
            &SourceFilters::default(),
            Some(&first[999]),
            1001,
            u64::MAX,
        )
        .unwrap();
        let second = second.into_iter().collect::<Vec<_>>();
        assert_eq!(second.len(), 105);
        assert_eq!(second.first().map(String::as_str), Some("service-1000"));
        assert_eq!(second.last().map(String::as_str), Some("service-1104"));
    }
}
