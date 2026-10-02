use anyhow::{ensure, Context, Result};
use serde_json::{Map, Value};

use super::{
    build_paged_query, build_query, CollectionSchema, CollectionScope, DefraQueryParams, MAX_LIMIT,
};
use crate::graphql::escape_graphql_string;

pub(super) const SCORE_FIELD: &str = "_score";

pub(super) fn build_search_query(
    params: &DefraQueryParams,
    scope: &CollectionScope,
    schema: &CollectionSchema,
    options: &Map<String, Value>,
) -> Result<String> {
    let text = options
        .get("text")
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .context(
            "options.text must be nonempty search terms; call query with argv:[help,search]",
        )?;
    let fields: Vec<String> = serde_json::from_value(
        options.get("search_fields").cloned().context(
            "options.search_fields is required; call query with argv:[fields] to discover names",
        )?,
    )
    .context(
        "options.search_fields must be an array of field names; call query with argv:[help,search]",
    )?;
    let guard = DefraQueryParams {
        fields: fields.clone(),
        ..params.clone()
    };
    build_query(&guard, scope)?;
    for field in &fields {
        ensure!(schema.visible_fields(&params.collection).iter()
            .any(|f| f.name == *field && f.named_type() == "String"),
            "search field {field:?} must be an available String field; call query with argv:[fields]");
    }
    ensure!(
        !params.fields.iter().any(|field| field == SCORE_FIELD),
        "{SCORE_FIELD} is reserved for the native BM25 score; omit it from options.fields"
    );
    let limit = params.limit.unwrap_or(10);
    ensure!(
        (1..=MAX_LIMIT).contains(&limit),
        "search limit must be 1–{MAX_LIMIT}; call query with argv:[help,search]"
    );
    let params = DefraQueryParams {
        limit: Some(limit),
        ..params.clone()
    };
    let mut query = build_paged_query(&params, scope, options.get("order"), 0)?;
    let order = query
        .rfind("order: [")
        .context("query renderer omitted order boundary")?;
    query.insert_str(
        order + "order: [".len(),
        &format!("{{_alias: {{{SCORE_FIELD}: DESC}}}}, "),
    );
    let selection = query
        .rfind(" } }")
        .context("query renderer omitted selection boundary")?;
    let fields = fields
        .iter()
        .map(|field| format!("\"{}\"", escape_graphql_string(field)))
        .collect::<Vec<_>>()
        .join(", ");
    query.insert_str(
        selection,
        &format!(
            " {SCORE_FIELD}: BM25(query: \"{}\", fields: [{fields}])",
            escape_graphql_string(text)
        ),
    );
    Ok(query)
}
