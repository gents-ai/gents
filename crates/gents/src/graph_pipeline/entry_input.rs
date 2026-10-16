use anyhow::{Context, Result};
use serde_json::{Map, Value};

use super::types::PlannedEntry;

const MAX_INPUT_SCHEMA_BYTES: usize = 64 * 1024;

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

