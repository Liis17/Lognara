//! Единственная схема Arrow/Parquet и получение полных событий по row_id.
use std::{
    fs::{self, File},
    path::Path,
    sync::{Arc, OnceLock},
};

use anyhow::{Result, anyhow, ensure};
use datafusion::arrow::{
    array::{
        Array, ArrayRef, FixedSizeBinaryArray, FixedSizeBinaryBuilder, RecordBatch, StringArray,
        TimestampNanosecondArray, UInt8Array, UInt64Array,
    },
    datatypes::{DataType, Field, Schema, SchemaRef, TimeUnit},
};
use parquet::{
    arrow::{
        ArrowWriter,
        arrow_reader::{ParquetRecordBatchReaderBuilder, RowSelection},
    },
    basic::{Compression, ZstdLevel},
    file::properties::WriterProperties,
};
use uuid::Uuid;

use crate::{
    model::StoredEvent,
    wal::sync_dir,
    wire::{Event, LogId, LogLevel, Source, SpanId, TraceId},
};

pub fn schema() -> SchemaRef {
    static SCHEMA: OnceLock<SchemaRef> = OnceLock::new();
    SCHEMA
        .get_or_init(|| {
            Arc::new(Schema::new(vec![
                Field::new("id", DataType::FixedSizeBinary(16), false),
                Field::new(
                    "timestamp",
                    DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into())),
                    false,
                ),
                Field::new(
                    "ingested_at",
                    DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into())),
                    false,
                ),
                Field::new("environment", DataType::Utf8, true),
                Field::new("server", DataType::Utf8, false),
                Field::new("backend", DataType::Utf8, false),
                Field::new("service", DataType::Utf8, false),
                Field::new("service_instance", DataType::Utf8, true),
                Field::new("action", DataType::Utf8, true),
                Field::new("level", DataType::UInt8, false),
                Field::new("message", DataType::Utf8, false),
                Field::new("trace_id", DataType::FixedSizeBinary(16), true),
                Field::new("span_id", DataType::FixedSizeBinary(8), true),
                Field::new("parent_span_id", DataType::FixedSizeBinary(8), true),
                Field::new("request_id", DataType::Utf8, true),
                Field::new("attributes", DataType::Utf8, false),
                Field::new("sequence", DataType::UInt64, false),
                Field::new(
                    "core_received_at",
                    DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into())),
                    false,
                ),
            ]))
        })
        .clone()
}

pub fn encode(rows: &[StoredEvent]) -> Result<RecordBatch> {
    let mut ids = FixedSizeBinaryBuilder::new(16);
    let mut traces = FixedSizeBinaryBuilder::new(16);
    let mut spans = FixedSizeBinaryBuilder::new(8);
    let mut parents = FixedSizeBinaryBuilder::new(8);
    let mut attrs = Vec::with_capacity(rows.len());
    for row in rows {
        ids.append_value(row.event.id.0.as_bytes())?;
        match row.event.trace_id {
            Some(id) => traces.append_value(id.0)?,
            None => traces.append_null(),
        }
        match row.event.span_id {
            Some(id) => spans.append_value(id.0)?,
            None => spans.append_null(),
        }
        match row.event.parent_span_id {
            Some(id) => parents.append_value(id.0)?,
            None => parents.append_null(),
        }
        attrs.push(serde_json::to_string(&row.event.attributes)?);
    }
    let arrays: Vec<ArrayRef> = vec![
        Arc::new(ids.finish()),
        Arc::new(
            TimestampNanosecondArray::from_iter_values(rows.iter().map(|r| r.event.timestamp))
                .with_timezone("UTC"),
        ),
        Arc::new(
            TimestampNanosecondArray::from_iter_values(rows.iter().map(|r| r.event.ingested_at))
                .with_timezone("UTC"),
        ),
        Arc::new(StringArray::from_iter(
            rows.iter().map(|r| r.source.environment.as_deref()),
        )),
        Arc::new(StringArray::from_iter_values(
            rows.iter().map(|r| r.source.server.as_str()),
        )),
        Arc::new(StringArray::from_iter_values(
            rows.iter().map(|r| r.source.backend.as_str()),
        )),
        Arc::new(StringArray::from_iter_values(
            rows.iter().map(|r| r.source.service.as_str()),
        )),
        Arc::new(StringArray::from_iter(
            rows.iter().map(|r| r.source.service_instance.as_deref()),
        )),
        Arc::new(StringArray::from_iter(
            rows.iter().map(|r| r.event.action.as_deref()),
        )),
        Arc::new(UInt8Array::from_iter_values(
            rows.iter().map(|r| r.event.level.code()),
        )),
        Arc::new(StringArray::from_iter_values(
            rows.iter().map(|r| r.event.message.as_str()),
        )),
        Arc::new(traces.finish()),
        Arc::new(spans.finish()),
        Arc::new(parents.finish()),
        Arc::new(StringArray::from_iter(
            rows.iter().map(|r| r.event.request_id.as_deref()),
        )),
        Arc::new(StringArray::from(attrs)),
        Arc::new(UInt64Array::from_iter_values(
            rows.iter().map(|r| r.sequence),
        )),
        Arc::new(
            TimestampNanosecondArray::from_iter_values(rows.iter().map(|r| r.core_received_at))
                .with_timezone("UTC"),
        ),
    ];
    Ok(RecordBatch::try_new(schema(), arrays)?)
}

fn column<'a, T: 'static>(batch: &'a RecordBatch, name: &str) -> Result<&'a T> {
    batch
        .column_by_name(name)
        .and_then(|column| column.as_any().downcast_ref())
        .ok_or_else(|| anyhow!("invalid column {name}"))
}

pub fn decode(batch: &RecordBatch) -> Result<Vec<StoredEvent>> {
    ensure!(batch.schema() == schema(), "unsupported log schema");
    for (field, array) in batch.schema().fields().iter().zip(batch.columns()) {
        ensure!(
            field.is_nullable() || array.null_count() == 0,
            "null in required column {}",
            field.name()
        );
    }
    let id = column::<FixedSizeBinaryArray>(batch, "id")?;
    let timestamp = column::<TimestampNanosecondArray>(batch, "timestamp")?;
    let ingested = column::<TimestampNanosecondArray>(batch, "ingested_at")?;
    let environment = column::<StringArray>(batch, "environment")?;
    let server = column::<StringArray>(batch, "server")?;
    let backend = column::<StringArray>(batch, "backend")?;
    let service = column::<StringArray>(batch, "service")?;
    let instance = column::<StringArray>(batch, "service_instance")?;
    let action = column::<StringArray>(batch, "action")?;
    let level = column::<UInt8Array>(batch, "level")?;
    let message = column::<StringArray>(batch, "message")?;
    let trace = column::<FixedSizeBinaryArray>(batch, "trace_id")?;
    let span = column::<FixedSizeBinaryArray>(batch, "span_id")?;
    let parent = column::<FixedSizeBinaryArray>(batch, "parent_span_id")?;
    let request = column::<StringArray>(batch, "request_id")?;
    let attrs = column::<StringArray>(batch, "attributes")?;
    let sequence = column::<UInt64Array>(batch, "sequence")?;
    let received = column::<TimestampNanosecondArray>(batch, "core_received_at")?;
    let optional =
        |array: &StringArray, row| (!array.is_null(row)).then(|| array.value(row).to_owned());
    (0..batch.num_rows())
        .map(|i| {
            Ok(StoredEvent {
                sequence: sequence.value(i),
                core_received_at: received.value(i),
                source: Source {
                    environment: optional(environment, i),
                    server: server.value(i).into(),
                    backend: backend.value(i).into(),
                    service: service.value(i).into(),
                    service_instance: optional(instance, i),
                },
                event: Event {
                    id: LogId(Uuid::from_slice(id.value(i))?),
                    timestamp: timestamp.value(i),
                    ingested_at: ingested.value(i),
                    action: optional(action, i),
                    level: LogLevel::from_code(level.value(i))?,
                    message: message.value(i).into(),
                    trace_id: if trace.is_null(i) {
                        None
                    } else {
                        Some(TraceId(trace.value(i).try_into()?))
                    },
                    span_id: if span.is_null(i) {
                        None
                    } else {
                        Some(SpanId(span.value(i).try_into()?))
                    },
                    parent_span_id: if parent.is_null(i) {
                        None
                    } else {
                        Some(SpanId(parent.value(i).try_into()?))
                    },
                    request_id: optional(request, i),
                    attributes: serde_json::from_str(attrs.value(i))?,
                },
            })
        })
        .collect()
}

pub fn write_parquet(path: &Path, batches: &[RecordBatch]) -> Result<()> {
    let temp = path.with_extension("parquet.tmp");
    let file = File::create(&temp)?;
    let props = WriterProperties::builder()
        .set_compression(Compression::ZSTD(ZstdLevel::try_new(3)?))
        .set_dictionary_enabled(true)
        .set_max_row_group_row_count(Some(16_384))
        .build();
    let mut writer = ArrowWriter::try_new(file.try_clone()?, schema(), Some(props))?;
    for batch in batches {
        writer.write(batch)?;
        crate::wal::failpoint("segment_during_parquet");
    }
    writer.close()?;
    file.sync_all()?;
    fs::rename(&temp, path)?;
    sync_dir(
        path.parent()
            .ok_or_else(|| anyhow!("missing parent directory"))?,
    )?;
    Ok(())
}

pub fn read_all(path: &Path) -> Result<Vec<RecordBatch>> {
    Ok(ParquetRecordBatchReaderBuilder::try_new(File::open(path)?)?
        .build()?
        .collect::<Result<Vec<_>, _>>()?)
}

/// row_ids строго возрастают; RowSelection пропускает ненужные row groups и страницы.
pub fn read_rows(path: &Path, row_ids: &[usize]) -> Result<Vec<StoredEvent>> {
    let mut unlimited = usize::MAX;
    read_rows_with_budget(path, row_ids, &mut unlimited)
}

/// Бюджет включает модель и JSON-ответ. Проверка до разбора attributes в Value.
pub fn charge_row(batch: &RecordBatch, row: usize, remaining: &mut usize) -> Result<()> {
    let mut bytes = 2048usize;
    for (field, array) in batch.schema().fields().iter().zip(batch.columns()) {
        if let Some(array) = array.as_any().downcast_ref::<StringArray>()
            && !array.is_null(row)
        {
            // JSON-дерево из коротких scalar/keys дороже текста. 64x покрывает
            // Value/container capacity; 8x для строк — escaping и буфер ответа.
            let factor = if field.name() == "attributes" { 64 } else { 8 };
            bytes = bytes.saturating_add(array.value(row).len().saturating_mul(factor));
        }
    }
    *remaining = remaining
        .checked_sub(bytes)
        .ok_or_else(|| anyhow!("search result exceeds memory budget; reduce page size"))?;
    Ok(())
}

pub fn read_rows_with_budget(
    path: &Path,
    row_ids: &[usize],
    remaining: &mut usize,
) -> Result<Vec<StoredEvent>> {
    if row_ids.is_empty() {
        return Ok(vec![]);
    }
    ensure!(
        row_ids.windows(2).all(|pair| pair[0] < pair[1]),
        "row ids must be unique and ordered"
    );
    let builder = ParquetRecordBatchReaderBuilder::try_new(File::open(path)?)?;
    let count = usize::try_from(builder.metadata().file_metadata().num_rows())?;
    ensure!(
        *row_ids.last().unwrap() < count,
        "row id outside Parquet file"
    );
    let selection =
        RowSelection::from_consecutive_ranges(row_ids.iter().map(|&row| row..row + 1), count);
    let mut rows = Vec::with_capacity(row_ids.len());
    for batch in builder
        .with_row_selection(selection)
        .with_batch_size(1)
        .build()?
    {
        let batch = batch?;
        charge_row(&batch, 0, remaining)?;
        rows.extend(decode(&batch)?);
    }
    ensure!(rows.len() == row_ids.len(), "Parquet row count mismatch");
    Ok(rows)
}
