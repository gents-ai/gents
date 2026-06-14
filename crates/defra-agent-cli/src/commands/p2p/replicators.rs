use anyhow::{Context, Result};
use defra_agent::agent::p2p_reconcile::templates::{FilterPredicate, PairingFilters};
use serde_json::{json, Value};

use crate::cli::args::{P2pAccessArgs, P2pReplicatorAddArgs, P2pReplicatorRemoveArgs};
use crate::shared::{P2pReplicatorDeleteRequest, P2pReplicatorRequest, P2pReplicatorRow};
use crate::{
    http_delete_json, http_post_json, print_json, resolve_graphql_endpoint, resolve_home_dir,
};

use super::collections::expand_p2p_collection_args;
use super::output::{
    fetch_live_http_p2p_status, flatten_p2p_fields, load_collection_name_by_id, p2p_replicator_rows,
};
use super::p2p_http_client;

pub(super) async fn p2p_replicators_list(args: P2pAccessArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let client = p2p_http_client()?;
    let api_base = crate::graphql_access::graphql_api_base(&graphql)?;
    let raw_replicators: Vec<P2pReplicatorRow> =
        crate::http_get_json(&client, &format!("{api_base}/p2p/replicators")).await?;
    let collection_names_by_id = load_collection_name_by_id(&client, &api_base).await;
    let replicators = p2p_replicator_rows(raw_replicators, &collection_names_by_id);
    let count = replicators.len();
    let home_dir = resolve_home_dir(args.home.as_deref());
    print_json(&json!({
        "status": "ok",
        "home": home_dir,
        "graphql": graphql,
        "replicators": replicators,
        "count": count,
    }))?;
    Ok(())
}

pub(super) async fn p2p_replicators_add(args: P2pReplicatorAddArgs) -> Result<()> {
    let collections = expand_p2p_collection_args(&args.collections, &args.profiles)?;
    let filters = parse_replicator_filters(&args.filters)
        .context("parsing --filter arguments for p2p admin replicators add")?;
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let client = p2p_http_client()?;
    let api_base = crate::graphql_access::graphql_api_base(&graphql)?;
    let request = P2pReplicatorRequest {
        collections: collections.clone(),
        addresses: vec![args.peer.clone()],
    };
    http_post_json(&client, &format!("{api_base}/p2p/replicators"), &request).await?;
    let p2p = fetch_live_http_p2p_status(args.home.as_deref(), &graphql).await?;
    let home_dir = resolve_home_dir(args.home.as_deref());
    // Serialise the parsed PairingFilters for output so callers can verify the
    // parse result. The filters are not yet forwarded to the HTTP endpoint
    // (pending defradb.rs #1033 filter API); they will be wired through
    // `add_replicator(.., &filters)` once the upstream pin includes that surface.
    let filters_json: serde_json::Map<String, Value> = filters
        .iter()
        .map(|(col, pred)| {
            (
                col.clone(),
                json!({ "field": pred.field, "value": pred.value }),
            )
        })
        .collect();
    let mut output = json!({
        "status": "replicator_added",
        "home": home_dir,
        "graphql": graphql,
        "peer": args.peer,
        "collections": collections,
        "filters": filters_json,
        "p2p": p2p,
    });
    if let Some(map) = output.as_object_mut() {
        let p2p_value = map.get("p2p").cloned().unwrap_or(Value::Null);
        flatten_p2p_fields(map, &p2p_value);
    }
    print_json(&output)?;
    Ok(())
}

/// Parse `--filter <collection>:<field>=<value>` entries into a `PairingFilters` map.
///
/// Each entry must have the form `Collection:field=value`. A missing `:` or `=`
/// separator, or an empty collection/field/value is a hard parse error.
pub(super) fn parse_replicator_filters(filters: &[String]) -> Result<PairingFilters> {
    let mut map = PairingFilters::new();
    for raw in filters {
        let (collection, rest) = raw.split_once(':').with_context(|| {
            format!(
                "invalid --filter {raw:?}: expected format \
                 `<collection>:<field>=<value>` (missing ':')"
            )
        })?;
        let (field, value) = rest.split_once('=').with_context(|| {
            format!(
                "invalid --filter {raw:?}: expected format \
                 `<collection>:<field>=<value>` (missing '=' after field name)"
            )
        })?;
        let collection = collection.trim();
        let field = field.trim();
        let value = value.trim();
        if collection.is_empty() {
            anyhow::bail!("invalid --filter {raw:?}: collection name must not be empty");
        }
        if field.is_empty() {
            anyhow::bail!("invalid --filter {raw:?}: field name must not be empty");
        }
        if value.is_empty() {
            anyhow::bail!("invalid --filter {raw:?}: filter value must not be empty");
        }
        map.insert(
            collection.to_string(),
            FilterPredicate {
                field: field.to_string(),
                value: value.to_string(),
            },
        );
    }
    Ok(map)
}

pub(super) async fn p2p_replicators_remove(args: P2pReplicatorRemoveArgs) -> Result<()> {
    let collections = expand_p2p_collection_args(&args.collections, &args.profiles)?;
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let client = p2p_http_client()?;
    let api_base = crate::graphql_access::graphql_api_base(&graphql)?;
    let request = P2pReplicatorDeleteRequest {
        id: args.peer.clone(),
        collections: collections.clone(),
    };
    http_delete_json(&client, &format!("{api_base}/p2p/replicators"), &request).await?;
    let home_dir = resolve_home_dir(args.home.as_deref());
    print_json(&json!({
        "status": "replicator_removed",
        "home": home_dir,
        "graphql": graphql,
        "peer": args.peer,
        "collections": collections,
    }))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn p2p_replicator_rows_resolve_collection_names() {
        let mut names_by_id = BTreeMap::new();
        names_by_id.insert("bafk-agent-runtime".to_string(), "AgentRuntime".to_string());
        let rows = p2p_replicator_rows(
            vec![P2pReplicatorRow {
                id: Some("peer-1".to_string()),
                addresses: vec!["iroh://peer-1".to_string()],
                collection_ids: vec!["bafk-agent-runtime".to_string(), "bafk-missing".to_string()],
            }],
            &names_by_id,
        );

        assert_eq!(rows[0].id.as_deref(), Some("peer-1"));
        assert_eq!(rows[0].collection_names, vec!["AgentRuntime"]);
        assert_eq!(rows[0].collection_ids.len(), 2);
    }

    #[test]
    fn parse_replicator_filters_valid_entries() {
        let filters = parse_replicator_filters(&[
            "AgentRequest:agent_did=did:key:alice".to_string(),
            "AgentResponse:agent_did=did:key:bob".to_string(),
        ])
        .expect("valid filters should parse");

        assert_eq!(filters.len(), 2);
        let req = filters.get("AgentRequest").unwrap();
        assert_eq!(req.field, "agent_did");
        assert_eq!(req.value, "did:key:alice");
        let resp = filters.get("AgentResponse").unwrap();
        assert_eq!(resp.field, "agent_did");
        assert_eq!(resp.value, "did:key:bob");
    }

    #[test]
    fn parse_replicator_filters_empty_is_ok() {
        let filters = parse_replicator_filters(&[]).expect("empty filters should parse");
        assert!(filters.is_empty());
    }

    #[test]
    fn parse_replicator_filters_rejects_missing_colon() {
        let err = parse_replicator_filters(&["AgentRequestagent_did=value".to_string()])
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("missing ':'"),
            "expected 'missing :' in error, got: {err}"
        );
    }

    #[test]
    fn parse_replicator_filters_rejects_missing_equals() {
        let err = parse_replicator_filters(&["AgentRequest:agent_did_no_value".to_string()])
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("missing '='"),
            "expected \"missing '='\" in error, got: {err}"
        );
    }

    #[test]
    fn parse_replicator_filters_rejects_empty_collection() {
        let err = parse_replicator_filters(&[":field=value".to_string()])
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("collection name must not be empty"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn parse_replicator_filters_rejects_empty_field() {
        let err = parse_replicator_filters(&["AgentRequest:=value".to_string()])
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("field name must not be empty"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn parse_replicator_filters_rejects_empty_value() {
        let err = parse_replicator_filters(&["AgentRequest:agent_did=".to_string()])
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("filter value must not be empty"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn parse_replicator_filters_value_may_contain_equals() {
        // DID values like "did:key:z=abc" should work; only the first '=' is split on.
        let filters =
            parse_replicator_filters(&["AgentRequest:agent_did=did:key:z=abc".to_string()])
                .expect("filter value with = should parse");
        let pred = filters.get("AgentRequest").unwrap();
        assert_eq!(pred.field, "agent_did");
        assert_eq!(pred.value, "did:key:z=abc");
    }
}
