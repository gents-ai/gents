use serde_json::{Map, Value};

use super::runtime::GraphPipelineError;
use super::types::PlannedEntry;

const MAX_INPUT_SCHEMA_BYTES: usize = 64 * 1024;

pub fn validate_input_schema(schema: &Value) -> Result<(), GraphPipelineError> {
    if schema.get("type").and_then(Value::as_str) != Some("object") {
        return Err(GraphPipelineError::InputSchemaNotObject);
    }
    let encoded = serde_json::to_vec(schema).map_err(GraphPipelineError::EncodeInputSchema)?;
    if encoded.len() > MAX_INPUT_SCHEMA_BYTES {
        return Err(GraphPipelineError::InputSchemaTooLarge {
            bytes: encoded.len(),
            ceiling: MAX_INPUT_SCHEMA_BYTES,
        });
    }
    jsonschema::validator_for(schema).map_err(GraphPipelineError::InputSchemaCompile)?;
    Ok(())
}

pub fn admit_operator_input(
    entry: &PlannedEntry,
    input: Value,
) -> Result<Map<String, Value>, GraphPipelineError> {
    let mut input = match input {
        Value::Object(input) => input,
        _ => {
            return Err(GraphPipelineError::EntryInputNotObject {
                entry: entry.name.clone(),
            })
        }
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
    let validator = jsonschema::validator_for(schema).map_err(|source| {
        GraphPipelineError::EntryInputSchemaCompile {
            entry: entry.name.clone(),
            source,
        }
    })?;
    let instance = Value::Object(input.clone());
    let mut violations: Vec<String> = validator
        .iter_errors(&instance)
        .map(|error| error.to_string())
        .collect();
    if !violations.is_empty() {
        violations.sort();
        return Err(GraphPipelineError::EntryInputViolations {
            entry: entry.name.clone(),
            violations: violations.join("; "),
        });
    }
    Ok(input)
}
