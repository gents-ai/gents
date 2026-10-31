use std::path::Path;

use anyhow::{Context, Result};
use gents::store::config_client::ConfigAccess;

use super::super::bootstrap::normalize_required;
use super::super::p2p_ops;
use super::{ux_local_runtime_record, ClientCore};
use crate::client::mutations::PeerMutationResult;
use crate::client::peer_directory::{PeerRecord, RemovalCause};

impl ClientCore {
    pub async fn refresh_local_standard_peer(
        &self,
        node_home: &Path,
        label: &str,
    ) -> Result<PeerRecord> {
        let discovery = crate::local_runtime::discover_standard_runtime(node_home).await?;
        self.persist_local_standard_peer(
            label,
            &discovery.p2p_listen_address,
            &discovery.node_did,
            &discovery.graphql,
            &node_home.display().to_string(),
        )
        .await
    }

    pub async fn persist_local_standard_peer(
        &self,
        label: &str,
        addr: &str,
        node_did: &str,
        graphql: &str,
        node_home: &str,
    ) -> Result<PeerRecord> {
        self.sync_state
            .upsert_local_standard_peer(
                normalize_required("label", label)?,
                normalize_required("addr", addr)?,
                normalize_required("node_did", node_did)?,
                normalize_required("graphql", graphql)?,
                normalize_required("node_home", node_home)?,
            )
            .await
    }

    pub async fn network_status(&self) -> NetworkStatus {
        let local_peer_id = p2p_ops::p2p_local_peer_id(&self.p2p).await;
        let listen_addresses = p2p_ops::p2p_listen_addresses(&self.p2p).await;
        let connected_peers = p2p_ops::p2p_connected_peers(&self.p2p).await;
        let replicators = p2p_ops::p2p_get_replicators(&self.p2p).await;
        let saved_peers = self.sync_state.records();

        NetworkStatus {
            local_peer_id: local_peer_id.map_err(|error| error.to_string()),
            listen_addresses: listen_addresses.map_err(|error| error.to_string()),
            connected_peers: connected_peers.map_err(|error| error.to_string()),
            replicators: replicators
                .map(|rows| {
                    rows.into_iter()
                        .map(|info| NetworkReplicator {
                            peer_id: info.id,
                            address: info.address,
                            collections: info.collections,
                            status: info.status,
                            last_status_change: info.last_status_change,
                        })
                        .collect()
                })
                .map_err(|error| error.to_string()),
            saved_peers,
        }
    }

    pub async fn rename_peer(&self, peer_id: &str, label: &str) -> Result<()> {
        let peer_id = normalize_required("peer_id", peer_id)?;
        let label = normalize_required("label", label)?;
        let record = self
            .sync_state
            .records()
            .iter()
            .find(|record| record.peer_id == peer_id)
            .cloned()
            .with_context(|| format!("peer {peer_id} not found"))?;
        let mut renamed = record.clone();
        renamed.label = label.to_string();
        self.sync_state
            .replace_record(&record, renamed)
            .await?
            .with_context(|| format!("peer {peer_id} changed while it was being renamed"))?;
        self.clear_mutation_error();
        Ok(())
    }

    pub async fn remove_peer(&self, peer_id: &str) -> Result<PeerMutationResult> {
        let peer_id = normalize_required("peer_id", peer_id)?;
        let removal = self
            .route_manager
            .remove_peer(&self.sync_state, peer_id, RemovalCause::Operator)
            .await?;
        let removed = removal.record;
        let warning = match removal.cleanup_error {
            None => None,
            Some(error) => {
                let warning = format!(
                    "deployment hidden locally; route teardown is pending and will retry: {error}"
                );
                tracing::warn!(
                    target: "gents_desktop_core::peer",
                    peer_id = %removed.peer_id,
                    label = %removed.label,
                    error = %error,
                    "desktop deployment removal queued for retry"
                );
                Some(warning)
            }
        };

        self.clear_mutation_error();
        tracing::info!(
            target: "gents_desktop_core::peer",
            peer_id = %removed.peer_id,
            label = %removed.label,
            "desktop deployment removed"
        );
        Ok(PeerMutationResult {
            peer_id: removed.peer_id,
            label: removed.label,
            addr: removed.addr,
            connected: false,
            warning,
        })
    }

    pub fn operator_graphql(
        &self,
        node_did: &str,
    ) -> Option<gents::store::config_client::GraphqlEndpoint> {
        self.sync_state
            .records()
            .iter()
            .find(|record| record.node_did == node_did)
            .and_then(crate::local_runtime::operator_endpoint)
    }

    pub fn operator_observation_graphql(
        &self,
        node_did: &str,
    ) -> Option<gents::store::config_client::GraphqlEndpoint> {
        self.sync_state
            .records()
            .iter()
            .find(|record| record.node_did == node_did)
            .and_then(|record| self.options.operator_identity_interaction.endpoint(record))
    }

    pub async fn operator_observation_endpoint(
        &self,
        node_did: &str,
    ) -> Option<gents::store::config_client::GraphqlEndpoint> {
        let record = self
            .sync_state
            .records()
            .into_iter()
            .find(|record| record.node_did == node_did)?;
        let interaction = self.options.operator_identity_interaction;
        tokio::task::spawn_blocking(move || interaction.endpoint(&record))
            .await
            .ok()
            .flatten()
    }

    pub fn operator_signer(
        &self,
        node_did: &str,
    ) -> Result<std::sync::Arc<dyn gents::store::identity::NodeIdentity>> {
        let record = self
            .sync_state
            .records()
            .into_iter()
            .find(|record| record.node_did == node_did)
            .context("no runtime record for this node")?;
        crate::local_runtime::operator_signer(&record)
    }

    pub fn operator_access(&self, node_did: &str) -> Result<ConfigAccess> {
        let record = self
            .sync_state
            .records()
            .into_iter()
            .find(|record| record.node_did == node_did);
        match record {
            Some(record) if record.operator_graphql().is_some() => {
                crate::local_runtime::operator_endpoint(&record)
                    .map(ConfigAccess::Graphql)
                    .context("local standard runtime has no operator GraphQL endpoint")
            }
            Some(_) => anyhow::bail!(
                "managed node {node_did} has no operator GraphQL endpoint; refusing a desktop-replica configuration fallback"
            ),
            None => Ok(ConfigAccess::local(self.node_arc())),
        }
    }

    pub fn local_home_operator_access(
        &self,
        node_did: &str,
        home: &std::path::Path,
    ) -> Result<ConfigAccess> {
        let records = self.sync_state.records();
        let record = ux_local_runtime_record(&records, node_did, home)?;
        crate::local_runtime::operator_endpoint(record)
            .map(ConfigAccess::Graphql)
            .context("the local node that installed this app has no operator connection")
    }
}

#[derive(Debug, Clone)]
pub struct NetworkStatus {
    pub local_peer_id: std::result::Result<String, String>,
    pub listen_addresses: std::result::Result<Vec<String>, String>,
    pub connected_peers: std::result::Result<Vec<String>, String>,
    pub replicators: std::result::Result<Vec<NetworkReplicator>, String>,
    pub saved_peers: Vec<crate::client::peer_directory::PeerRecord>,
}

#[derive(Debug, Clone)]
pub struct NetworkReplicator {
    pub peer_id: Option<String>,
    pub address: Option<String>,
    pub collections: Vec<String>,
    pub status: Option<u8>,
    pub last_status_change: Option<String>,
}
