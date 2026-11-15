//! Алерты на логи: новые совпадения правила с прошлой оценки.
//!
//! Новизна определяется sequence (порядок материализации), а не временем события:
//! событие с поздней доставкой или старым временем всё равно учитывается один раз.
//! Сегменты, целиком материализованные до `after_sequence`, не открываются.

use std::{ops::Bound, sync::Arc, time::Instant};

use serde::{Deserialize, Serialize};
use tantivy::{
    Term,
    collector::Count,
    query::{BooleanQuery, Occur, Query, RangeQuery},
};

use crate::{
    index,
    model::LogEvent,
    query::{self, Filters, PreparedFilter, PreparedText, QueryError, TextQuery, check_deadline},
    storage::Core,
};

pub const MAX_RULES: usize = 100;
pub const MAX_SAMPLES: usize = 3;
/// Все время событий: правило отбирает по sequence.
const FROM: &str = "1970-01-01T00:00:00Z";
const TO: &str = "2262-04-11T00:00:00Z";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlertQueryRequest {
    pub rules: Vec<AlertRule>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlertRule {
    pub id: String,
    #[serde(default)]
    pub filters: Filters,
    pub text: Option<TextQuery>,
    /// Последний учтённый watermark; без него правило только получает базовую линию.
    pub after_sequence: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct AlertQueryResponse {
    pub as_of: String,
    /// Следующий `after_sequence` для всех правил запроса.
    pub watermark: u64,
    pub results: Vec<AlertResult>,
}

#[derive(Debug, Serialize)]
pub struct AlertResult {
    pub id: String,
    pub count: u64,
    /// До трёх последних совпадений, новые первыми.
    pub samples: Vec<LogEvent>,
}

struct Prepared {
    id: String,
    filter: PreparedFilter,
    text: Option<PreparedText>,
    after: Option<u64>,
}

fn prepare(request: AlertQueryRequest) -> Result<Vec<Prepared>, QueryError> {
    if request.rules.is_empty() || request.rules.len() > MAX_RULES {
        return Err(QueryError::Invalid(format!(
            "rules requires 1..{MAX_RULES} items"
        )));
    }
    request
        .rules
        .into_iter()
        .map(|rule| {
            if rule.id.is_empty() || rule.id.len() > 64 {
                return Err(QueryError::Invalid("rule id requires 1..64 bytes".into()));
            }
            Ok(Prepared {
                id: rule.id,
                filter: PreparedFilter::new(FROM, TO, &rule.filters)?,
                text: rule.text.as_ref().map(PreparedText::new).transpose()?,
                after: rule.after_sequence,
            })
        })
        .collect()
}

pub async fn alert_query(
    core: Arc<Core>,
    request: AlertQueryRequest,
) -> Result<AlertQueryResponse, QueryError> {
    let rules = prepare(request)?;
    let deadline = Instant::now() + core.journal.config.query_timeout;
    tokio::task::spawn_blocking(move || evaluate(&core, rules, deadline))
        .await
        .map_err(|error| QueryError::Storage(error.into()))?
}

fn evaluate(
    core: &Core,
    rules: Vec<Prepared>,
    deadline: Instant,
) -> Result<AlertQueryResponse, QueryError> {
    let snapshot = core.snapshot();
    let watermark = snapshot.watermark;
    let sequence = index::schema().get_field("sequence").unwrap();
    let mut results = Vec::with_capacity(rules.len());
    for rule in rules {
        let Some(after) = rule.after.filter(|after| *after < watermark) else {
            results.push(AlertResult {
                id: rule.id,
                count: 0,
                samples: vec![],
            });
            continue;
        };
        let tokens = rule
            .text
            .as_ref()
            .map_or(&[][..], |text| text.tokens.as_slice());
        let query = BooleanQuery::new(vec![
            (
                Occur::Must,
                Box::new(query::build_query(
                    &rule.filter,
                    tokens,
                    rule.text.as_ref().map(|text| text.mode),
                    watermark,
                    None,
                    false,
                )) as Box<dyn Query>,
            ),
            (
                Occur::Must,
                Box::new(RangeQuery::new(
                    Bound::Excluded(Term::from_field_u64(sequence, after)),
                    Bound::Unbounded,
                )),
            ),
        ]);
        let mut sources: Vec<usize> = snapshot
            .closed
            .iter()
            .enumerate()
            .filter(|(_, segment)| segment.meta.last_sequence > after)
            .map(|(index, _)| index)
            .collect();
        if snapshot
            .active
            .as_ref()
            .is_some_and(|active| active.meta.rows > 0 && active.meta.last_sequence > after)
        {
            sources.push(snapshot.closed.len());
        }
        let mut count = 0u64;
        let mut hits = Vec::new();
        for source in sources {
            check_deadline(deadline)?;
            let searcher = if source == snapshot.closed.len() {
                snapshot.active.as_ref().unwrap().searcher.clone()
            } else {
                let segment = &snapshot.closed[source];
                core.indexes.searcher(&segment.path, segment.meta.rows)?
            };
            let result = (|| -> anyhow::Result<_> {
                let matched = searcher.search(&query, &Count)? as u64;
                let found = if matched > 0 {
                    query::segment_hits(&searcher, &query, MAX_SAMPLES, false, source)?
                } else {
                    vec![]
                };
                Ok((matched, found))
            })();
            if result.is_err() && source < snapshot.closed.len() {
                core.indexes.invalidate(&snapshot.closed[source].path);
            }
            let (matched, found) = result?;
            count += matched;
            hits.extend(found);
        }
        hits.sort_by(|a, b| (b.timestamp, b.sequence).cmp(&(a.timestamp, a.sequence)));
        hits.truncate(MAX_SAMPLES);
        let samples = query::hydrate(
            &snapshot,
            &hits,
            deadline,
            core.journal.config.search_memory_bytes,
        )?;
        results.push(AlertResult {
            id: rule.id,
            count,
            samples: samples.into_iter().map(LogEvent::from).collect(),
        });
    }
    Ok(AlertQueryResponse {
        as_of: crate::model::format_timestamp(snapshot.published_at),
        watermark,
        results,
    })
}
