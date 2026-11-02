use super::{RepositoryPlacement, ToolServiceRegistry};
use anyhow::{ensure, Result};

impl ToolServiceRegistry {
    pub fn validate(&self) -> Result<()> {
        if let Some(endpoint_url) = self
            .endpoint_url
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            crate::tools::mcp::registry::parse_endpoint_url(endpoint_url)
                .map_err(|error| anyhow::anyhow!("service {}: {error}", self.service_id))?;
            return Ok(());
        }
        ensure!(
            self.mcp_port.is_some_and(|port| port > 0),
            "service {} must contain a positive mcp_port",
            self.service_id
        );
        ensure!(
            [&self.hostname, &self.tailscale_ip, &self.lan_ip]
                .into_iter()
                .any(|address| address
                    .as_deref()
                    .is_some_and(|address| !address.trim().is_empty())),
            "service {} requires hostname, tailscale_ip, or lan_ip",
            self.service_id
        );
        Ok(())
    }
}

impl RepositoryPlacement {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.host_path.trim().is_empty(),
            "repository placement {} must contain a non-empty host_path",
            self.repository_id
        );
        Ok(())
    }
}
