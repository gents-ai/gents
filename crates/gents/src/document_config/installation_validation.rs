use super::error::DocumentConfigError;
use super::{RepositoryPlacement, ToolServiceRegistry};

impl ToolServiceRegistry {
    pub fn validate(&self) -> Result<(), DocumentConfigError> {
        if !self.mcp_port.is_some_and(|port| port > 0) {
            return Err(DocumentConfigError::ServiceMcpPort {
                service_id: self.service_id.clone(),
            });
        }
        if ![&self.hostname, &self.tailscale_ip, &self.lan_ip]
            .into_iter()
            .any(|address| {
                address
                    .as_deref()
                    .is_some_and(|address| !address.trim().is_empty())
            })
        {
            return Err(DocumentConfigError::ServiceAddress {
                service_id: self.service_id.clone(),
            });
        }
        Ok(())
    }
}

impl RepositoryPlacement {
    pub fn validate(&self) -> Result<(), DocumentConfigError> {
        if self.host_path.trim().is_empty() {
            return Err(DocumentConfigError::RepositoryHostPath {
                repository_id: self.repository_id.clone(),
            });
        }
        Ok(())
    }
}
