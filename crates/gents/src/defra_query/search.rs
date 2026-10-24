use serde_json::{Map, Value};

use super::error::DefraQueryFailure;
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
) -> Result<String, DefraQueryFailure> {
    let text = options
        .get("text")
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .ok_or(DefraQueryFailure::SearchTextMissing)?;
    let fields: Vec<String> = serde_json::from_value(
        options
            .get("search_fields")
            .cloned()
            .ok_or(DefraQueryFailure::SearchFieldsMissing)?,
    )
    .map_err(DefraQueryFailure::SearchFieldsInvalid)?;
    let guard = DefraQueryParams {
        fields: fields.clone(),
        ..params.clone()
    };
    build_query(&guard, scope)?;
    let visible = schema.visible_fields(&params.collection);
    for field in &fields {
        if !visible
            .iter()
            .any(|f| f.name == *field && f.named_type() == "String")
        {
            return Err(DefraQueryFailure::SearchFieldUnavailable {
                field: field.clone(),
            });
        }
    }
    if params.fields.iter().any(|field| field == SCORE_FIELD) {
        return Err(DefraQueryFailure::ScoreFieldReserved);
    }
    let limit = params.limit.unwrap_or(10);
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(DefraQueryFailure::SearchLimit);
    }
    let params = DefraQueryParams {
        limit: Some(limit),
        ..params.clone()
    };
    let mut query = build_paged_query(&params, scope, options.get("order"), 0)?;
    let order = query
        .rfind("order: [")
        .ok_or(DefraQueryFailure::RendererBoundaryMissing { boundary: "order" })?;
    query.insert_str(
        order + "order: [".len(),
        &format!("{{_alias: {{{SCORE_FIELD}: DESC}}}}, "),
    );
    let selection = query
        .rfind(" } }")
        .ok_or(DefraQueryFailure::RendererBoundaryMissing {
            boundary: "selection",
        })?;
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
