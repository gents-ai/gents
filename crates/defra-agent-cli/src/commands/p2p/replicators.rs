use anyhow::Result;
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
    let mut output = json!({
        "status": "replicator_added",
        "home": home_dir,
        "graphql": graphql,
        "peer": args.peer,
        "collections": collections,
        "p2p": p2p,
    });
    if let Some(map) = output.as_object_mut() {
        let p2p_value = map.get("p2p").cloned().unwrap_or(Value::Null);
        flatten_p2p_fields(map, &p2p_value);
    }
    print_json(&output)?;
    Ok(())
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
}
