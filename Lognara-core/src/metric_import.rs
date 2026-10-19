//! Переносимые наборы правил: проверка, разрешение конфликтов и атомарное применение.
use crate::{
    definitions::{self, Definition, DefinitionInput, RuleSet},
    query::QueryError,
    storage::Core,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{collections::HashSet, sync::Arc};
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub server: String,
    pub service: String,
    pub bundle: Value,
    #[serde(default)]
    pub revision: Option<String>,
    #[serde(default)]
    pub actions: Vec<Action>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bundle {
    pub format: String,
    pub version: u32,
    pub metrics: Vec<Value>,
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "lowercase", deny_unknown_fields)]
pub enum Action {
    Create,
    Skip,
    Rename { name: String },
    Replace,
}

#[derive(Serialize)]
pub struct Problem {
    pub index: Option<usize>,
    pub field: String,
    pub message: String,
}

#[derive(Serialize)]
pub struct Conflict {
    pub index: usize,
    pub existing: Definition,
}

#[derive(Serialize)]
pub struct Check {
    pub revision: String,
    pub errors: Vec<Problem>,
    pub conflicts: Vec<Conflict>,
    pub metrics: Vec<Value>,
}

fn revision(definitions: &[Definition]) -> Result<String, QueryError> {
    Ok(hex::encode(Sha256::digest(
        serde_json::to_vec(definitions).map_err(anyhow::Error::from)?,
    )))
}

fn parse(request: &Request) -> Result<Bundle, QueryError> {
    definitions::validate_service(&request.server).map_err(QueryError::Invalid)?;
    definitions::validate_service(&request.service).map_err(QueryError::Invalid)?;
    let bundle: Bundle = serde_json::from_value(request.bundle.clone())
        .map_err(|e| QueryError::Invalid(format!("bundle: {e}")))?;
    if bundle.format != "lognara-metrics" || bundle.version != 1 {
        return Err(QueryError::Invalid(
            "bundle.format/version: expected lognara-metrics version 1".into(),
        ));
    }
    if !(1..=definitions::MAX_RULES_PER_SOURCE).contains(&bundle.metrics.len()) {
        return Err(QueryError::Invalid(format!(
            "bundle.metrics: expected 1..{} metrics",
            definitions::MAX_RULES_PER_SOURCE
        )));
    }
    Ok(bundle)
}

fn input(request: &Request, metric: &Value) -> Result<DefinitionInput, String> {
    let mut value = metric
        .as_object()
        .cloned()
        .ok_or("metric must be an object")?;
    if value.contains_key("server") || value.contains_key("service") {
        return Err("server/service: choose the source in Dock".into());
    }
    if value.get("rule").is_none_or(Value::is_null) {
        return Err("rule: required for imported metrics".into());
    }
    value.insert("server".into(), request.server.clone().into());
    value.insert("service".into(), request.service.clone().into());
    serde_json::from_value(Value::Object(value)).map_err(|e| e.to_string())
}

fn native(
    core: &Core,
    catalog: &crate::catalog::Catalog,
    input: &DefinitionInput,
) -> Result<bool, QueryError> {
    definitions::has_native(core, catalog, input)
}

pub fn check(core: &Core, request: Request) -> Result<Check, QueryError> {
    let bundle = parse(&request)?;
    let catalog = core.journal.catalog.lock().unwrap();
    let definitions = catalog.definitions()?;
    let mut result = Check {
        revision: revision(&definitions)?,
        errors: vec![],
        conflicts: vec![],
        metrics: bundle.metrics.clone(),
    };
    let mut names = HashSet::new();
    for (index, metric) in bundle.metrics.iter().enumerate() {
        let error = match input(&request, metric) {
            Err(error) => Some(error),
            Ok(input) => {
                if let Some(existing) = definitions.iter().find(|d| {
                    d.server == input.server && d.service == input.service && d.name == input.name
                }) {
                    result.conflicts.push(Conflict {
                        index,
                        existing: existing.clone(),
                    });
                }
                if !names.insert(input.name.clone()) {
                    Some("name: repeated within bundle".into())
                } else {
                    definitions::validate(&input, &[], native(core, &catalog, &input)?).err()
                }
            }
        };
        if let Some(message) = error {
            let field = problem_field(&message);
            result.errors.push(Problem {
                index: Some(index),
                field,
                message,
            });
        }
    }
    let additions = bundle.metrics.len().saturating_sub(
        result
            .conflicts
            .iter()
            .filter(|conflict| conflict.existing.rule.is_some())
            .count(),
    );
    if definitions::rule_count(&definitions, &request.server, &request.service) + additions
        > definitions::MAX_RULES_PER_SOURCE
    {
        result.errors.push(Problem {
            index: None,
            field: "metrics".into(),
            message: format!(
                "at most {} definitions per server/service may have a rule; skip metrics or remove existing rules",
                definitions::MAX_RULES_PER_SOURCE
            ),
        });
    }
    Ok(result)
}

pub fn apply(core: &Core, request: Request) -> Result<Value, QueryError> {
    let bundle = parse(&request)?;
    if request.actions.len() != bundle.metrics.len() {
        return Err(QueryError::Invalid(
            "actions: one action per metric is required".into(),
        ));
    }
    let catalog = core.journal.catalog.lock().unwrap();
    let mut definitions = catalog.definitions()?;
    if request.revision.as_deref() != Some(revision(&definitions)?.as_str()) {
        return Err(QueryError::Conflict);
    }
    let mut writes = vec![];
    let mut created = 0;
    let mut replaced = 0;
    let mut skipped = 0;
    let mut targets = HashSet::new();
    for (index, (metric, action)) in bundle.metrics.iter().zip(&request.actions).enumerate() {
        if matches!(action, Action::Skip) {
            skipped += 1;
            continue;
        }
        let mut input = input(&request, metric)
            .map_err(|e| QueryError::Invalid(format!("metrics[{index}]: {e}")))?;
        if let Action::Rename { name } = action {
            input.name = name.clone();
        }
        if !targets.insert(input.name.clone()) {
            return Err(QueryError::Invalid(format!(
                "metrics[{index}].name: repeated target"
            )));
        }
        let existing = definitions.iter().position(|d| {
            d.server == input.server && d.service == input.service && d.name == input.name
        });
        let mut legacy_rule_id = None;
        let id = match action {
            Action::Replace => {
                let position = existing.ok_or(QueryError::Conflict)?;
                replaced += 1;
                let previous = definitions.remove(position);
                legacy_rule_id = previous.legacy_rule_id;
                previous.id
            }
            _ => {
                if existing.is_some() {
                    return Err(if matches!(action, Action::Rename { .. }) {
                        QueryError::Invalid(format!(
                            "metrics[{index}].name: definition already exists"
                        ))
                    } else {
                        QueryError::Conflict
                    });
                }
                created += 1;
                Uuid::new_v4()
            }
        };
        definitions::validate(&input, &definitions, native(core, &catalog, &input)?)
            .map_err(|e| QueryError::Invalid(format!("metrics[{index}]: {e}")))?;
        let mut definition = Definition::new(id, input);
        definition.legacy_rule_id = legacy_rule_id;
        definitions.push(definition.clone());
        writes.push(definition);
    }
    let rules = RuleSet::load(&definitions)?;
    let tx = catalog
        .connection
        .unchecked_transaction()
        .map_err(anyhow::Error::from)?;
    for d in &writes {
        tx.execute("INSERT INTO series_definitions VALUES(?1,?2,?3,?4,?5) ON CONFLICT(id) DO UPDATE SET body=excluded.body",
            rusqlite::params![d.id.to_string(),d.server,d.service,d.name,serde_json::to_string(d).map_err(anyhow::Error::from)?]).map_err(anyhow::Error::from)?;
    }
    tx.commit().map_err(anyhow::Error::from)?;
    *core.rules.write().unwrap() = Arc::new(rules);
    Ok(serde_json::json!({"created":created,"replaced":replaced,"skipped":skipped}))
}

fn problem_field(message: &str) -> String {
    for (text, field) in [
        ("value_group", "rule.value_group"),
        ("label group", "rule.label_groups"),
        ("label_groups", "rule.label_groups"),
        ("environment", "rule.environment"),
        ("contains", "rule.contains"),
        ("pattern", "rule.pattern"),
        ("name", "name"),
        ("description", "description"),
        ("title", "title"),
        ("unit", "unit"),
    ] {
        if message.contains(text) {
            return field.into();
        }
    }
    if let Some(field) = message.split('\x60').nth(1) {
        return field.into();
    }
    "metric".into()
}
