//! lognara-relay: принимает пачки логов от lognara-agent, разбирает записи в события,
//! группирует их по источнику и сжатыми пачками отправляет в lognara-core.

pub mod agent_wire;
pub mod config;
pub mod core_wire;
pub mod normalize;
