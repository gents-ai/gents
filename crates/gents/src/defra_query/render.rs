use anyhow::{bail, Result};
use serde_json::Value;

use crate::graphql::escape_graphql_string;

pub(crate) fn render_filter(filter: &Value) -> Result<String> {
    if !filter.is_object() {
        bail!("filter must be a JSON object");
    }
    render_value(filter)
}

fn render_value(value: &Value) -> Result<String> {
    match value {
        Value::Null => Ok("null".to_string()),
        Value::Bool(b) => Ok(b.to_string()),
        Value::Number(n) => Ok(n.to_string()),
        Value::String(s) => Ok(format!("\"{}\"", escape_graphql_string(s))),
        Value::Array(items) => {
            let rendered = items.iter().map(render_value).collect::<Result<Vec<_>>>()?;
            Ok(format!("[{}]", rendered.join(", ")))
        }
        Value::Object(map) => {
            if map.is_empty() {
                return Ok("{}".to_string());
            }
            let mut parts = Vec::with_capacity(map.len());
            for (key, val) in map {
                validate_identifier(key)?;
                parts.push(format!("{}: {}", key, render_value(val)?));
            }
            Ok(format!("{{ {} }}", parts.join(", ")))
        }
    }
}

pub(crate) fn validate_identifier(name: &str) -> Result<()> {
    Ok(crate::graphql::validate_graphql_name(name)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn escapes_string_values_to_prevent_injection() {
        let out = render_filter(&json!({ "content": { "_eq": "a\" } evil { x" } })).unwrap();
        assert_eq!(out, r#"{ content: { _eq: "a\" } evil { x" } }"#);
    }

    #[test]
    fn rejects_injection_in_field_name() {
        let err = render_filter(&json!({ "status\" } evil { ": { "_eq": "x" } })).unwrap_err();
        assert!(err.to_string().contains("invalid identifier"), "{err}");
    }
}
