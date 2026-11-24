use anyhow::{bail, ensure, Context, Result};
use serde_json::{Map, Value};

use super::relation::{ranking, Observed, Ranking, Shape};
use super::schema::visible_fields;
use super::{build_paged_query, build_query, CollectionScope, DefraQueryParams, MAX_LIMIT};
use gents_store::config_client::schema_introspection::validate_identifier;
use gents_store::graphql::escape_graphql_string;

pub(super) const SCORE_FIELD: &str = "_score";

fn requested_ranking(options: &Map<String, Value>) -> Ranking {
    ranking(
        options.contains_key("text") || options.contains_key("search_fields"),
        options.contains_key("vector") || options.contains_key("vector_field"),
    )
}

pub(super) fn ranking_name(options: &Map<String, Value>) -> &'static str {
    match requested_ranking(options) {
        Ranking::Similarity => "similarity",
        Ranking::Bm25 | Ranking::Refused => "bm25",
    }
}

pub(super) fn build_search_query(
    params: &DefraQueryParams,
    scope: &CollectionScope,
    observed: &Observed,
    options: &Map<String, Value>,
) -> Result<String> {
    ensure!(
        !params.fields.iter().any(|field| field == SCORE_FIELD),
        "{SCORE_FIELD} is reserved for the native score; omit it from options.fields"
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
    let (query, score) = match requested_ranking(options) {
        Ranking::Bm25 => keyword_query(&params, scope, observed, options)?,
        Ranking::Similarity => vector_query(&params, scope, observed, options)?,
        Ranking::Refused => bail!(
            "search ranks by text with search_fields or by vector with vector_field; give exactly one. Call query with argv:[help,search]"
        ),
    };
    let selection = query
        .rfind(" } }")
        .context("query renderer omitted selection boundary")?;
    let mut query = query;
    query.insert_str(selection, &format!(" {SCORE_FIELD}: {score}"));
    Ok(query)
}

fn score_first(query: String) -> Result<String> {
    let order = query
        .rfind("order: [")
        .context("query renderer omitted order boundary")?;
    let mut query = query;
    query.insert_str(
        order + "order: [".len(),
        &format!("{{_alias: {{{SCORE_FIELD}: DESC}}}}, "),
    );
    Ok(query)
}

fn keyword_query(
    params: &DefraQueryParams,
    scope: &CollectionScope,
    observed: &Observed,
    options: &Map<String, Value>,
) -> Result<(String, String)> {
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
        ensure!(visible_fields(&observed.schema, &params.collection).iter()
            .any(|f| f.name == *field && f.named_type() == "String"),
            "search field {field:?} must be an available String field; call query with argv:[fields]");
    }
    let query = score_first(build_paged_query(params, scope, options.get("order"), 0)?)?;
    let fields = fields
        .iter()
        .map(|field| format!("\"{}\"", escape_graphql_string(field)))
        .collect::<Vec<_>>()
        .join(", ");
    Ok((
        query,
        format!(
            "BM25(query: \"{}\", fields: [{fields}])",
            escape_graphql_string(text)
        ),
    ))
}

/// Without an order of its own the score is the only order, the shape the
/// native planner answers from the field's vector index.
fn vector_query(
    params: &DefraQueryParams,
    scope: &CollectionScope,
    observed: &Observed,
    options: &Map<String, Value>,
) -> Result<(String, String)> {
    let field = options
        .get("vector_field")
        .and_then(Value::as_str)
        .context(
            "options.vector_field must name a numeric list field; call query with argv:[fields]",
        )?;
    validate_identifier(field)?;
    let visible = visible_fields(&observed.schema, &params.collection)
        .iter()
        .any(|f| f.name == field);
    ensure!(
        visible
            && matches!(
                observed.shape(field),
                Some(Shape::Value { numeric_list: true })
            ),
        "vector_field {field:?} must be a numeric list field such as [Float32!]; call query with argv:[fields]"
    );
    let vector = options
        .get("vector")
        .and_then(Value::as_array)
        .filter(|values| {
            !values.is_empty()
                && values
                    .iter()
                    .all(|value| value.as_f64().is_some_and(f64::is_finite))
        })
        .context("options.vector must be a nonempty array of numbers; call query with argv:[help,search]")?;
    let query = match options.get("order") {
        Some(order) => score_first(build_paged_query(params, scope, Some(order), 0)?)?,
        None => {
            let mut query = build_query(params, scope)?;
            let arguments = query
                .rfind(") {")
                .context("query renderer omitted argument boundary")?;
            query.insert_str(
                arguments,
                &format!(", order: [{{_alias: {{{SCORE_FIELD}: DESC}}}}]"),
            );
            query
        }
    };
    let vector = vector
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    Ok((
        query,
        format!("SIMILARITY({field}: {{vector: [{vector}]}})"),
    ))
}
