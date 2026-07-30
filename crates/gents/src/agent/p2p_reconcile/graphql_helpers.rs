use anyhow::{bail, Context, Result};
use defra_node::QueryResponse;
use serde::Deserialize;

use crate::graphql::escape_graphql_string;

pub(super) fn ensure_no_errors(response: &QueryResponse, label: &str) -> Result<()> {
    if response.has_errors() {
        bail!("{label} failed: {:?}", response.errors);
    }
    Ok(())
}

pub(super) fn rows<T>(response: &QueryResponse, field: &str) -> Result<Vec<T>>
where
    T: for<'de> Deserialize<'de>,
{
    let Some(value) = response.data.as_ref().and_then(|data| data.get(field)) else {
        return Ok(Vec::new());
    };
    serde_json::from_value(value.clone()).with_context(|| format!("decode {field} rows"))
}

pub(super) fn first_row<T>(response: &QueryResponse, field: &str) -> Result<Option<T>>
where
    T: for<'de> Deserialize<'de>,
{
    Ok(rows::<T>(response, field)?.into_iter().next())
}

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
