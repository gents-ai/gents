mod access;
mod collections;
mod connect;
mod documents;
mod output;
mod pair;
mod replicators;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::cli::args::{
    P2pCollectionsCommand, P2pCommand, P2pDocumentsCommand, P2pReplicatorsCommand,
};

pub(crate) use output::{flatten_p2p_fields, load_live_http_p2p_status, persisted_p2p_status};

pub(crate) async fn dispatch(command: P2pCommand) -> Result<()> {
    match command {
        P2pCommand::Status(args) => access::p2p_status(args).await,
        P2pCommand::Peers(args) => access::p2p_peers(args).await,
        P2pCommand::Connect(args) => connect::p2p_connect(args).await,
        P2pCommand::Collections { command } => match command {
            P2pCollectionsCommand::List(args) => collections::p2p_collections_list(args).await,
            P2pCollectionsCommand::Add(args) => collections::p2p_collections_add(args).await,
            P2pCollectionsCommand::Remove(args) => collections::p2p_collections_remove(args).await,
            P2pCollectionsCommand::SyncBranchable(args) => {
                collections::p2p_collections_sync_branchable(args).await
            }
            P2pCollectionsCommand::SyncVersions(args) => {
                collections::p2p_collections_sync_versions(args).await
            }
        },
        P2pCommand::Replicators { command } => match command {
            P2pReplicatorsCommand::List(args) => replicators::p2p_replicators_list(args).await,
            P2pReplicatorsCommand::Add(args) => replicators::p2p_replicators_add(args).await,
            P2pReplicatorsCommand::Remove(args) => replicators::p2p_replicators_remove(args).await,
        },
        P2pCommand::Documents { command } => match command {
            P2pDocumentsCommand::List(args) => documents::p2p_documents_list(args).await,
            P2pDocumentsCommand::Add(args) => documents::p2p_documents_add(args).await,
            P2pDocumentsCommand::Remove(args) => documents::p2p_documents_remove(args).await,
            P2pDocumentsCommand::Sync(args) => documents::p2p_documents_sync(args).await,
        },
        P2pCommand::Diagnose(args) => connect::p2p_diagnose(args).await,
        P2pCommand::Pair(args) => pair::p2p_pair(args).await,
    }
}

pub(super) fn p2p_http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .context("building P2P HTTP client")
}

pub(super) async fn p2p_probe_get(client: &reqwest::Client, url: &str) -> Value {
    match crate::http_get_json::<Value>(client, url).await {
        Ok(value) => json!({
            "ok": true,
            "value": value,
        }),
        Err(error) => json!({
            "ok": false,
            "error": error.to_string(),
        }),
    }
}
