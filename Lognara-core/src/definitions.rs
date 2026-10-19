//! Определения метрик: ручные настройки и правила извлечения точек из текста сообщений.
//! Точки хранятся сырыми; kind и aggregation применяются при запросе.
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use anyhow::Result;
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    model::{format_timestamp, now_nanos},
    query::QueryError,
    storage::Core,
};

pub(crate) const MAX_RULES_PER_SOURCE: usize = 1024;
const MAX_LABEL_GROUPS: usize = 4;
/// Различных меток на правило за время жизни процесса.
const LABEL_LIMIT: usize = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Delta,
    Cumulative,
    Gauge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Aggregation {
    Sum,
    Avg,
    Min,
    Max,
    Last,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    #[serde(default)]
    pub environment: Option<String>,
    pub contains: String,
    pub pattern: String,
    #[serde(default)]
    pub value_group: Option<String>,
    #[serde(default)]
    pub label_groups: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Definition {
    /// Стабильный ID извлечения для истории до разделения по серверам.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_rule_id: Option<String>,
    #[serde(default)]
    pub server: String,
    pub id: Uuid,
    pub service: String,
    pub name: String,
    pub title: String,
    pub description: String,
    pub unit: String,
    pub kind: Kind,
    pub aggregation: Aggregation,
    pub enabled: bool,
    pub rule: Option<Rule>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DefinitionInput {
    pub server: String,
    pub service: String,
    pub name: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub unit: String,
    pub kind: Kind,
    #[serde(default)]
    pub aggregation: Option<Aggregation>,
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
    #[serde(default)]
    pub rule: Option<Rule>,
}

fn enabled_by_default() -> bool {
    true
}

impl Definition {
    pub(crate) fn new(id: Uuid, input: DefinitionInput) -> Self {
        let aggregation = input.aggregation.unwrap_or(match input.kind {
            Kind::Gauge => Aggregation::Avg,
            Kind::Delta | Kind::Cumulative => Aggregation::Sum,
        });
        Self {
            legacy_rule_id: None,
            id,
            server: input.server,
            service: input.service,
            name: input.name,
            title: input.title,
            description: input.description,
            unit: input.unit,
            kind: input.kind,
            aggregation,
            enabled: input.enabled,
            rule: input.rule,
            updated_at: format_timestamp(now_nanos()),
        }
    }
}

pub fn validate_service(service: &str) -> Result<(), String> {
    if (1..=128).contains(&service.len()) {
        Ok(())
    } else {
        Err("service must be 1..=128 bytes".into())
    }
}

pub fn validate_environment(environment: Option<&str>) -> Result<(), String> {
    match environment {
        Some(environment) if !(1..=128).contains(&environment.len()) => {
            Err("environment must be 1..=128 bytes".into())
        }
        _ => Ok(()),
    }
}

/// Проверяет запись. others - остальные определения каталога; native - есть ли у (service, name) counter или gauge.
pub fn validate(
    input: &DefinitionInput,
    others: &[Definition],
    native: bool,
) -> Result<(), String> {
    validate_service(&input.service)?;
    validate_service(&input.server).map_err(|_| "server must be 1..=128 bytes")?;
    if !(1..=128).contains(&input.name.len())
        || !input
            .name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.:-".contains(&byte))
    {
        return Err("name must be 1..=128 bytes of letters, digits, _, ., : or -".into());
    }
    if input.title.chars().count() > 128 {
        return Err("title must be at most 128 characters".into());
    }
    if input.description.chars().count() > 2000 {
        return Err("description must be at most 2000 characters".into());
    }
    if input.unit.chars().count() > 32 {
        return Err("unit must be at most 32 characters".into());
    }
    if others.iter().any(|definition| {
        definition.server == input.server
            && definition.service == input.service
            && definition.name == input.name
    }) {
        return Err("definition already exists".into());
    }
    if let Some(rule) = &input.rule {
        validate_rule(rule)?;
        if rule_count(others, &input.server, &input.service) >= MAX_RULES_PER_SOURCE {
            return Err(format!(
                "at most {MAX_RULES_PER_SOURCE} definitions per server/service may have a rule"
            ));
        }
        if native {
            return Err(
                "name collides with a native counter or gauge of this service; choose another name"
                    .into(),
            );
        }
    }
    Ok(())
}

pub(crate) fn rule_count(definitions: &[Definition], server: &str, service: &str) -> usize {
    definitions
        .iter()
        .filter(|definition| {
            definition.server == server
                && definition.service == service
                && definition.rule.is_some()
        })
        .count()
}

/// Проверяет правило и возвращает его regex.
pub fn validate_rule(rule: &Rule) -> Result<Regex, String> {
    validate_environment(rule.environment.as_deref())?;
    if !(1..=256).contains(&rule.contains.len()) {
        return Err("contains must be 1..=256 bytes".into());
    }
    if rule.pattern.len() > 4096 {
        return Err("pattern exceeds 4096 bytes".into());
    }
    let regex = compile(&rule.pattern).map_err(|error| format!("pattern is invalid: {error}"))?;
    let named = |group: &str| regex.capture_names().flatten().any(|name| name == group);
    if let Some(group) = &rule.value_group
        && !named(group)
    {
        return Err(format!(
            "value_group {group} is not a named group in pattern"
        ));
    }
    if rule.label_groups.len() > MAX_LABEL_GROUPS {
        return Err(format!("at most {MAX_LABEL_GROUPS} label_groups"));
    }
    for (index, group) in rule.label_groups.iter().enumerate() {
        if !named(group) {
            return Err(format!(
                "label group {group} is not a named group in pattern"
            ));
        }
        if rule.value_group.as_deref() == Some(group.as_str()) {
            return Err(format!("label group {group} must differ from value_group"));
        }
        if rule.label_groups[..index].contains(group) {
            return Err(format!("label group {group} is repeated"));
        }
    }
    Ok(regex)
}

fn compile(pattern: &str) -> Result<Regex, regex::Error> {
    RegexBuilder::new(pattern).size_limit(1 << 20).build()
}

/// Включённое правило с готовым regex; id и name берутся из определения.
pub struct CompiledRule {
    pub server: Option<String>,
    pub id: String,
    pub name: String,
    pub environment: Option<String>,
    pub contains: String,
    pub regex: Regex,
    pub value_group: Option<String>,
    pub label_groups: Vec<String>,
}

impl CompiledRule {
    pub fn new(id: String, name: String, rule: &Rule) -> Result<Self> {
        Ok(Self {
            server: None,
            id,
            name,
            environment: rule.environment.clone(),
            contains: rule.contains.clone(),
            regex: compile(&rule.pattern)?,
            value_group: rule.value_group.clone(),
            label_groups: rule.label_groups.clone(),
        })
    }
}

/// Включённые правила по сервису и серверу. Core заменяет снимок после каждой записи.
pub struct RuleSet {
    services: HashMap<String, HashMap<String, Vec<CompiledRule>>>,
}

impl RuleSet {
    pub fn load(definitions: &[Definition]) -> Result<Self> {
        let mut services: HashMap<String, HashMap<String, Vec<CompiledRule>>> = HashMap::new();
        let mut counts = HashMap::new();
        for definition in definitions {
            if let Some(rule) = &definition.rule {
                let count = counts
                    .entry((&definition.server, &definition.service))
                    .or_insert(0);
                *count += 1;
                anyhow::ensure!(
                    *count <= MAX_RULES_PER_SOURCE,
                    "at most {MAX_RULES_PER_SOURCE} definitions per server/service may have a rule"
                );
                if !definition.enabled {
                    continue;
                }
                let mut compiled = CompiledRule::new(
                    definition
                        .legacy_rule_id
                        .clone()
                        .unwrap_or_else(|| definition.id.to_string()),
                    definition.name.clone(),
                    rule,
                )?;
                compiled.server = Some(definition.server.clone());
                services
                    .entry(definition.service.clone())
                    .or_default()
                    .entry(definition.server.clone())
                    .or_default()
                    .push(compiled);
            }
        }
        Ok(Self { services })
    }

    pub fn for_source(&self, server: &str, service: &str) -> &[CompiledRule] {
        self.services
            .get(service)
            .and_then(|servers| servers.get(server))
            .map_or(&[], Vec::as_slice)
    }
}

/// Лимит различных меток на правило. Состояние только в памяти: после перезапуска начинается заново.
#[derive(Default)]
pub struct LabelGuard {
    seen: HashMap<String, HashSet<String>>,
}

impl LabelGuard {
    /// Пропускает известные метки и новые, пока у правила не набрано LABEL_LIMIT.
    pub fn admit(&mut self, rule_id: &str, labels: &str) -> bool {
        let seen = self.seen.entry(rule_id.to_owned()).or_default();
        if seen.contains(labels) {
            true
        } else if seen.len() < LABEL_LIMIT {
            seen.insert(labels.to_owned());
            true
        } else {
            false
        }
    }
}

pub fn list(core: &Core) -> Result<Vec<Definition>, QueryError> {
    Ok(core.journal.catalog.lock().unwrap().definitions()?)
}

pub fn create(core: &Core, input: DefinitionInput) -> Result<Definition, QueryError> {
    // Проверка, запись и публикация правил идут под одной блокировкой каталога.
    let catalog = core.journal.catalog.lock().unwrap();
    let mut definitions = catalog.definitions()?;
    let native = has_native(core, &catalog, &input)?;
    validate(&input, &definitions, native).map_err(QueryError::Invalid)?;
    let definition = Definition::new(Uuid::new_v4(), input);
    definitions.push(definition.clone());
    let rules = RuleSet::load(&definitions)?;
    catalog.insert_definition(&definition)?;
    *core.rules.write().unwrap() = Arc::new(rules);
    Ok(definition)
}

/// None - определения с таким id нет.
pub fn update(
    core: &Core,
    id: &str,
    input: DefinitionInput,
) -> Result<Option<Definition>, QueryError> {
    let Ok(id) = Uuid::parse_str(id) else {
        return Ok(None);
    };
    let catalog = core.journal.catalog.lock().unwrap();
    let mut definitions = catalog.definitions()?;
    let Some(position) = definitions
        .iter()
        .position(|definition| definition.id == id)
    else {
        return Ok(None);
    };
    let previous = definitions.remove(position);
    let native = has_native(core, &catalog, &input)?;
    validate(&input, &definitions, native).map_err(QueryError::Invalid)?;
    let mut definition = Definition::new(id, input);
    definition.legacy_rule_id = previous.legacy_rule_id;
    definitions.push(definition.clone());
    let rules = RuleSet::load(&definitions)?;
    if !catalog.update_definition(&definition)? {
        return Ok(None);
    }
    *core.rules.write().unwrap() = Arc::new(rules);
    Ok(Some(definition))
}

pub fn delete(core: &Core, id: &str) -> Result<bool, QueryError> {
    let Ok(id) = Uuid::parse_str(id) else {
        return Ok(false);
    };
    let catalog = core.journal.catalog.lock().unwrap();
    let mut definitions = catalog.definitions()?;
    let before = definitions.len();
    definitions.retain(|definition| definition.id != id);
    if definitions.len() == before {
        return Ok(false);
    }
    let rules = RuleSet::load(&definitions)?;
    if !catalog.delete_definition(&id.to_string())? {
        return Ok(false);
    }
    *core.rules.write().unwrap() = Arc::new(rules);
    Ok(true)
}

pub(crate) fn has_native(
    core: &Core,
    catalog: &crate::catalog::Catalog,
    input: &DefinitionInput,
) -> Result<bool, QueryError> {
    let persisted: bool = catalog.connection.query_row("SELECT EXISTS(SELECT 1 FROM scoped_series WHERE server=?1 AND service=?2 AND name=?3 AND origin IN ('counter','gauge'))", rusqlite::params![input.server,input.service,input.name], |r|r.get(0)).map_err(anyhow::Error::from)?;
    Ok(persisted
        || core.snapshot().active.as_ref().is_some_and(|active| {
            active
                .scoped
                .names
                .keys()
                .any(|(server, service, name, origin, _)| {
                    server == &input.server
                        && service == &input.service
                        && name == &input.name
                        && origin != "rule"
                })
        }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rule_quota_counts_disabled_rules_but_is_independent_per_source() {
        let input = |server: &str, service: &str, name: &str| DefinitionInput {
            server: server.into(),
            service: service.into(),
            name: name.into(),
            title: String::new(),
            description: String::new(),
            unit: String::new(),
            kind: Kind::Delta,
            aggregation: None,
            enabled: false,
            rule: Some(Rule {
                environment: None,
                contains: "done".into(),
                pattern: "done".into(),
                value_group: None,
                label_groups: vec![],
            }),
        };
        let definitions: Vec<_> = (0..MAX_RULES_PER_SOURCE)
            .map(|i| Definition::new(Uuid::new_v4(), input("a", "api", &format!("metric_{i}"))))
            .collect();
        assert!(validate(&input("a", "api", "extra"), &definitions, false).is_err());
        assert!(validate(&input("b", "api", "extra"), &definitions, false).is_ok());
        assert!(validate(&input("a", "worker", "extra"), &definitions, false).is_ok());
        // Изменение существующего правила не занимает дополнительного места.
        assert!(validate(&input("a", "api", "metric_0"), &definitions[1..], false).is_ok());
        let mut native = input("a", "api", "native");
        native.rule = None;
        assert!(validate(&native, &definitions, true).is_ok());
        assert!(RuleSet::load(&definitions).is_ok());
        let mut overflow = definitions;
        overflow.push(Definition::new(Uuid::new_v4(), input("a", "api", "extra")));
        assert!(RuleSet::load(&overflow).is_err());
    }

    #[test]
    fn label_guard_admits_known_labels_and_limits_new_ones_per_rule() {
        let mut guard = LabelGuard::default();
        for i in 0..LABEL_LIMIT {
            assert!(guard.admit("rule", &format!("code={i}")));
        }
        assert!(!guard.admit("rule", "code=new"));
        assert!(guard.admit("rule", "code=0"));
        assert!(guard.admit("other", "code=new"));
    }
}
