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
    let values = values
        .iter()
        .map(|value| format!(r#""{}""#, escape_graphql_string(value)))
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

pub(super) fn escape_graphql_string(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}
