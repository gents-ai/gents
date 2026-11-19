use super::{ux_local_runtime_record, ClientRuntime, LocalHomeAccessError};
use anyhow::{Context, Result};
use gents_store::config_client::ConfigAccess;

impl ClientRuntime {
    pub fn operator_graphql(
        &self,
        node_did: &str,
    ) -> Option<gents_store::config_client::GraphqlEndpoint> {
        self.directory
            .records()
            .iter()
            .find(|record| record.node_did == node_did)
            .and_then(|record| {
                Some(record.graphql.as_str()).and_then(|url| {
                    self.runtime_endpoints
                        .endpoint(&record.node_did, url)
                        .ok()
                        .and_then(|endpoint| {
                            gents::runtime::lifecycle_hooks::compose_endpoint(
                                endpoint.with_client_signed_agent_creates(),
                            )
                            .ok()
                        })
                })
            })
    }

    pub fn operator_observation_graphql(
        &self,
        node_did: &str,
    ) -> Option<gents_store::config_client::GraphqlEndpoint> {
        self.directory
            .records()
            .iter()
            .find(|record| record.node_did == node_did)
            .and_then(|record| {
                self.runtime_endpoints
                    .endpoint(&record.node_did, &record.graphql)
                    .ok()
            })
    }

    pub fn operator_signer(
        &self,
        node_did: &str,
    ) -> Result<std::sync::Arc<dyn gents_store::identity::NodeIdentity>> {
        let record = self
            .directory
            .records()
            .into_iter()
            .find(|record| record.node_did == node_did)
            .context("no runtime record for this node")?;
        anyhow::ensure!(
            !record.graphql.is_empty(),
            "runtime has no operator endpoint"
        );
        Ok(std::sync::Arc::new(self.node_identity.clone()))
    }

    pub fn operator_access(&self, node_did: &str) -> Result<ConfigAccess> {
        self.operator_graphql(node_did)
            .map(ConfigAccess::Graphql)
            .context("Connect this runtime before editing configuration.")
    }

    pub async fn provision_agent_identity(
        &self,
        node_did: &str,
    ) -> Result<gents_protocol::agent_identity::AgentIdentityBinding> {
        let endpoint = self
            .operator_graphql(node_did)
            .context("Connect this runtime before creating an agent.")?;
        Ok(gents_store::identity::provision_agent_identity_remote(&endpoint, node_did).await?)
    }

    /// The User's signed endpoint on the managed runtime whose home installed
    /// a pack app.
    pub fn local_home_operator_endpoint(
        &self,
        node_did: &str,
        home: &std::path::Path,
    ) -> std::result::Result<gents_store::config_client::GraphqlEndpoint, LocalHomeAccessError>
    {
        let records = self.directory.records();
        let record = ux_local_runtime_record(&records, node_did, home)?;
        let url = record.graphql.as_str();
        Ok(self
            .runtime_endpoints
            .endpoint(&record.node_did, url)
            .map_err(LocalHomeAccessError::Signing)?
            .with_client_signed_agent_creates())
    }
}
