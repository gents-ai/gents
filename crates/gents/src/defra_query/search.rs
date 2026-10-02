use anyhow::{ensure, Context, Result};
use serde_json::{Map, Value};

use super::{build_query, CollectionSchema, CollectionScope, DefraQueryParams, MAX_LIMIT};

pub(super) const SCORE_FIELD: &str = "_similarity";

pub(super) fn build_search_query(
    params: &DefraQueryParams,
    scope: &CollectionScope,
    schema: &CollectionSchema,
    options: &Map<String, Value>,
) -> Result<String> {
    let field = options
        .get("vector_field")
        .and_then(Value::as_str)
        .context(
            "options.vector_field is required; call query with argv:[fields] to discover names",
        )?;
    let guard = DefraQueryParams {
        fields: vec![field.into()],
        ..params.clone()
    };
    build_query(&guard, scope)?;
    ensure!(
        schema.visible_fields(&params.collection).iter().any(|f| f.name == field),
        "vector_field {field:?} is not an available application field; call query with argv:[fields]"
    );
    ensure!(
        !params.fields.iter().any(|f| f == SCORE_FIELD),
        "{SCORE_FIELD} is reserved for the native similarity score; omit it from options.fields"
    );
    let vector: Vec<f64> = serde_json::from_value(
        options.get("vector").cloned().context(
            "options.vector is required; supply numeric embeddings from the same model as the stored field; call query with argv:[help,search]",
        )?,
    )
    .context("options.vector must be an array of finite numbers; call query with argv:[help,search]")?;
    ensure!(
        !vector.is_empty() && vector.iter().all(|n| n.is_finite()),
        "options.vector must be a nonempty array of finite numbers; call query with argv:[help,search]"
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
    let mut query = build_query(&params, scope)?;
    let arguments = query
        .find(") {")
        .context("query renderer omitted argument boundary")?;
    query.insert_str(
        arguments,
        &format!(", order: {{_alias: {{{SCORE_FIELD}: DESC}}}}"),
    );
    let selection = query
        .rfind(" } }")
        .context("query renderer omitted selection boundary")?;
    query.insert_str(
        selection,
        &format!(
            " {SCORE_FIELD}: SIMILARITY({field}: {{vector: {}}})",
            serde_json::to_string(&vector)?
        ),
    );
    Ok(query)
}
