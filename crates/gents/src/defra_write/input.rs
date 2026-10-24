use serde_json::{json, Value};

use super::DefraWriteFailure;
use crate::graphql::escape_graphql_string;

pub(crate) fn parameters(schema: &str) -> Result<Value, DefraWriteFailure> {
    let base = schema.trim_end_matches('!');
    let mut shape = if let Some(item) = base.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        json!({"type":"array", "items":parameters(item)?})
    } else {
        match base {
            "String" | "ID" | "DateTime" | "Blob" => json!({"type":"string"}),
            "Int" => json!({"type":"integer"}),
            "Float32" | "Float64" | "Float" => json!({"type":"number"}),
            "Boolean" => json!({"type":"boolean"}),
            "JSON" => json!({"type":["object", "array", "string", "number", "boolean"]}),
            _ => {
                return Err(DefraWriteFailure::UnsupportedFieldType {
                    schema: schema.to_owned(),
                })
            }
        }
    };
    if !schema.ends_with('!') {
        if let Some(kind) = shape.get("type").cloned() {
            let mut kinds = match kind {
                Value::Array(kinds) => kinds,
                kind => vec![kind],
            };
            kinds.push(json!("null"));
            shape["type"] = Value::Array(kinds);
        }
    }
    Ok(shape)
}

pub fn can_hold_canonical_count(schema: &str) -> bool {
    if schema.trim_end_matches('!') == "DateTime" {
        return false;
    }
    parameters(schema).is_ok_and(|shape| match shape.get("type") {
        Some(Value::Array(kinds)) => kinds.iter().any(admits_count),
        Some(kind) => admits_count(kind),
        None => false,
    })
}

fn admits_count(kind: &Value) -> bool {
    matches!(kind.as_str(), Some("integer" | "number" | "string"))
}

pub(crate) fn validate_input(
    schema: &str,
    required: bool,
    filled: bool,
    value: Option<&Value>,
) -> Result<(), DefraWriteFailure> {
    if filled {
        if value.is_some() {
            return Err(DefraWriteFailure::RuntimeFilledSupplied);
        }
        return Ok(());
    }
    match value {
        None if required => Err(DefraWriteFailure::RequiredFieldMissing),
        None => Ok(()),
        Some(value) => literal(schema, value).map(|_| ()),
    }
}

pub(super) fn literal(schema: &str, value: &Value) -> Result<String, DefraWriteFailure> {
    let nullable = !schema.ends_with('!');
    if value.is_null() {
        if !nullable {
            return Err(DefraWriteFailure::NullNotAllowed {
                schema: schema.to_owned(),
            });
        }
        return Ok("null".into());
    }
    let base = schema.trim_end_matches('!');
    if let Some(item) = base.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        let values = value
            .as_array()
            .ok_or_else(|| DefraWriteFailure::ExpectedSchemaValue {
                schema: schema.to_owned(),
            })?;
        if values.is_empty() {
            if !nullable {
                return Err(DefraWriteFailure::EmptyNonnullableList);
            }
            return Ok("null".into());
        }
        return Ok(format!(
            "[{}]",
            values
                .iter()
                .map(|v| literal(item, v))
                .collect::<Result<Vec<_>, _>>()?
                .join(", ")
        ));
    }
    match (base, value) {
        ("String" | "ID" | "DateTime" | "Blob", Value::String(text)) => {
            Ok(format!("\"{}\"", escape_graphql_string(text)))
        }
        ("Int", Value::Number(number)) if number.as_i64().is_some() => Ok(number.to_string()),
        ("Float32" | "Float64" | "Float", Value::Number(number)) => Ok(number.to_string()),
        ("Boolean", Value::Bool(value)) => Ok(value.to_string()),
        ("JSON", _) => json_literal(value),
        _ => Err(DefraWriteFailure::NativeValueMismatch {
            schema: schema.to_owned(),
        }),
    }
}

fn json_literal(value: &Value) -> Result<String, DefraWriteFailure> {
    reject_empty_json_arrays(value)?;
    Ok(gents_protocol::graphql::graphql_input_literal(value)?)
}

// The shared mutation renderer maps empty lists to null. A JSON scalar must not
// silently change its contents, so reject that case before delegating rendering.
fn reject_empty_json_arrays(value: &Value) -> Result<(), DefraWriteFailure> {
    match value {
        Value::Array(values) => {
            if values.is_empty() {
                return Err(DefraWriteFailure::EmptyJsonArray);
            }
            values.iter().try_for_each(reject_empty_json_arrays)
        }
        Value::Object(fields) => fields.values().try_for_each(reject_empty_json_arrays),
        _ => Ok(()),
    }
}
