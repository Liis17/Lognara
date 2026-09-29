//! Проверяемый запрос, стабильная сортировка и подписанные курсоры.
use crate::{
    columns, index,
    model::{LogEvent, StoredEvent, now_nanos, parse_timestamp},
    storage::{Core, Snapshot},
    wire::LogLevel,
};
use anyhow::{Result, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, ops::Bound, time::Instant};
use tantivy::{
    Order, Searcher, Term,
    collector::{TopDocs, sort_key::SortByStaticFastValue},
    query::{BooleanQuery, Occur, PhraseQuery, Query, RangeQuery, TermQuery},
    schema::IndexRecordOption,
};
use uuid::Uuid;

pub type Filters = BTreeMap<String, Vec<String>>;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SearchRequest {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub filters: Filters,
    pub text: Option<TextQuery>,
    #[serde(default = "default_limit")]
    pub limit: usize,
    pub cursor: Option<String>,
}

pub fn default_limit() -> usize {
    100
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TextQuery {
    pub query: String,
    #[serde(default)]
    pub mode: TextMode,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TextMode {
    #[default]
    All,
    Phrase,
}

#[derive(Debug, Serialize)]
pub struct SearchResponse {
    pub events: Vec<LogEvent>,
    pub next_cursor: Option<String>,
    pub as_of: String,
}

#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    #[error("{0}")]
    Invalid(String),
    #[error("cursor expired or its snapshot is no longer available")]
    Gone,
    #[error("query deadline exceeded")]
    Timeout,
    #[error("query capacity is exhausted")]
    Busy,
    #[error("storage query failed")]
    Storage(#[from] anyhow::Error),
}

#[derive(Debug, Clone, Serialize)]
pub struct PreparedFilter {
    pub from: i64,
    pub to: i64,
    pub filters: Filters,
}

impl PreparedFilter {
    pub fn new(from: &str, to: &str, filters: &Filters) -> Result<Self, QueryError> {
        let from = parse_timestamp(from).map_err(|_| {
            QueryError::Invalid("from must be RFC3339 in the i64 nanosecond range".into())
        })?;
        let to = parse_timestamp(to).map_err(|_| {
            QueryError::Invalid("to must be RFC3339 in the i64 nanosecond range".into())
        })?;
        if from >= to {
            return Err(QueryError::Invalid("from must precede to".into()));
        }
        let mut filters = filters.clone();
        for (field, values) in &mut filters {
            if !index::EXACT_FIELDS.contains(&field.as_str()) && field != "level" {
                return Err(QueryError::Invalid(format!("unsupported filter: {field}")));
            }
            if values.is_empty() || values.len() > 64 {
                return Err(QueryError::Invalid(
                    "each filter requires 1..64 values".into(),
                ));
            }
            for value in values.iter_mut() {
                if value.len() > 4096 {
                    return Err(QueryError::Invalid(
                        "filter value exceeds 4096 bytes".into(),
                    ));
                }
                match field.as_str() {
                    "id" => {
                        *value = Uuid::parse_str(value)
                            .map_err(|_| QueryError::Invalid("invalid id".into()))?
                            .to_string()
                    }
                    "trace_id" => normalize_hex(value, 16)?,
                    "span_id" | "parent_span_id" => normalize_hex(value, 8)?,
                    "level" => {
                        level(value).ok_or_else(|| QueryError::Invalid("unknown level".into()))?;
                    }
                    _ => {}
                }
            }
            values.sort();
            values.dedup();
        }
        Ok(Self { from, to, filters })
    }

    pub fn overlaps(&self, min: i64, max: i64) -> bool {
        max >= self.from && min < self.to
    }
}

fn normalize_hex(value: &mut String, length: usize) -> Result<(), QueryError> {
    if value.len() != length * 2 || hex::decode(&value).is_err() {
        return Err(QueryError::Invalid("invalid trace/span hex id".into()));
    }
    value.make_ascii_lowercase();
    Ok(())
}

pub fn level(value: &str) -> Option<LogLevel> {
    [
        LogLevel::Trace,
        LogLevel::Debug,
        LogLevel::Info,
        LogLevel::Warn,
        LogLevel::Error,
        LogLevel::Fatal,
        LogLevel::Unknown,
    ]
    .into_iter()
    .find(|level| level.as_str() == value)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    version: u8,
    instance: Uuid,
    generation: u64,
    watermark: u64,
    query: [u8; 32],
    timestamp: i64,
    sequence: u64,
    issued_at: i64,
    as_of: i64,
}

#[derive(Debug, Clone)]
struct Hit {
    timestamp: i64,
    sequence: u64,
    row_id: usize,
    source: usize,
}

pub fn search(
    core: &Core,
    request: SearchRequest,
    ascending: bool,
    deadline: Instant,
) -> Result<SearchResponse, QueryError> {
    let filter = PreparedFilter::new(&request.from, &request.to, &request.filters)?;
    if !(1..=1000).contains(&request.limit) {
        return Err(QueryError::Invalid("limit must be 1..1000".into()));
    }
    let tokens = match &request.text {
        Some(text) => {
            if text.query.len() > 8192 {
                return Err(QueryError::Invalid("text query exceeds 8192 bytes".into()));
            }
            let tokens = index::tokens(&text.query);
            if tokens.is_empty() || tokens.len() > 64 {
                return Err(QueryError::Invalid("text requires 1..64 tokens".into()));
            }
            tokens
        }
        None => vec![],
    };
    let fingerprint: [u8; 32] = Sha256::digest(
        serde_json::to_vec(&(&filter, &request.text, ascending)).map_err(anyhow::Error::from)?,
    )
    .into();
    let snapshot = core.snapshot();
    let cursor = request
        .cursor
        .as_deref()
        .map(|text| decode_cursor(core, text))
        .transpose()?;
    if let Some(cursor) = &cursor {
        if cursor.version != 1
            || cursor.instance != core.instance
            || cursor.generation != snapshot.generation
            || cursor.issued_at > now_nanos()
            || now_nanos().saturating_sub(cursor.issued_at) > 600_000_000_000
            || cursor.watermark > snapshot.watermark
        {
            return Err(QueryError::Gone);
        }
        if cursor.query != fingerprint {
            return Err(QueryError::Invalid("cursor does not match query".into()));
        }
    }
    let watermark = cursor
        .as_ref()
        .map_or(snapshot.watermark, |cursor| cursor.watermark);
    let query = build_query(
        &filter,
        &tokens,
        request.text.as_ref().map(|text| text.mode),
        watermark,
        cursor.as_ref(),
        ascending,
    );
    let hits = find_hits(
        core,
        &snapshot,
        &filter,
        &query,
        request.limit + 1,
        ascending,
        deadline,
    )?;
    let has_more = hits.len() > request.limit;
    let hits: Vec<_> = hits.into_iter().take(request.limit).collect();
    let events = hydrate(&snapshot, &hits, deadline)?;
    let as_of = cursor
        .as_ref()
        .map_or(snapshot.published_at, |cursor| cursor.as_of);
    let next_cursor = if has_more {
        let last = hits.last().expect("nonempty page");
        Some(encode_cursor(
            core,
            &Cursor {
                version: 1,
                instance: core.instance,
                generation: snapshot.generation,
                watermark,
                query: fingerprint,
                timestamp: last.timestamp,
                sequence: last.sequence,
                issued_at: cursor
                    .as_ref()
                    .map_or_else(now_nanos, |cursor| cursor.issued_at),
                as_of,
            },
        )?)
    } else {
        None
    };
    Ok(SearchResponse {
        events: events.into_iter().map(LogEvent::from).collect(),
        next_cursor,
        as_of: crate::model::format_timestamp(as_of),
    })
}

fn build_query(
    filter: &PreparedFilter,
    tokens: &[String],
    mode: Option<TextMode>,
    watermark: u64,
    cursor: Option<&Cursor>,
    ascending: bool,
) -> BooleanQuery {
    let schema = index::schema();
    let timestamp = schema.get_field("timestamp").unwrap();
    let sequence = schema.get_field("sequence").unwrap();
    let mut clauses: Vec<(Occur, Box<dyn Query>)> = vec![
        (
            Occur::Must,
            Box::new(RangeQuery::new(
                Bound::Included(Term::from_field_i64(timestamp, filter.from)),
                Bound::Excluded(Term::from_field_i64(timestamp, filter.to)),
            )),
        ),
        (
            Occur::Must,
            Box::new(RangeQuery::new(
                Bound::Unbounded,
                Bound::Included(Term::from_field_u64(sequence, watermark)),
            )),
        ),
    ];
    for (name, values) in &filter.filters {
        let field = schema.get_field(name).unwrap();
        let alternatives = values
            .iter()
            .map(|value| {
                let term = if name == "level" {
                    Term::from_field_u64(field, level(value).unwrap().code() as u64)
                } else {
                    Term::from_field_text(field, value)
                };
                (
                    Occur::Should,
                    Box::new(TermQuery::new(term, IndexRecordOption::Basic)) as Box<dyn Query>,
                )
            })
            .collect();
        clauses.push((Occur::Must, Box::new(BooleanQuery::new(alternatives))));
    }
    let field = schema.get_field("message").unwrap();
    let terms: Vec<_> = tokens
        .iter()
        .map(|token| Term::from_field_text(field, token))
        .collect();
    if matches!(mode, Some(TextMode::Phrase)) && terms.len() > 1 {
        clauses.push((Occur::Must, Box::new(PhraseQuery::new(terms))));
    } else {
        for term in terms {
            clauses.push((
                Occur::Must,
                Box::new(TermQuery::new(term, IndexRecordOption::Basic)),
            ));
        }
    }
    if let Some(cursor) = cursor {
        let time_term = Term::from_field_i64(timestamp, cursor.timestamp);
        let seq_term = Term::from_field_u64(sequence, cursor.sequence);
        let time_range = if ascending {
            RangeQuery::new(Bound::Excluded(time_term.clone()), Bound::Unbounded)
        } else {
            RangeQuery::new(Bound::Unbounded, Bound::Excluded(time_term.clone()))
        };
        let seq_range = if ascending {
            RangeQuery::new(Bound::Excluded(seq_term), Bound::Unbounded)
        } else {
            RangeQuery::new(Bound::Unbounded, Bound::Excluded(seq_term))
        };
        clauses.push((
            Occur::Must,
            Box::new(BooleanQuery::new(vec![
                (Occur::Should, Box::new(time_range)),
                (
                    Occur::Should,
                    Box::new(BooleanQuery::new(vec![
                        (
                            Occur::Must,
                            Box::new(TermQuery::new(time_term, IndexRecordOption::Basic)),
                        ),
                        (Occur::Must, Box::new(seq_range)),
                    ])),
                ),
            ])),
        ));
    }
    BooleanQuery::new(clauses)
}

fn find_hits(
    core: &Core,
    snapshot: &Snapshot,
    filter: &PreparedFilter,
    query: &dyn Query,
    limit: usize,
    ascending: bool,
    deadline: Instant,
) -> Result<Vec<Hit>, QueryError> {
    let mut sources: Vec<_> = snapshot
        .closed
        .iter()
        .enumerate()
        .filter(|(_, segment)| {
            filter.overlaps(segment.meta.min_timestamp, segment.meta.max_timestamp)
        })
        .map(|(i, segment)| (i, &segment.meta))
        .collect();
    if let Some(active) = &snapshot.active
        && filter.overlaps(active.meta.min_timestamp, active.meta.max_timestamp)
    {
        sources.push((snapshot.closed.len(), &active.meta));
    }
    sources.sort_by_key(|(_, meta)| {
        if ascending {
            meta.min_timestamp as i128
        } else {
            -(meta.max_timestamp as i128)
        }
    });
    let mut best: Vec<Hit> = Vec::new();
    for (source, meta) in sources {
        check_deadline(deadline)?;
        if best.len() == limit {
            let worst = best.last().unwrap();
            let bound = if ascending {
                (meta.min_timestamp, meta.first_sequence)
            } else {
                (meta.max_timestamp, meta.last_sequence)
            };
            if (ascending && bound > (worst.timestamp, worst.sequence))
                || (!ascending && bound < (worst.timestamp, worst.sequence))
            {
                continue;
            }
        }
        let searcher = if source == snapshot.closed.len() {
            snapshot.active.as_ref().unwrap().searcher.clone()
        } else {
            core.indexes
                .searcher(&snapshot.closed[source].path, meta.rows)?
        };
        let result = segment_hits(&searcher, query, limit, ascending, source);
        if result.is_err() && source < snapshot.closed.len() {
            core.indexes.invalidate(&snapshot.closed[source].path);
        }
        best.extend(result?);
        best.sort_by(|a, b| {
            let order = (a.timestamp, a.sequence).cmp(&(b.timestamp, b.sequence));
            if ascending { order } else { order.reverse() }
        });
        best.truncate(limit);
    }
    Ok(best)
}

fn segment_hits(
    searcher: &Searcher,
    query: &dyn Query,
    limit: usize,
    ascending: bool,
    source: usize,
) -> Result<Vec<Hit>> {
    let order = if ascending { Order::Asc } else { Order::Desc };
    let collector = TopDocs::with_limit(limit).order_by((
        (SortByStaticFastValue::<i64>::for_field("timestamp"), order),
        (SortByStaticFastValue::<u64>::for_field("sequence"), order),
    ));
    searcher
        .search(query, &collector)?
        .into_iter()
        .map(|((timestamp, sequence), address)| {
            let row = searcher
                .segment_reader(address.segment_ord)
                .fast_fields()
                .u64("row_id")?
                .first(address.doc_id);
            Ok(Hit {
                timestamp: timestamp.context("missing timestamp fast field")?,
                sequence: sequence.context("missing sequence fast field")?,
                row_id: row.context("missing row_id fast field")? as usize,
                source,
            })
        })
        .collect()
}

fn hydrate(
    snapshot: &Snapshot,
    hits: &[Hit],
    deadline: Instant,
) -> Result<Vec<StoredEvent>, QueryError> {
    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for hit in hits {
        groups.entry(hit.source).or_default().push(hit.row_id);
    }
    let mut fetched = BTreeMap::new();
    for (source, mut ids) in groups {
        check_deadline(deadline)?;
        ids.sort_unstable();
        ids.dedup();
        let rows = if source < snapshot.closed.len() {
            columns::read_rows(&snapshot.closed[source].path.join("logs.parquet"), &ids)?
        } else {
            let mut offset = 0;
            let mut rows = vec![];
            for batch in &snapshot.active.as_ref().unwrap().batches {
                for &id in ids
                    .iter()
                    .filter(|&&id| id >= offset && id < offset + batch.num_rows())
                {
                    rows.extend(columns::decode(&batch.slice(id - offset, 1))?);
                }
                offset += batch.num_rows();
            }
            rows
        };
        for (id, row) in ids.into_iter().zip(rows) {
            fetched.insert((source, id), row);
        }
    }
    hits.iter()
        .map(|hit| {
            let row = fetched
                .remove(&(hit.source, hit.row_id))
                .context("index points outside segment")?;
            ensure!(
                row.sequence == hit.sequence && row.event.timestamp == hit.timestamp,
                "index/Parquet row mismatch"
            );
            Ok(row)
        })
        .collect::<Result<Vec<_>>>()
        .map_err(QueryError::Storage)
}

pub fn check_deadline(deadline: Instant) -> Result<(), QueryError> {
    if Instant::now() >= deadline {
        Err(QueryError::Timeout)
    } else {
        Ok(())
    }
}

fn encode_cursor(core: &Core, cursor: &Cursor) -> Result<String> {
    let mut bytes = serde_json::to_vec(cursor)?;
    let mut mac = Hmac::<Sha256>::new_from_slice(&core.cursor_key)?;
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

use anyhow::Context;
