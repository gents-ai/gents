mod access;
mod collections;
mod connect;
mod documents;
mod invite;
mod join;
mod network;
mod network_admin;
mod output;
mod pairings;
mod replicators;
mod templates;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::cli::args::{
    P2pAdminCommand, P2pCollectionsCommand, P2pCommand, P2pDocumentsCommand, P2pNetworkCommand,
    P2pPairingsCommand, P2pReplicatorsCommand, P2pTemplatesCommand,
};

pub(crate) use output::{
    fetch_live_http_p2p_status, flatten_p2p_fields, load_live_http_p2p_status, persisted_p2p_status,
};

pub(crate) async fn dispatch(command: P2pCommand) -> Result<()> {
    match command {
        P2pCommand::Status(args) => access::p2p_status(args).await,
        P2pCommand::Peers(args) => access::p2p_peers(args).await,
        P2pCommand::Diagnose(args) => connect::p2p_diagnose(args).await,
        P2pCommand::Pairings { command } => match command {
            P2pPairingsCommand::List(args) => pairings::p2p_pairings_list(args).await,
            P2pPairingsCommand::Set(args) => pairings::p2p_pairings_set(args).await,
            P2pPairingsCommand::Remove(args) => pairings::p2p_pairings_remove(args).await,
            P2pPairingsCommand::Invite(args) => invite::p2p_invite(args).await,
            P2pPairingsCommand::Join(args) => join::p2p_join(args).await,
        },
        P2pCommand::Network { command } => match command {
            P2pNetworkCommand::Register(args) => network::p2p_network_register(args).await,
            P2pNetworkCommand::List(args) => network::p2p_network_list(args).await,
            P2pNetworkCommand::Rm(args) => network::p2p_network_rm(args).await,
            P2pNetworkCommand::Create(args) => network_admin::p2p_network_create(args).await,
            P2pNetworkCommand::Grant(args) => network_admin::p2p_network_grant(args).await,
            P2pNetworkCommand::Revoke(args) => network_admin::p2p_network_revoke(args).await,
        },
        P2pCommand::Templates { command } => match command {
            P2pTemplatesCommand::List(args) => templates::p2p_templates_list(args).await,
        },
        P2pCommand::Admin { command } => match command {
            P2pAdminCommand::Connect(args) => connect::p2p_connect(args).await,
            P2pAdminCommand::Collections { command } => match command {
                P2pCollectionsCommand::List(args) => collections::p2p_collections_list(args).await,
                P2pCollectionsCommand::Add(args) => collections::p2p_collections_add(args).await,
                P2pCollectionsCommand::Remove(args) => {
                    collections::p2p_collections_remove(args).await
                }
                P2pCollectionsCommand::SyncBranchable(args) => {
                    collections::p2p_collections_sync_branchable(args).await
                }
                P2pCollectionsCommand::SyncVersions(args) => {
                    collections::p2p_collections_sync_versions(args).await
                }
            },
            P2pAdminCommand::Replicators { command } => match command {
                P2pReplicatorsCommand::List(args) => replicators::p2p_replicators_list(args).await,
                P2pReplicatorsCommand::Add(args) => replicators::p2p_replicators_add(args).await,
                P2pReplicatorsCommand::Remove(args) => {
                    replicators::p2p_replicators_remove(args).await
                }
            },
            P2pAdminCommand::Documents { command } => match command {
                P2pDocumentsCommand::List(args) => documents::p2p_documents_list(args).await,
                P2pDocumentsCommand::Add(args) => documents::p2p_documents_add(args).await,
                P2pDocumentsCommand::Remove(args) => documents::p2p_documents_remove(args).await,
                P2pDocumentsCommand::Sync(args) => documents::p2p_documents_sync(args).await,
            },
        },
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
