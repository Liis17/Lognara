//! Lognara Core: надёжный приём пачек relay, хранение и запросы.

pub mod analytics;
pub mod api;
pub mod catalog;
pub mod columns;
pub mod config;
pub mod definitions;
pub mod extract;
pub mod index;
pub mod journal;
pub mod metrics;
pub mod model;
pub mod query;
pub mod series;
pub mod series_query;
pub mod storage;
pub mod wal;
pub mod wire;
mod wire_budget;
