//! Lognara Core: надёжный приём пачек relay, хранение и запросы.

pub mod access;
pub mod analytics;
pub mod api;
pub mod catalog;
pub mod columns;
pub mod config;
pub mod index;
pub mod journal;
pub mod metrics;
pub mod model;
pub mod pattern_catalog;
pub mod pattern_identity;
pub mod patterns;
pub mod query;
pub mod storage;
pub mod summary;
pub mod wal;
pub mod wire;
mod wire_budget;
