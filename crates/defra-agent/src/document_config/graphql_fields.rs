use crate::graphql::escape_graphql_string;

use super::serde_helpers::normalize_optional_string;

pub(super) fn graphql_string_field(name: &str, value: Option<&str>) -> Option<String> {
    Some(format!(
        r#"{name}: "{}""#,
        escape_graphql_string(normalize_optional_string(value).unwrap_or_default())
    ))
}

pub(super) fn graphql_nullable_string_field(name: &str, value: Option<&str>) -> String {
    match normalize_optional_string(value) {
        Some(value) => format!(r#"{name}: "{}""#, escape_graphql_string(value)),
        None => format!("{name}: null"),
    }
}

pub(super) fn graphql_optional_int_field(name: &str, value: Option<i64>) -> Option<String> {
    value.map(|value| format!("{name}: {value}"))
}

pub(super) fn graphql_optional_float_field(name: &str, value: Option<f64>) -> Option<String> {
    value.map(|value| format!("{name}: {value}"))
}

pub(super) fn graphql_optional_bool_field(name: &str, value: Option<bool>) -> Option<String> {
    value.map(|value| format!("{name}: {}", graphql_bool(value)))
}

pub(super) fn graphql_string_list_field(name: &str, value: Option<&[String]>) -> Option<String> {
    let values = value?;
    Some(format!(
        "{name}: [{}]",
        values
            .iter()
            .map(|value| format!(r#""{}""#, escape_graphql_string(value)))
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

pub(super) fn graphql_bool(value: bool) -> &'static str {
    if value {
        "true"
    } else {
        "false"
    }
}
