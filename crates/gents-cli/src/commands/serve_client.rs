use std::path::Path;

use anyhow::Result;
use gents_host::serve::{BoundRootAgents, ServeClient};
use serde_json::Value;

use crate::cli::ManifestNodeDidBindingArg;
use crate::commands::config::binding::{load_bound_manifest, ManifestBindingOptions};

pub(crate) struct CliServeClient;

#[async_trait::async_trait]
impl ServeClient for CliServeClient {
    fn report(&self, report: &Value) -> Result<()> {
        crate::print_json(report)
    }

    fn start_failure_hint(&self, home_dir: &Path) -> String {
        crate::server_start_failure_hint(home_dir)
    }

    fn version(&self) -> gents_host::http::version::VersionResponse {
        crate::version::build_version()
    }

    async fn bound_root_agents(
        &self,
        root: &Path,
        home_dir: &Path,
        access: &gents_store::config_client::ConfigAccess,
    ) -> Result<BoundRootAgents> {
        let bound = load_bound_manifest(ManifestBindingOptions {
            root,
            home: Some(home_dir),
            graphql: None,
            bind_node_did: Some(ManifestNodeDidBindingArg::Home),
            force_rebind_concrete_did: true,
            access: Some(access),
        })
        .await?
        .require_valid()?;
        Ok(BoundRootAgents {
            default_agent: bound
                .manifest
                .default_agent_selection
                .as_ref()
                .and_then(|selection| selection.selected())
                .map(str::to_owned),
            agents: bound
                .manifest
                .agents
                .iter()
                .map(|agent| agent.agent_did.clone())
                .collect(),
        })
    }
}
