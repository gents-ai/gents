//! `defra-agent query` — a read-only structured query against a DefraDB
//! collection, reusing the agent's `defra_query` core (filter rendering, the
//! always-on secret-field guard, and collection scoping). Talks to a running
//! node over GraphQL-over-HTTP, so it works while `defra-agent server` is up.
//!
//! The shared [`run_defra_query`] helper also backs the MCP `defra_query` tool,
//! so the CLI and MCP surfaces are guaranteed to behave identically.

use anyhow::{Context, Result};
use defra_agent::defra_query::{build_query, CollectionScope, DefraQueryParams};
use serde_json::{json, Value};

use crate::cli::args::QueryArgs;
use crate::{post_graphql, print_json, resolve_graphql_endpoint};

/// Build + execute a structured query over GraphQL-over-HTTP and return the
/// `{collection, count, results}` envelope. Shared by the CLI command and the
/// MCP tool so both honor the same secret guard + scope.
pub(crate) async fn run_defra_query(
    graphql: &str,
    params: &DefraQueryParams,
    scope: &CollectionScope,
) -> Result<Value> {
    let query = build_query(params, scope)?;
    let response = post_graphql(graphql, &query).await?;
    if let Some(errors) = response
        .get("errors")
        .and_then(Value::as_array)
        .filter(|errors| !errors.is_empty())
    {
        anyhow::bail!(
            "defra_query against {:?} failed: {errors:?}",
            params.collection
        );
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
