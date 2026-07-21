//! `gents query` — a read-only structured query against a DefraDB
//! collection, reusing the agent's `defra_query` core (filter rendering, the
//! always-on secret-field guard, and collection scoping). Talks to a running
//! node over GraphQL-over-HTTP, so it works while `gents server` is up.
//!
//! The shared [`run_defra_query`] helper also backs the MCP `defra_query` tool,
//! so the CLI and MCP surfaces are guaranteed to behave identically.

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

/// Turn a failed query into an agent-usable diagnostic by introspecting the
/// collection; when introspection itself fails (e.g. the failure was
/// transport-level), the original error text is preserved unchanged.
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

/// Build + execute a structured query over GraphQL-over-HTTP and return the
/// `{collection, count, results}` envelope. Shared by the CLI command and the
/// MCP tool so both honor the same secret guard + scope.
///
/// `fields: ["*"]` is discovery mode and returns the collection's queryable
/// field inventory instead of documents. On a GraphQL failure the collection
/// is introspected and the error enriched into a field-level diagnostic; if
/// introspection itself fails, the raw errors are surfaced unchanged.
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

/// Parse the `{collection, filter, fields, limit, allow_collections}` CLI args
/// into the structured contract.
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
