use crate::store::graphql::escape_graphql_string;
pub(super) use crate::store::graphql::{first_row, graphql_string_list_literal, rows};

pub(super) fn graphql_nullable_string_literal(value: Option<&str>) -> String {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| format!(r#""{}""#, escape_graphql_string(value)))
        .unwrap_or_else(|| "null".to_string())
}
