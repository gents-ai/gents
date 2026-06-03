//! Rendering and validation of the structured query contract into DefraDB
//! GraphQL syntax. All field/operator names are validated as identifiers and
//! all string literals are escaped via [`escape_graphql_string`], so untrusted
//! input cannot break out of the query.

use anyhow::{bail, Result};
use serde_json::Value;

use crate::graphql::escape_graphql_string;

/// Render a JSON filter object into DefraDB GraphQL filter object syntax.
///
/// e.g. `{"status": {"_eq": "pending"}}` -> `{ status: { _eq: "pending" } }`.
///
/// The top-level filter must be a JSON object. Keys (field names and operators
/// such as `_eq`, `_and`) are validated as GraphQL identifiers; string values
/// are escaped.
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

/// Validate that a name is a safe GraphQL identifier: ASCII alphanumeric and
/// underscore, starting with a letter or underscore. This covers field names
/// (`status`, `_docID`) and filter operators (`_eq`, `_and`).
pub(crate) fn validate_identifier(name: &str) -> Result<()> {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => bail!("invalid identifier {name:?}: must start with a letter or underscore"),
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        bail!("invalid identifier {name:?}: only letters, digits, and underscore are allowed");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn renders_simple_equality_filter() {
        let out = render_filter(&json!({ "status": { "_eq": "pending" } })).unwrap();
        assert_eq!(out, r#"{ status: { _eq: "pending" } }"#);
    }

    #[test]
    fn renders_numeric_and_bool_operands_without_quotes() {
        let out = render_filter(&json!({ "retry_count": { "_gt": 0 } })).unwrap();
        assert_eq!(out, "{ retry_count: { _gt: 0 } }");

        let out = render_filter(&json!({ "interrupt": { "_eq": true } })).unwrap();
        assert_eq!(out, "{ interrupt: { _eq: true } }");
    }

    #[test]
    fn renders_in_operator_with_array() {
        let out = render_filter(&json!({ "tool_name": { "_in": ["a", "b"] } })).unwrap();
        assert_eq!(out, r#"{ tool_name: { _in: ["a", "b"] } }"#);
    }

    #[test]
    fn renders_boolean_composition() {
        let out = render_filter(&json!({
            "_and": [
                { "status": { "_eq": "x" } },
                { "retry_count": { "_gt": 0 } }
            ]
        }))
        .unwrap();
        assert_eq!(
            out,
            r#"{ _and: [{ status: { _eq: "x" } }, { retry_count: { _gt: 0 } }] }"#
        );
    }

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

    #[test]
    fn rejects_non_object_top_level_filter() {
        let err = render_filter(&json!("not an object")).unwrap_err();
        assert!(err.to_string().contains("must be a JSON object"), "{err}");
    }
}
