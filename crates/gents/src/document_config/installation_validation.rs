use anyhow::{ensure, Result};
use super::{RepositoryPlacement, ToolServiceRegistry};

impl ToolServiceRegistry {
    pub fn validate(&self) -> Result<()> {
        ensure!(self.mcp_port.is_some_and(|port| port > 0),
            "service {} must contain a positive mcp_port", self.service_id);
        ensure!([&self.hostname, &self.tailscale_ip, &self.lan_ip].into_iter()
            .any(|address| address.as_deref().is_some_and(|address| !address.trim().is_empty())),
            "service {} requires hostname, tailscale_ip, or lan_ip", self.service_id);
        Ok(())
    }
}

impl RepositoryPlacement {
    pub fn validate(&self) -> Result<()> {
        ensure!(!self.host_path.trim().is_empty(),
            "repository placement {} must contain a non-empty host_path", self.repository_id);
        Ok(())
    }
}
