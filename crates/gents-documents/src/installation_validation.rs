use super::{RepositoryPlacement, ToolServiceRegistry};
use anyhow::{ensure, Result};

impl ToolServiceRegistry {
    /// Validates an operator-supplied MCP endpoint and returns its canonical form.
    /// Credentials and query strings are refused so no secret can travel in the URL
    /// or be echoed back through health rows, which record the endpoint verbatim.
    pub fn parse_endpoint_url(raw: &str) -> anyhow::Result<reqwest::Url> {
        let url = reqwest::Url::parse(raw.trim())
            .map_err(|error| anyhow::anyhow!("endpoint_url must be an absolute URL: {error}"))?;
        anyhow::ensure!(
            matches!(url.scheme(), "http" | "https"),
            "endpoint_url must use http or https"
        );
        anyhow::ensure!(
            url.host_str().is_some_and(|host| !host.is_empty()),
            "endpoint_url must contain a host"
        );
        anyhow::ensure!(
            url.username().is_empty() && url.password().is_none(),
            "endpoint_url must not contain credentials"
        );
        anyhow::ensure!(
            url.query().is_none() && url.fragment().is_none(),
            "endpoint_url must not contain a query string or fragment"
        );
        Ok(url)
    }

    pub fn validate(&self) -> Result<()> {
        if let Some(endpoint_url) = self
            .endpoint_url
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            Self::parse_endpoint_url(endpoint_url)
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
