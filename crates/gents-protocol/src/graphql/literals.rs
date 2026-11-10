use serde_json::Value;

use super::{escape_graphql_string, validate_graphql_name, GraphqlError, Result};

pub fn graphql_string_list_literal(values: &[String]) -> String {
    format!(
        "[{}]",
        values
            .iter()
            .map(|value| format!(r#""{}""#, escape_graphql_string(value)))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// Converts a `serde_json::Value` into a GraphQL input literal for use in
/// DefraDB document mutations (create/update/upsert payloads).
///
/// # Empty list handling
///
/// An empty `Value::Array` is rendered as the literal `null`, never `[]`.
/// DefraDB types a bare `[]` as `JsonArray([])`. This is incompatible with
/// `NillableStringArray` (`[String]`) columns (used for tags, skill IDs,
/// agent target IDs, and similar canonical lists). A create may appear to
/// succeed while storing the wrong type; any
/// subsequent update then fails re-validation.
///
/// All array fields that currently flow through this function for DefraDB
/// collections are list-of-string columns. The blanket rule is therefore
/// safe for the documented use cases in the apply/desired-state machinery.
pub fn graphql_input_literal(value: &Value) -> Result<String> {
    match value {
        Value::Null => Ok("null".to_string()),
        Value::Bool(value) => Ok(value.to_string()),
        Value::Number(value) => Ok(value.to_string()),
        Value::String(value) => Ok(graphql_string_literal(value)),
        Value::Array(values) => {
            if values.is_empty() {
                return Ok("null".to_string());
            }
            let rendered = values
                .iter()
                .map(graphql_input_literal)
                .collect::<Result<Vec<_>>>()?;
            Ok(format!("[{}]", rendered.join(", ")))
        }
        Value::Object(map) => {
            let rendered = map
                .iter()
                .map(|(key, value)| {
                    // Keys are in identifier position, where escaping cannot
                    // apply; an unvalidated key is an injection into the mutation.
                    validate_graphql_name(key)?;
                    Ok(format!("{key}: {}", graphql_input_literal(value)?))
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(format!("{{ {} }}", rendered.join(", ")))
        }
    }
}

pub fn nullable_string_field(name: &str, value: Option<&str>) -> String {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => format!(r#"{name}: "{}""#, escape_graphql_string(value)),
        None => format!("{name}: null"),
    }
}

pub fn graphql_bool_literal(value: bool) -> &'static str {
    if value {
        "true"
    } else {
        "false"
    }
}

pub fn normalize_optional_rfc3339(value: Option<&str>) -> Result<Option<String>> {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        Some(raw) => {
            let parsed = chrono::DateTime::parse_from_rfc3339(raw).map_err(|source| {
                GraphqlError::Rfc3339 {
                    raw: raw.to_owned(),
                    source,
                }
            })?;
            Ok(Some(
                parsed
                    .with_timezone(&chrono::Utc)
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            ))
        }
        None => Ok(None),
    }
}

pub fn optional_i64_field(name: &str, value: Option<i64>) -> Option<String> {
    value.map(|value| format!("{name}: {value}"))
}

pub fn optional_f64_field(name: &str, value: Option<f64>) -> Option<String> {
    value.map(|value| format!("{name}: {value}"))
}

pub fn optional_bool_field(name: &str, value: Option<bool>) -> Option<String> {
    value.map(|value| format!("{name}: {}", graphql_bool_literal(value)))
}

pub fn optional_i64_list_field(name: &str, value: Option<&[i64]>) -> Option<String> {
    let values = value?;
    if values.is_empty() {
        Some(format!("{name}: null"))
    } else {
        Some(format!(
            "{name}: [{}]",
            values
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }
}

pub fn optional_string_field(name: &str, value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| format!(r#"{name}: "{}""#, escape_graphql_string(value)))
}

pub fn string_list_field(name: &str, values: &[String]) -> Option<String> {
    // Empty lists serialize as `null`, NOT `[]`. A bare `[]` GraphQL literal is
    // typed by DefraDB as `JsonArray([])`, incompatible with a
    // `NillableStringArray` (`[String]`) column: the create "succeeds" but
    // stores a JsonArray, and every later update fails re-validation.
    if values.is_empty() {
        Some(format!("{name}: null"))
    } else {
        Some(format!("{name}: {}", graphql_string_list_literal(values)))
    }
}

fn graphql_string_literal(value: &str) -> String {
    format!(r#""{}""#, escape_graphql_string(value))
}
