use super::error::{P2pCommandError, Result};
use serde_json::json;

use crate::cli::args::{P2pAccessArgs, P2pDocumentsMutateArgs, P2pDocumentsSyncArgs};
use crate::shared::P2pSyncDocumentsRequest;
use crate::{
    expand_nonempty_values, http_send, print_json, resolve_graphql_endpoint, resolve_home_dir,
};

use super::p2p_http_client;

pub(super) async fn p2p_documents_list(args: P2pAccessArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let client = p2p_http_client()?;
    let api_base = crate::graphql_access::graphql_api_base(graphql.url())?;
    let doc_ids: Vec<String> =
        crate::http_get_json(graphql.authorize(client.get(format!("{api_base}/p2p/documents")))?)
            .await?;
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
    let api_base = crate::graphql_access::graphql_api_base(graphql.url())?;
    http_send(
        graphql.authorize(
            client
                .post(format!("{api_base}/p2p/documents"))
                .json(&doc_ids),
        )?,
    )
    .await?;
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
    let api_base = crate::graphql_access::graphql_api_base(graphql.url())?;
    http_send(
        graphql.authorize(
            client
                .delete(format!("{api_base}/p2p/documents"))
                .json(&doc_ids),
        )?,
    )
    .await?;
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
        return Err(P2pCommandError::MissingCollection);
    }
    let doc_ids = expand_nonempty_values(&args.doc_ids, "--doc-id")?;
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let client = p2p_http_client()?;
    let api_base = crate::graphql_access::graphql_api_base(graphql.url())?;
    let request = P2pSyncDocumentsRequest {
        collection_name: collection.clone(),
        doc_ids: doc_ids.clone(),
    };
    http_send(
        graphql.authorize(
            client
                .post(format!("{api_base}/p2p/documents/sync"))
                .json(&request),
        )?,
    )
    .await?;
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
