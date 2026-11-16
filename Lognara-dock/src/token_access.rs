//! Права и скоуп MCP-токенов: PAT и OAuth-гранты.

use serde::{Deserialize, Serialize};

pub const READ_SCOPE: &str = "lognara.read";
pub const WRITE_SCOPE: &str = "lognara.write";

/// Что токен может делать. Запись алертов требует чтения соответствующих логов или метрик.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rights {
    pub logs: bool,
    pub metrics: bool,
    pub alerts_write: bool,
}

impl Default for Rights {
    /// Значения по умолчанию совпадают с прежними токенами: логи и метрики, без записи.
    fn default() -> Self {
        Self {
            logs: true,
            metrics: true,
            alerts_write: false,
        }
    }
}

impl Rights {
    /// Нужно хотя бы чтение логов или метрик; право записи без чтения данных запрещено.
    pub fn is_valid(self) -> bool {
        self.logs || self.metrics
    }

    /// OAuth scope, который выдаётся вместе с этими правами.
    pub fn oauth_scope(self) -> &'static str {
        if self.alerts_write {
            "lognara.read lognara.write"
        } else {
            READ_SCOPE
        }
    }
}

/// Проверяет scope, выданный OAuth-грантом.
pub fn is_granted_scope(scope: &str) -> bool {
    scope == READ_SCOPE || scope == "lognara.read lognara.write"
}

/// Узел, которому ограничен токен. `All` — без ограничения.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Scope {
    #[default]
    All,
    Relay {
        id: i64,
    },
    Agent {
        id: i64,
    },
}

impl Scope {
    pub fn is_valid(self) -> bool {
        match self {
            Scope::All => true,
            Scope::Relay { id } | Scope::Agent { id } => id > 0,
        }
    }

    /// Значения колонок `scope_kind` и `scope_id`; для `all` id равен 0.
    pub fn columns(self) -> (&'static str, i64) {
        match self {
            Scope::All => ("all", 0),
            Scope::Relay { id } => ("relay", id),
            Scope::Agent { id } => ("agent", id),
        }
    }

    pub fn from_columns(kind: &str, id: i64) -> Option<Self> {
        match (kind, id) {
            ("all", 0) => Some(Scope::All),
            ("relay", id) if id > 0 => Some(Scope::Relay { id }),
            ("agent", id) if id > 0 => Some(Scope::Agent { id }),
            _ => None,
        }
    }

    /// Параметр core, которым ограничивается запрос: имя и положительный id узла.
    pub fn core_param(self) -> Option<(&'static str, i64)> {
        match self {
            Scope::All => None,
            Scope::Relay { id } => Some(("relay_id", id)),
            Scope::Agent { id } => Some(("agent_id", id)),
        }
    }
}

/// Права и скоуп, которые сохраняются вместе с токеном или грантом.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Permissions {
    #[serde(flatten)]
    pub rights: Rights,
    pub node_scope: Scope,
}

/// Результат проверки токена, который кладётся в запрос MCP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenAccess {
    pub username: String,
    pub rights: Rights,
    pub node_scope: Scope,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alert_writers_need_at_least_one_data_read_right() {
        let read_all = Rights::default();
        assert!(read_all.is_valid());
        assert!(
            Rights {
                logs: true,
                metrics: false,
                alerts_write: true
            }
            .is_valid()
        );
        assert!(
            Rights {
                logs: false,
                metrics: true,
                alerts_write: true
            }
            .is_valid()
        );
        assert!(
            !Rights {
                logs: false,
                metrics: false,
                alerts_write: false
            }
            .is_valid()
        );
        assert!(
            !Rights {
                logs: false,
                metrics: false,
                alerts_write: true
            }
            .is_valid()
        );
    }

    #[test]
    fn oauth_scope_reflects_write_right() {
        assert_eq!(Rights::default().oauth_scope(), READ_SCOPE);
        let write = Rights {
            alerts_write: true,
            ..Rights::default()
        };
        assert_eq!(write.oauth_scope(), "lognara.read lognara.write");
        assert!(is_granted_scope(write.oauth_scope()));
        assert!(!is_granted_scope("lognara.write"));
    }

    #[test]
    fn scope_columns_round_trip_and_reject_bad_ids() {
        for scope in [Scope::All, Scope::Relay { id: 3 }, Scope::Agent { id: 9 }] {
            let (kind, id) = scope.columns();
            assert_eq!(Scope::from_columns(kind, id), Some(scope));
        }
        assert_eq!(Scope::from_columns("all", 5), None);
        assert_eq!(Scope::from_columns("relay", 0), None);
        assert!(!Scope::Agent { id: 0 }.is_valid());
    }

    #[test]
    fn scope_json_uses_internal_kind_tag() {
        let value = serde_json::to_value(Scope::Relay { id: 4 }).unwrap();
        assert_eq!(value, serde_json::json!({"kind": "relay", "id": 4}));
        let all: Scope = serde_json::from_str(r#"{"kind":"all"}"#).unwrap();
        assert_eq!(all, Scope::All);
    }
}
