use anyhow::{bail, Result};
use serde_json::{json, Value};

use crate::graphql::escape_graphql_string;

pub(super) fn parameters(schema: &str) -> Result<Value> {
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
            _ => bail!("unsupported bounded write field type `{schema}`"),
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

pub(super) fn validate_input(
    schema: &str,
    required: bool,
    filled: bool,
    value: Option<&Value>,
) -> Result<()> {
    if filled {
        if value.is_some() {
            bail!("runtime-filled field must not be supplied");
        }
        return Ok(());
    }
    match value {
        None if required => bail!("required field missing"),
        None => Ok(()),
        Some(value) => literal(schema, value).map(|_| ()),
    }
}

pub(super) fn literal(schema: &str, value: &Value) -> Result<String> {
    let nullable = !schema.ends_with('!');
    if value.is_null() {
        if !nullable {
            bail!("null is not allowed for `{schema}`");
        }
        return Ok("null".into());
    }
    let base = schema.trim_end_matches('!');
    if let Some(item) = base.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        let values = value
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("expected `{schema}`"))?;
        if values.is_empty() {
            if !nullable {
                bail!("empty nonnullable lists cannot be written by DefraDB");
            }
            return Ok("null".into());
        }
        return Ok(format!(
            "[{}]",
            values
                .iter()
                .map(|v| literal(item, v))
                .collect::<Result<Vec<_>>>()?
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
        _ => bail!("expected native JSON value matching `{schema}`; values are not coerced"),
    }
}

fn json_literal(value: &Value) -> Result<String> {
    reject_empty_json_arrays(value)?;
    gents_protocol::graphql::graphql_input_literal(value)
}

// The shared mutation renderer maps empty lists to null. A JSON scalar must not
// silently change its contents, so reject that case before delegating rendering.
fn reject_empty_json_arrays(value: &Value) -> Result<()> {
    match value {
        Value::Array(values) => {
            if values.is_empty() {
                bail!("empty JSON arrays cannot be represented in a DefraDB mutation without changing the value");
            }
            values.iter().try_for_each(reject_empty_json_arrays)
        }
        Value::Object(fields) => fields.values().try_for_each(reject_empty_json_arrays),
        _ => Ok(()),
    }
}
