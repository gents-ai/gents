use crate::graphql::escape_graphql_string;
pub(super) use crate::graphql::{ensure_no_errors, first_row, rows};

/// Render a GraphQL string-list literal, emitting `null` for an empty list
/// (never `[]`, which types as `JsonArray` and corrupts nillable array columns).
pub(super) fn graphql_string_list_literal<'a>(values: impl IntoIterator<Item = &'a str>) -> String {
    let items = values
        .into_iter()
        .map(|value| format!(r#""{}""#, escape_graphql_string(value)))
        .collect::<Vec<_>>();
    if items.is_empty() {
        "null".to_string()
    } else {
        format!("[{}]", items.join(", "))
    }
}

/// Render a nullable GraphQL string literal, emitting `null` for absent/blank.
pub(super) fn graphql_nullable_string_literal(value: Option<&str>) -> String {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| format!(r#""{}""#, escape_graphql_string(value)))
        .unwrap_or_else(|| "null".to_string())
}
