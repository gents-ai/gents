use anyhow::{bail, Context, Result};
use defra_node::EmbeddedNode;

pub(super) async fn execute_mutation(
    node: &EmbeddedNode,
    mutation: &str,
    operation: &str,
) -> Result<()> {
    let response = node.execute(mutation).await;
    if response.has_errors() {
        bail!(
            "{operation} failed: {}",
            response
                .errors
                .iter()
                .map(|error| error.message.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        );
    }
    Ok(())
}

pub(super) fn join_fields(fields: &[Option<String>]) -> String {
    fields
        .iter()
        .flatten()
        .cloned()
        .collect::<Vec<_>>()
        .join(",\n                    ")
}

pub(super) fn graphql_string_field(name: &str, value: Option<&str>) -> String {
    match normalize_optional_string(value) {
        Some(value) => format!(r#"{name}: "{}""#, escape_graphql_string(value)),
        None => format!("{name}: null"),
    }
}

pub(super) fn graphql_string_list_field(name: &str, values: &[String]) -> String {
    // Empty lists serialize as `null`, never `[]`: a bare `[]` literal is typed
    // by DefraDB as JsonArray and corrupts a NillableStringArray column (create
    // stores JsonArray, later updates fail re-validation). Matches the
    // `graphql_string_field` null idiom and the protocol-level `string_list_field` fix.
    if values.is_empty() {
        return format!("{name}: null");
    }
    let values = values
        .iter()
        .map(|value| format!(r#""{}""#, escape_graphql_string(value)))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{name}: [{values}]")
}

pub(super) fn graphql_optional_int_list_field(name: &str, values: Option<&[i64]>) -> String {
    let Some(values) = values else {
        return format!("{name}: null");
    };
    if values.is_empty() {
        return format!("{name}: null");
    }
    let values = values
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    format!("{name}: [{values}]")
}

pub(super) fn graphql_optional_bool_field(name: &str, value: Option<bool>) -> String {
    match value {
        Some(value) => format!("{name}: {value}"),
        None => format!("{name}: null"),
    }
}

pub(super) fn graphql_optional_int_field(name: &str, value: Option<i64>) -> String {
    match value {
        Some(value) => format!("{name}: {value}"),
        None => format!("{name}: null"),
    }
}

pub(super) fn graphql_optional_float_field(name: &str, value: Option<f64>) -> String {
    match value {
        Some(value) => format!("{name}: {value}"),
        None => format!("{name}: null"),
    }
}

pub(super) fn normalize_required<'a>(field: &str, value: &'a str) -> Result<&'a str> {
    normalize_optional_string(Some(value)).with_context(|| format!("{field} must not be empty"))
}

pub(super) fn normalize_optional_string(value: Option<&str>) -> Option<&str> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then_some(trimmed)
    })
}

pub(super) use gents_protocol::graphql::escape_graphql_string;
