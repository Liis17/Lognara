//! Поисковый индекс содержит поля для поиска и стабильный row_id, без STORED-дубликата.
use crate::{columns, model::StoredEvent, wal::sync_dir};
use anyhow::{Result, ensure};
use lru::LruCache;
use std::{
    collections::HashSet,
    fs,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};
use tantivy::{
    Index, IndexReader, IndexWriter, ReloadPolicy, Searcher, TantivyDocument,
    indexer::IndexWriterOptions,
    schema::{FAST, INDEXED, IndexRecordOption, STRING, Schema, TextFieldIndexing, TextOptions},
    tokenizer::{LowerCaser, SimpleTokenizer, TextAnalyzer},
};

pub const EXACT_FIELDS: &[&str] = &[
    "id",
    "environment",
    "server",
    "backend",
    "service",
    "service_instance",
    "action",
    "trace_id",
    "span_id",
    "parent_span_id",
    "request_id",
    "pattern_id",
    "fingerprint",
];

pub fn schema() -> Schema {
    static SCHEMA: OnceLock<Schema> = OnceLock::new();
    SCHEMA
        .get_or_init(|| {
            let mut builder = Schema::builder();
            builder.add_i64_field("timestamp", INDEXED | FAST);
            builder.add_u64_field("sequence", INDEXED | FAST);
            builder.add_u64_field("row_id", FAST);
            builder.add_u64_field("level", INDEXED | FAST);
            for field in EXACT_FIELDS {
                builder.add_text_field(field, STRING);
            }
            builder.add_text_field(
                "message",
                TextOptions::default().set_indexing_options(
                    TextFieldIndexing::default()
                        .set_tokenizer("log_text")
                        .set_index_option(IndexRecordOption::WithFreqsAndPositions),
                ),
            );
            builder.build()
        })
        .clone()
}

fn configure(index: &Index) {
    index.tokenizers().register("log_text", text_analyzer());
}

fn text_analyzer() -> TextAnalyzer {
    TextAnalyzer::builder(SimpleTokenizer::default())
        .filter(LowerCaser)
        .build()
}

pub fn tokens(text: &str) -> Vec<String> {
    let mut analyzer = text_analyzer();
    let mut stream = analyzer.token_stream(text);
    let mut tokens = vec![];
    while stream.advance() {
        tokens.push(stream.token().text.clone());
    }
    tokens
}

pub struct ActiveIndex {
    writer: IndexWriter,
    reader: IndexReader,
}

impl ActiveIndex {
    pub fn create(path: &Path, memory: usize) -> Result<Self> {
        fs::create_dir_all(path)?;
        let index = Index::create_in_dir(path, schema())?;
        configure(&index);
        let writer = index.writer_with_options(
            IndexWriterOptions::builder()
                .num_worker_threads(1)
                .memory_budget_per_thread(memory)
                .build(),
        )?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        Ok(Self { writer, reader })
    }

    pub fn add(&mut self, row: &StoredEvent, row_id: u64) -> Result<()> {
        let schema = schema();
        let field = |name| schema.get_field(name).expect("fixed schema");
        let mut doc = TantivyDocument::default();
        doc.add_i64(field("timestamp"), row.event.timestamp);
        doc.add_u64(field("sequence"), row.sequence);
        doc.add_u64(field("row_id"), row_id);
        doc.add_u64(field("level"), row.event.level.code() as u64);
        doc.add_text(field("id"), row.event.id.0.to_string());
        for (name, value) in [
            ("environment", row.source.environment.as_deref()),
            ("server", Some(row.source.server.as_str())),
            ("backend", Some(row.source.backend.as_str())),
            ("service", Some(row.source.service.as_str())),
            ("service_instance", row.source.service_instance.as_deref()),
            ("action", row.event.action.as_deref()),
            ("request_id", row.event.request_id.as_deref()),
        ] {
            if let Some(value) = value {
                doc.add_text(field(name), value);
            }
        }
        if let Some(id) = row.event.trace_id {
            doc.add_text(field("trace_id"), hex::encode(id.0));
        }
        if let Some(id) = row.event.span_id {
            doc.add_text(field("span_id"), hex::encode(id.0));
        }
        if let Some(id) = row.event.parent_span_id {
            doc.add_text(field("parent_span_id"), hex::encode(id.0));
        }
        doc.add_text(field("message"), &row.event.message);
        let identity = crate::pattern_identity::identify(
            row.source.environment.as_deref(),
            &row.source.service,
            row.event.level.as_str(),
            &row.event.message,
        );
        let exception = crate::issue_identity::Exception::from_map(&row.event.attributes);
        if let Some(issue) = crate::issue_identity::fingerprint(
            row.source.environment.as_deref(),
            &row.source.service,
            row.event.action.as_deref(),
            row.event.level,
            &identity,
            &exception,
        ) {
            doc.add_text(field("fingerprint"), issue.id);
        }
        doc.add_text(field("pattern_id"), identity.id);
        self.writer.add_document(doc)?;
        Ok(())
    }

    pub fn publish(&mut self) -> Result<Searcher> {
        self.commit()?;
        self.reader.reload()?;
        Ok(self.reader.searcher())
    }

    pub fn finish(mut self) -> Result<()> {
        self.commit()?;
        self.writer.wait_merging_threads()?;
        Ok(())
    }

    fn commit(&mut self) -> Result<()> {
        let mut commit = self.writer.prepare_commit()?;
        commit.set_payload(crate::pattern_identity::INDEX_PAYLOAD);
        commit.commit()?;
        Ok(())
    }
}

pub struct IndexCache {
    readers: Mutex<LruCache<PathBuf, IndexReader>>,
    /// Закрытые индексы неизменны: полная проверка checksum нужна один раз за процесс.
    verified: Mutex<HashSet<PathBuf>>,
    memory: usize,
}

impl IndexCache {
    pub fn new(entries: usize, memory: usize) -> Self {
        Self {
            readers: Mutex::new(LruCache::new(
                NonZeroUsize::new(entries).expect("nonzero config"),
            )),
            verified: Mutex::new(HashSet::new()),
            memory,
        }
    }

    pub fn searcher(&self, segment: &Path, rows: usize) -> Result<Searcher> {
        let mut readers = self.readers.lock().unwrap();
        if let Some(reader) = readers.get(segment) {
            return Ok(reader.searcher());
        }
        let verified = self.verified.lock().unwrap().contains(segment);
        let reader = match open(segment, rows, !verified) {
            Ok(reader) => reader,
            Err(error) => {
                tracing::warn!(%error, path = %segment.display(), "rebuilding index from Parquet");
                rebuild(segment, self.memory)?;
                open(segment, rows, true)?
            }
        };
        self.verified.lock().unwrap().insert(segment.into());
        let searcher = reader.searcher();
        readers.put(segment.into(), reader);
        Ok(searcher)
    }

    pub fn invalidate(&self, segment: &Path) {
        self.readers.lock().unwrap().pop(segment);
        self.verified.lock().unwrap().remove(segment);
    }
}

fn open(segment: &Path, rows: usize, checksum: bool) -> Result<IndexReader> {
    let index = Index::open_in_dir(segment.join("index"))?;
    ensure!(index.schema() == schema(), "incompatible index schema");
    ensure!(
        index.load_metas()?.payload.as_deref() == Some(crate::pattern_identity::INDEX_PAYLOAD),
        "incompatible pattern normalization"
    );
    ensure!(
        !checksum || index.validate_checksum()?.is_empty(),
        "index checksum mismatch"
    );
    configure(&index);
    let reader = index.reader()?;
    ensure!(
        reader.searcher().num_docs() == rows as u64,
        "index row count mismatch"
    );
    Ok(reader)
}

fn rebuild(segment: &Path, memory: usize) -> Result<()> {
    let temp = segment.join("index-rebuild");
    if temp.exists() {
        fs::remove_dir_all(&temp)?;
    }
    build(&segment.join("logs.parquet"), &temp, memory, &|| false)?;
    let path = segment.join("index");
    let old = segment.join("index-old");
    if old.exists() {
        fs::remove_dir_all(&old)?;
    }
    if path.exists() {
        fs::rename(&path, &old)?;
    }
    fs::rename(temp, path)?;
    sync_dir(segment)?;
    if old.exists() {
        fs::remove_dir_all(old)?;
    }
    Ok(())
}

/// Строит индекс Parquet-файла в `dir`; row_id - позиция строки в файле.
/// Читает по одной пачке; false - `stop` прервал сборку, индекс неполон.
pub(crate) fn build(
    parquet: &Path,
    dir: &Path,
    memory: usize,
    stop: &dyn Fn() -> bool,
) -> Result<bool> {
    let mut writer = ActiveIndex::create(dir, memory)?;
    let mut row_id = 0;
    for batch in columns::stream_batches(parquet)? {
        if stop() {
            return Ok(false);
        }
        for row in columns::decode(&batch?)? {
            writer.add(&row, row_id)?;
            row_id += 1;
        }
    }
    writer.finish()?;
    Ok(true)
}
