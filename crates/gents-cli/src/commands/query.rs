use anyhow::{Context, Result};
use gents::defra_query::{
    build_query, diagnose_failed_query, discovery_payload, introspection_query,
    parse_collection_schema, unknown_collection_message, CollectionSchema, CollectionScope,
    DefraQueryParams,
};
use serde_json::{json, Value};

use crate::cli::args::QueryArgs;
use crate::{post_graphql, print_json, resolve_graphql_endpoint};

/// Introspect a collection's field set over GraphQL-over-HTTP. `Ok(None)`
/// means the collection (GraphQL type) does not exist on the node.
async fn fetch_collection_schema(
    graphql: &str,
    collection: &str,
) -> Result<Option<CollectionSchema>> {
    let query = introspection_query(collection)?;
    let response = post_graphql(graphql, &query).await?;
    if let Some(errors) = response
        .get("errors")
        .and_then(Value::as_array)
        .filter(|errors| !errors.is_empty())
    {
        anyhow::bail!("schema introspection for {collection:?} failed: {errors:?}");
    }
    Ok(parse_collection_schema(response.get("data")))
}

async fn enriched_query_failure(
    graphql: &str,
    params: &DefraQueryParams,
    raw: String,
) -> anyhow::Error {
    let diagnostic = match fetch_collection_schema(graphql, &params.collection).await {
        Ok(schema) => diagnose_failed_query(params, schema.as_ref(), &raw),
        Err(_) => raw,
    };
    anyhow::anyhow!(
        "defra_query against {:?} failed: {diagnostic}",
        params.collection
    )
}

pub(crate) async fn run_defra_query(
    graphql: &str,
    params: &DefraQueryParams,
    scope: &CollectionScope,
) -> Result<Value> {
    if params.is_discovery() {
        scope.ensure_allowed(&params.collection)?;
        let schema = fetch_collection_schema(graphql, &params.collection)
            .await?
            .with_context(|| {
                format!(
                    "defra_query against {:?} failed: {}",
                    params.collection,
                    unknown_collection_message(&params.collection)
                )
            })?;
        return Ok(discovery_payload(&params.collection, &schema));
    }

    let query = build_query(params, scope)?;
    // `post_graphql` bails when the response carries GraphQL errors, so the
    // enrichment hook is its Err path (transport failures fall back to the
    // original error because introspection then fails too).
    let response = match post_graphql(graphql, &query).await {
        Ok(response) => response,
        Err(error) => {
            return Err(enriched_query_failure(graphql, params, format!("{error:#}")).await);
        }
    };
    if let Some(errors) = response
        .get("errors")
        .and_then(Value::as_array)
        .filter(|errors| !errors.is_empty())
    {
        return Err(enriched_query_failure(graphql, params, format!("{errors:?}")).await);
    }
    let rows = response
        .get("data")
        .and_then(|data| data.get(&params.collection))
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()));
    let count = rows.as_array().map(Vec::len).unwrap_or(0);
    Ok(json!({
        "collection": params.collection,
        "count": count,
        "results": rows,
    }))
}

pub(crate) fn params_from_args(args: &QueryArgs) -> Result<(DefraQueryParams, CollectionScope)> {
    let filter = match args
        .filter
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(raw) => Some(serde_json::from_str::<Value>(raw).context("parsing --filter as JSON")?),
        None => None,
    };
    let params = DefraQueryParams {
        collection: args.collection.clone(),
        filter,
        fields: args.fields.clone(),
        limit: args.limit,
    };
    let scope = if args.allow_collections.is_empty() {
        CollectionScope::all()
    } else {
        CollectionScope::restricted(args.allow_collections.clone())
    };
    Ok((params, scope))
}

pub(crate) async fn query(args: QueryArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let (params, scope) = params_from_args(&args)?;
    let output = run_defra_query(&graphql, &params, &scope).await?;
    print_json(&output)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use gents_protocol::schemas::EVAL_VERDICT_NAME;

    use super::*;

    /// `gents query` with no `--allow-collection` is the widest scope the CLI
    /// offers, and it is the one an operator reaches for by default. A protected
    /// collection must still be refused there, before any request leaves the
    /// process — the endpoint below is never listening, so a refusal that named a
    /// transport failure would not name the collection.
    fn protected_args(fields: Vec<String>) -> QueryArgs {
        QueryArgs {
            home: None,
            graphql: Some("http://127.0.0.1:1/api/v0/graphql".into()),
            collection: EVAL_VERDICT_NAME.into(),
            fields,
            filter: None,
            limit: None,
            allow_collections: Vec::new(),
        }
    }

    #[tokio::test]
    async fn refuses_a_protected_collection_under_the_default_scope() {
        let args = protected_args(vec!["verdict_id".into()]);
        let (params, scope) = params_from_args(&args).expect("args parse");
        assert!(scope.is_unrestricted(), "no --allow-collection means all");

        let error = run_defra_query("http://127.0.0.1:1/api/v0/graphql", &params, &scope)
            .await
            .expect_err("EvalVerdict must never be readable through `gents query`");
        let message = format!("{error:#}");
        assert!(message.contains(EVAL_VERDICT_NAME), "{message}");
        assert!(message.contains("protected"), "{message}");
    }

    #[tokio::test]
    async fn refuses_discovery_of_a_protected_collection() {
        let args = protected_args(vec!["*".into()]);
        let (params, scope) = params_from_args(&args).expect("args parse");
        assert!(params.is_discovery(), "a lone `*` is the discovery request");

        let error = run_defra_query("http://127.0.0.1:1/api/v0/graphql", &params, &scope)
            .await
            .expect_err("a protected collection's field inventory must stay unlisted");
        let message = format!("{error:#}");
        assert!(message.contains(EVAL_VERDICT_NAME), "{message}");
        assert!(message.contains("protected"), "{message}");
    }
}
