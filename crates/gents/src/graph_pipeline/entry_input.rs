//! Admits operator-supplied input for a graph entry against its
//! pack-declared JSON Schema, and validates that schema at pack load time.
//!
//! An entry without a declared `input_schema` takes an unvalidated JSON
//! object, exactly as before this contract existed.

use anyhow::{Context, Result};
use serde_json::{Map, Value};

use super::types::PlannedEntry;

/// A pack's own entry `input_schema` may be at most this large, so an
/// admission check never compiles an unbounded document.
const MAX_INPUT_SCHEMA_BYTES: usize = 64 * 1024;

/// Validated at pack load: the schema must compile, fit the size ceiling,
/// and describe an object (the only shape an entry input can take).
pub fn validate_input_schema(schema: &Value) -> Result<()> {
    anyhow::ensure!(
        schema.get("type").and_then(Value::as_str) == Some("object"),
        "entry input_schema must declare \"type\": \"object\""
    );
    let encoded = serde_json::to_vec(schema).context("encoding entry input_schema")?;
    anyhow::ensure!(
        encoded.len() <= MAX_INPUT_SCHEMA_BYTES,
        "entry input_schema is {} bytes, over the {MAX_INPUT_SCHEMA_BYTES}-byte ceiling",
        encoded.len()
    );
    jsonschema::validator_for(schema).context("entry input_schema does not compile")?;
    Ok(())
}

/// Fills top-level property defaults absent from `input`, then validates the
/// merged object against `entry.input_schema`. An entry without a schema
/// requires only that `input` is a JSON object.
pub fn admit_operator_input(entry: &PlannedEntry, input: Value) -> Result<Map<String, Value>> {
    let mut input = match input {
        Value::Object(input) => input,
        _ => anyhow::bail!("entry {:?} input must be a JSON object", entry.name),
    };
    let Some(schema) = entry.input_schema.as_ref() else {
        return Ok(input);
    };
    if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
        for (key, property) in properties {
            if input.contains_key(key) {
                continue;
            }
            if let Some(default) = property.get("default") {
                input.insert(key.clone(), default.clone());
            }
        }
    }
    let validator = jsonschema::validator_for(schema)
        .with_context(|| format!("entry {:?} input_schema does not compile", entry.name))?;
    let instance = Value::Object(input.clone());
    let mut violations: Vec<String> = validator
        .iter_errors(&instance)
        .map(|error| error.to_string())
        .collect();
    if !violations.is_empty() {
        violations.sort();
        anyhow::bail!(
            "entry {:?} input does not satisfy its schema: {}",
            entry.name,
            violations.join("; ")
        );
    }
    Ok(input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph_pipeline::PortRef;
    use serde_json::json;

    fn entry(schema: Option<Value>) -> PlannedEntry {
        PlannedEntry {
            name: "job".to_owned(),
            collection: "Job".to_owned(),
            schema: "Job/v1".to_owned(),
            input_contract: None,
            to: PortRef {
                node_id: "worker".to_owned(),
                port: "job".to_owned(),
            },
            target: crate::graph_pipeline::StageTarget::Task {
                task_id: "worker-task".to_owned(),
            },
            correlation_field: "correlation".to_owned(),
            input_schema: schema,
            prepare: None,
        }
    }

    #[test]
    fn no_schema_requires_only_an_object() {
        let entry = entry(None);
        let admitted = admit_operator_input(&entry, json!({"anything": 1})).unwrap();
        assert_eq!(admitted["anything"], 1);
        let error = admit_operator_input(&entry, json!("not an object")).unwrap_err();
        assert!(format!("{error:#}").contains("must be a JSON object"));
    }

    #[test]
    fn missing_defaults_are_inserted() {
        let schema = json!({
            "type": "object",
            "properties": {
                "scope": {"type": "string", "default": "everything"},
                "question": {"type": "string"},
            },
            "required": ["question"],
        });
        let entry = entry(Some(schema));
        let admitted = admit_operator_input(&entry, json!({"question": "why?"})).unwrap();
        assert_eq!(admitted["scope"], "everything");
        assert_eq!(admitted["question"], "why?");
        // An explicit value is never overridden by the schema default.
        let admitted =
            admit_operator_input(&entry, json!({"question": "why?", "scope": "narrow"})).unwrap();
        assert_eq!(admitted["scope"], "narrow");
    }

    #[test]
    fn every_violation_is_reported_sorted_and_joined() {
        let schema = json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["a", "b"],
            "properties": {"a": {"type": "string"}, "b": {"type": "string"}},
        });
        let entry = entry(Some(schema));
        let error = admit_operator_input(&entry, json!({"c": 1})).unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("; "), "{message}");
    }
}
