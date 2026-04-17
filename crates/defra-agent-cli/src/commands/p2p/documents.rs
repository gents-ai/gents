use anyhow::Result;
use serde_json::json;

use crate::cli::args::{P2pAccessArgs, P2pDocumentsMutateArgs, P2pDocumentsSyncArgs};
use crate::shared::P2pSyncDocumentsRequest;
use crate::{
    expand_nonempty_values, http_delete_json, http_post_json, print_json, resolve_graphql_endpoint,
    resolve_home_dir,
};

use super::{p2p_api_base, p2p_http_client};

pub(super) async fn p2p_documents_list(args: P2pAccessArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let client = p2p_http_client()?;
    let api_base = p2p_api_base(&graphql)?;
    let doc_ids: Vec<String> =
        crate::http_get_json(&client, &format!("{api_base}/p2p/documents")).await?;
    let count = doc_ids.len();
    let home_dir = resolve_home_dir(args.home.as_deref());
    print_json(&json!({
        "status": "ok",
        "home": home_dir,
        "graphql": graphql,
        "doc_ids": doc_ids,
        "count": count,
    }))?;
    Ok(())
}

pub(super) async fn p2p_documents_add(args: P2pDocumentsMutateArgs) -> Result<()> {
    let doc_ids = expand_nonempty_values(&args.doc_ids, "--doc-id")?;
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let client = p2p_http_client()?;
    let api_base = p2p_api_base(&graphql)?;
    http_post_json(&client, &format!("{api_base}/p2p/documents"), &doc_ids).await?;
    let home_dir = resolve_home_dir(args.home.as_deref());
    print_json(&json!({
        "status": "documents_added",
        "home": home_dir,
        "graphql": graphql,
        "doc_ids": doc_ids,
    }))?;
    Ok(())
}

pub(super) async fn p2p_documents_remove(args: P2pDocumentsMutateArgs) -> Result<()> {
    let doc_ids = expand_nonempty_values(&args.doc_ids, "--doc-id")?;
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let client = p2p_http_client()?;
    let api_base = p2p_api_base(&graphql)?;
    http_delete_json(&client, &format!("{api_base}/p2p/documents"), &doc_ids).await?;
    let home_dir = resolve_home_dir(args.home.as_deref());
    print_json(&json!({
        "status": "documents_removed",
        "home": home_dir,
        "graphql": graphql,
        "doc_ids": doc_ids,
    }))?;
    Ok(())
}

pub(super) async fn p2p_documents_sync(args: P2pDocumentsSyncArgs) -> Result<()> {
    let collection = args.collection.trim().to_string();
    if collection.is_empty() {
        anyhow::bail!("provide --collection");
    }
    let doc_ids = expand_nonempty_values(&args.doc_ids, "--doc-id")?;
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let client = p2p_http_client()?;
    let api_base = p2p_api_base(&graphql)?;
    let request = P2pSyncDocumentsRequest {
        collection_name: collection.clone(),
        doc_ids: doc_ids.clone(),
    };
    http_post_json(&client, &format!("{api_base}/p2p/documents/sync"), &request).await?;
    let home_dir = resolve_home_dir(args.home.as_deref());
    print_json(&json!({
        "status": "documents_sync_requested",
        "home": home_dir,
        "graphql": graphql,
        "collection": collection,
        "doc_ids": doc_ids,
    }))?;
    Ok(())
}
