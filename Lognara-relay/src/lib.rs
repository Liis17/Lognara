//! lognara-relay: принимает пачки логов от lognara-agent, разбирает записи в события,
//! группирует их по источнику и сжатыми пачками отправляет в lognara-core.

pub mod config;
