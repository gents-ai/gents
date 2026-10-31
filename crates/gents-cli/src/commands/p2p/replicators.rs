use anyhow::{Context, Result};
use gents::runtime::agent::p2p_reconcile::templates::{
    combine_filters, equality_filter, to_replication_filters, PairingFilters,
};
use serde_json::{json, Value};

use crate::cli::args::{P2pAccessArgs, P2pReplicatorAddArgs, P2pReplicatorRemoveArgs};
use crate::shared::{P2pReplicatorDeleteRequest, P2pReplicatorRequest, P2pReplicatorRow};
use crate::{http_send, print_json, resolve_graphql_endpoint, resolve_home_dir};

use super::collections::expand_p2p_collection_args;
use super::output::{
    fetch_live_http_p2p_status, flatten_p2p_fields, load_collection_name_by_id, p2p_replicator_rows,
};
use super::p2p_http_client;

pub(super) async fn p2p_replicators_list(args: P2pAccessArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let client = p2p_http_client()?;
    let api_base = crate::graphql_access::graphql_api_base(graphql.url())?;
    let raw_replicators: Vec<P2pReplicatorRow> =
        crate::http_get_json(graphql.authorize(client.get(format!("{api_base}/p2p/replicators")))?)
            .await?;
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
    let api_base = crate::graphql_access::graphql_api_base(graphql.url())?;
    let wire_filters = to_replication_filters(&filters).map_err(anyhow::Error::msg)?;
    let request = P2pReplicatorRequest {
        collections: collections.clone(),
        addresses: vec![args.peer.clone()],
        filters: wire_filters,
    };
    http_send(
        graphql.authorize(
            client
                .post(format!("{api_base}/p2p/replicators"))
                .json(&request),
        )?,
    )
    .await?;
    let p2p = fetch_live_http_p2p_status(args.home.as_deref(), &graphql).await?;
    let home_dir = resolve_home_dir(args.home.as_deref());
    let filters_json: serde_json::Map<String, Value> = request
        .filters
        .iter()
        .map(|(col, filter)| {
            (
                col.clone(),
                serde_json::to_value(filter).unwrap_or(Value::Null),
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
        let predicate = equality_filter(field, value);
        match map.remove(collection) {
            Some(existing) => {
                map.insert(collection.to_string(), combine_filters(existing, predicate));
            }
            None => {
                map.insert(collection.to_string(), predicate);
            }
        }
    }
    Ok(map)
}

pub(super) async fn p2p_replicators_remove(args: P2pReplicatorRemoveArgs) -> Result<()> {
    let collections = expand_p2p_collection_args(&args.collections, &args.profiles)?;
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let client = p2p_http_client()?;
    let api_base = crate::graphql_access::graphql_api_base(graphql.url())?;
    let request = P2pReplicatorDeleteRequest {
        id: args.peer.clone(),
        collections: collections.clone(),
    };
    http_send(
        graphql.authorize(
            client
                .delete(format!("{api_base}/p2p/replicators"))
                .json(&request),
        )?,
    )
    .await?;
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
