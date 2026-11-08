use std::path::Path;

use anyhow::{Context, Result};
use gents::store::config_client::ConfigAccess;
use gents::store::identity::NodeIdentity as _;

use super::super::bootstrap::normalize_required;
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
            .and_then(|record| {
                record.operator_graphql().and_then(|url| {
                    let capability =
                        gents::store::config_client::ClientSigningCapability::from_ed25519_key(
                            self.node_identity.private_key_bytes(),
                        )
                        .ok()?;
                    gents::store::config_client::GraphqlEndpoint::as_principal(
                        url,
                        self.node_identity.did(),
                    )
                    .with_client_signing_capability(capability)
                    .ok()
                    .map(|endpoint| endpoint.with_client_signed_agent_creates())
                })
            })
    }

    pub fn operator_observation_graphql(
        &self,
        node_did: &str,
    ) -> Option<gents::store::config_client::GraphqlEndpoint> {
        self.sync_state
            .records()
            .iter()
            .find(|record| record.node_did == node_did)
            .and_then(|record| {
                crate::local_runtime::operator_endpoint(
                    record,
                    &self.node_identity.client_signing_capability().ok()?,
                )
            })
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
        let capability = self.node_identity.client_signing_capability().ok()?;
        tokio::task::spawn_blocking(move || {
            crate::local_runtime::operator_endpoint(&record, &capability)
        })
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
        anyhow::ensure!(
            record.operator_graphql().is_some(),
            "runtime has no operator endpoint"
        );
        Ok(std::sync::Arc::new(self.node_identity.clone()))
    }

    /// Operational requests such as shared credential usage refresh retain the
    /// co-hosted runtime signer. Configuration mutations use `operator_signer`.
    pub fn runtime_operator_signer(
        &self,
        node_did: &str,
    ) -> Result<std::sync::Arc<dyn gents::store::identity::NodeIdentity>> {
        let record = self
            .sync_state
            .records()
            .into_iter()
            .find(|record| record.node_did == node_did)
            .context("no runtime record for this node")?;
        crate::local_runtime::runtime_operator_signer(&record)
    }

    pub fn operator_access(&self, node_did: &str) -> Result<ConfigAccess> {
        let record = self
            .sync_state
            .records()
            .into_iter()
            .find(|record| record.node_did == node_did)
            .context("Connect this runtime before editing configuration.")?;
        record
            .operator_graphql()
            .map(|url| -> Result<ConfigAccess> {
                let capability =
                    gents::store::config_client::ClientSigningCapability::from_ed25519_key(
                        self.node_identity.private_key_bytes(),
                    )?;
                Ok(ConfigAccess::Graphql(
                    gents::store::config_client::GraphqlEndpoint::as_principal(
                        url,
                        self.node_identity.did(),
                    )
                    .with_client_signing_capability(capability)?
                    .with_client_signed_agent_creates(),
                ))
            })
            .transpose()?
            .context("Connect this runtime before editing configuration.")
    }

    pub async fn provision_agent_identity(
        &self,
        node_did: &str,
    ) -> Result<gents_protocol::agent_identity::AgentIdentityBinding> {
        let endpoint = self
            .operator_graphql(node_did)
            .context("Connect this runtime before creating an agent.")?;
        Ok(crate::agent_identity::provision(&endpoint, node_did).await?)
    }

    pub fn local_home_operator_access(
        &self,
        node_did: &str,
        home: &std::path::Path,
    ) -> Result<ConfigAccess> {
        let records = self.sync_state.records();
        let record = ux_local_runtime_record(&records, node_did, home)?;
        record
            .operator_graphql()
            .map(|url| -> Result<ConfigAccess> {
                let capability =
                    gents::store::config_client::ClientSigningCapability::from_ed25519_key(
                        self.node_identity.private_key_bytes(),
                    )?;
                Ok(ConfigAccess::Graphql(
                    gents::store::config_client::GraphqlEndpoint::as_principal(
                        url,
                        self.node_identity.did(),
                    )
                    .with_client_signing_capability(capability)?
                    .with_client_signed_agent_creates(),
                ))
            })
            .transpose()?
            .context("the local node that installed this app has no operator connection")
    }
}
