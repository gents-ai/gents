use anyhow::Result;
use gents::agent::p2p_reconcile::{expand_p2p_collection_profile_ids, P2pCollectionProfile};
use serde_json::{json, Value};

use crate::cli::args::{
    P2pAccessArgs, P2pCollectionProfileArg, P2pCollectionsMutateArgs, P2pSyncBranchableArgs,
    P2pSyncVersionsArgs,
};
use crate::shared::{P2pSyncBranchableRequest, P2pSyncVersionsRequest};
use crate::{
    expand_nonempty_values, http_delete_json, http_post_json, print_json, resolve_graphql_endpoint,
    resolve_home_dir,
};

use super::output::{
    fetch_live_http_p2p_status, flatten_p2p_fields, load_collection_name_by_id,
    p2p_collection_names, p2p_collection_rows,
};
use super::p2p_http_client;

pub(super) fn expand_p2p_collection_args(
    explicit_collections: &[String],
    profiles: &[P2pCollectionProfileArg],
) -> Result<Vec<String>> {
    let profile_ids = profiles
        .iter()
        .map(|profile| p2p_collection_profile_id(*profile));
    let collections = expand_p2p_collection_profile_ids(
        explicit_collections.iter().map(String::as_str),
        profile_ids,
    )?;
    Ok(collections.into_iter().collect())
}

pub(super) fn p2p_collection_profile_id(profile: P2pCollectionProfileArg) -> &'static str {
    match profile {
        P2pCollectionProfileArg::Runtime => P2pCollectionProfile::Runtime.id(),
        P2pCollectionProfileArg::Agent => P2pCollectionProfile::Agent.id(),
        P2pCollectionProfileArg::DesktopConfig => P2pCollectionProfile::DesktopConfig.id(),
        P2pCollectionProfileArg::ChatRequests => P2pCollectionProfile::ChatRequests.id(),
        P2pCollectionProfileArg::ToolServices => P2pCollectionProfile::ToolServices.id(),
        P2pCollectionProfileArg::Discovery => P2pCollectionProfile::Discovery.id(),
    }
}

pub(super) async fn p2p_collections_list(args: P2pAccessArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let client = p2p_http_client()?;
    let api_base = crate::graphql_access::graphql_api_base(&graphql)?;
    let collection_ids: Vec<String> =
        crate::http_get_json(&client, &format!("{api_base}/p2p/collections")).await?;
    let collection_names_by_id = load_collection_name_by_id(&client, &api_base).await;
    let collections = p2p_collection_rows(&collection_ids, &collection_names_by_id);
    let collection_names = p2p_collection_names(&collection_ids, &collection_names_by_id);
    let count = collections.len();
    let home_dir = resolve_home_dir(args.home.as_deref());
    print_json(&json!({
        "status": "ok",
        "home": home_dir,
        "graphql": graphql,
        "collections": collections,
        "collection_ids": collection_ids,
        "collection_names": collection_names,
        "count": count,
    }))?;
    Ok(())
}

pub(super) async fn p2p_collections_add(args: P2pCollectionsMutateArgs) -> Result<()> {
    let collections = expand_p2p_collection_args(&args.collections, &args.profiles)?;
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let client = p2p_http_client()?;
    let api_base = crate::graphql_access::graphql_api_base(&graphql)?;
    http_post_json(
        &client,
        &format!("{api_base}/p2p/collections"),
        &collections,
    )
    .await?;
    let p2p = fetch_live_http_p2p_status(args.home.as_deref(), &graphql).await?;
    let home_dir = resolve_home_dir(args.home.as_deref());
    let mut output = json!({
        "status": "collections_added",
        "home": home_dir,
        "graphql": graphql,
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

pub(super) async fn p2p_collections_remove(args: P2pCollectionsMutateArgs) -> Result<()> {
    let collections = expand_p2p_collection_args(&args.collections, &args.profiles)?;
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let client = p2p_http_client()?;
    let api_base = crate::graphql_access::graphql_api_base(&graphql)?;
    http_delete_json(
        &client,
        &format!("{api_base}/p2p/collections"),
        &collections,
    )
    .await?;
    let home_dir = resolve_home_dir(args.home.as_deref());
    print_json(&json!({
        "status": "collections_removed",
        "home": home_dir,
        "graphql": graphql,
        "collections": collections,
    }))?;
    Ok(())
}

pub(super) async fn p2p_collections_sync_branchable(args: P2pSyncBranchableArgs) -> Result<()> {
    let collection_id = args.collection_id.trim().to_string();
    if collection_id.is_empty() {
        anyhow::bail!("provide --collection-id");
    }
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let client = p2p_http_client()?;
    let api_base = crate::graphql_access::graphql_api_base(&graphql)?;
    let request = P2pSyncBranchableRequest {
        collection_id: collection_id.clone(),
    };
    http_post_json(
        &client,
        &format!("{api_base}/p2p/collections/sync-branchable"),
        &request,
    )
    .await?;
    let home_dir = resolve_home_dir(args.home.as_deref());
    print_json(&json!({
        "status": "collection_sync_requested",
        "home": home_dir,
        "graphql": graphql,
        "collection_id": collection_id,
    }))?;
    Ok(())
}

pub(super) async fn p2p_collections_sync_versions(args: P2pSyncVersionsArgs) -> Result<()> {
    let version_ids = expand_nonempty_values(&args.version_ids, "--version-id")?;
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let client = p2p_http_client()?;
    let api_base = crate::graphql_access::graphql_api_base(&graphql)?;
    let request = P2pSyncVersionsRequest {
        version_ids: version_ids.clone(),
    };
    http_post_json(
        &client,
        &format!("{api_base}/p2p/collections/sync-versions"),
        &request,
    )
    .await?;
    let home_dir = resolve_home_dir(args.home.as_deref());
    print_json(&json!({
        "status": "collection_versions_sync_requested",
        "home": home_dir,
        "graphql": graphql,
        "version_ids": version_ids,
    }))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn p2p_collection_profiles_expand_and_dedupe_collection_names() {
        let collections = expand_p2p_collection_args(
            &[
                " AgentRequest ".to_string(),
                "AgentRequest".to_string(),
                "".to_string(),
            ],
            &[
                P2pCollectionProfileArg::ChatRequests,
                P2pCollectionProfileArg::ToolServices,
            ],
        )
        .unwrap();

        assert!(collections.iter().any(|name| name == "AgentRequest"));
        assert!(collections.iter().any(|name| name == "AgentResponse"));
        assert!(collections.iter().any(|name| name == "ToolServiceRegistry"));
        assert_eq!(
            collections
                .iter()
                .filter(|name| name.as_str() == "AgentRequest")
                .count(),
            1
        );
    }

    #[test]
    fn p2p_collection_args_require_collection_or_profile() {
        let error = expand_p2p_collection_args(&[], &[]).unwrap_err();
        assert!(error
            .to_string()
            .contains("provide at least one --collection or --profile"));
    }

    #[test]
    fn p2p_collection_rows_include_human_readable_names_when_known() {
        let mut names_by_id = BTreeMap::new();
        names_by_id.insert("bafk-agent-request".to_string(), "AgentRequest".to_string());
        let rows = p2p_collection_rows(
            &["bafk-agent-request".to_string(), "bafk-unknown".to_string()],
            &names_by_id,
        );

        assert_eq!(rows[0].id, "bafk-agent-request");
        assert_eq!(rows[0].name.as_deref(), Some("AgentRequest"));
        assert_eq!(rows[1].id, "bafk-unknown");
        assert!(rows[1].name.is_none());
    }
}
