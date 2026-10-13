//! Точки метрик из отчётов приложений. Сырые значения хранятся без retention; интерпретация позже.
use std::{
    path::PathBuf,
    sync::{Arc, OnceLock},
};

use anyhow::Result;
use datafusion::arrow::{
    array::{
        ArrayRef, FixedSizeBinaryBuilder, Float64Array, RecordBatch, StringArray,
        TimestampNanosecondArray,
    },
    datatypes::{DataType, Field, Schema, SchemaRef, TimeUnit},
};
use serde::{Deserialize, Serialize};

use crate::wire::Source;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Counter,
    Gauge,
    Rule,
}

impl Origin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Counter => "counter",
            Self::Gauge => "gauge",
            Self::Rule => "rule",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Point {
    pub timestamp: i64,
    pub source: Source,
    pub name: String,
    pub origin: Origin,
    pub rule_id: Option<String>,
    pub labels: String,
    pub value: f64,
    pub event_id: Option<[u8; 16]>,
}

pub fn schema() -> SchemaRef {
    static SCHEMA: OnceLock<SchemaRef> = OnceLock::new();
    SCHEMA
        .get_or_init(|| {
            Arc::new(Schema::new(vec![
                Field::new(
                    "timestamp",
                    DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into())),
                    false,
                ),
                Field::new("service", DataType::Utf8, false),
                Field::new("environment", DataType::Utf8, true),
                Field::new("server", DataType::Utf8, false),
                Field::new("backend", DataType::Utf8, false),
                Field::new("service_instance", DataType::Utf8, true),
                Field::new("name", DataType::Utf8, false),
                Field::new("origin", DataType::Utf8, false),
                Field::new("rule_id", DataType::Utf8, true),
                Field::new("labels", DataType::Utf8, false),
                Field::new("value", DataType::Float64, false),
                Field::new("event_id", DataType::FixedSizeBinary(16), true),
            ]))
        })
        .clone()
}

pub fn encode(points: &[Point]) -> Result<RecordBatch> {
    let mut event_ids = FixedSizeBinaryBuilder::new(16);
    for point in points {
        match point.event_id {
            Some(id) => event_ids.append_value(id)?,
            None => event_ids.append_null(),
        }
    }
    let arrays: Vec<ArrayRef> = vec![
        Arc::new(
            TimestampNanosecondArray::from_iter_values(points.iter().map(|p| p.timestamp))
                .with_timezone("UTC"),
        ),
        Arc::new(StringArray::from_iter_values(
            points.iter().map(|p| p.source.service.as_str()),
        )),
        Arc::new(StringArray::from_iter(
            points.iter().map(|p| p.source.environment.as_deref()),
        )),
        Arc::new(StringArray::from_iter_values(
            points.iter().map(|p| p.source.server.as_str()),
        )),
        Arc::new(StringArray::from_iter_values(
            points.iter().map(|p| p.source.backend.as_str()),
        )),
        Arc::new(StringArray::from_iter(
            points.iter().map(|p| p.source.service_instance.as_deref()),
        )),
        Arc::new(StringArray::from_iter_values(
            points.iter().map(|p| p.name.as_str()),
        )),
        Arc::new(StringArray::from_iter_values(
            points.iter().map(|p| p.origin.as_str()),
        )),
        Arc::new(StringArray::from_iter(
            points.iter().map(|p| p.rule_id.as_deref()),
        )),
        Arc::new(StringArray::from_iter_values(
            points.iter().map(|p| p.labels.as_str()),
        )),
        Arc::new(Float64Array::from_iter_values(
            points.iter().map(|p| p.value),
        )),
        Arc::new(event_ids.finish()),
    ];
    Ok(RecordBatch::try_new(schema(), arrays)?)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeriesFileMeta {
    pub version: u32,
    pub id: String,
    /// Путь относительно data_dir.
    pub path: String,
    pub rows: usize,
    pub min_timestamp: i64,
    pub max_timestamp: i64,
    pub closed_at: i64,
}

pub struct SeriesFile {
    pub meta: SeriesFileMeta,
    pub path: PathBuf,
}
