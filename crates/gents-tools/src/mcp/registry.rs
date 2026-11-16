use anyhow::anyhow;
use serde::{Deserialize, Deserializer};

pub(crate) fn null_as_empty_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
}

pub(crate) fn null_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Debug, thiserror::Error)]
pub enum RegistryReadError {
    #[error("MCP registry owner is required")]
    MissingNode,
    #[error("MCP service identity is required")]
    MissingService,
    #[error("MCP registry returned an invalid scoped identity")]
    InvalidIdentity,
    #[error("duplicate MCP service {service_id} for node {node_did}")]
    Duplicate {
        service_id: String,
        node_did: String,
    },
    #[error(transparent)]
    View(#[from] gents_store::views::ViewError),
}

pub async fn configured_mcp_services_on(
    access: &gents_store::config_client::ConfigAccess,
    node_did: &str,
) -> Result<Vec<gents_documents::ToolServiceRegistry>, RegistryReadError> {
    if node_did.trim().is_empty() {
        return Err(RegistryReadError::MissingNode);
    }
    let services = access
        .read_view(&super::views::Services {
            node_did,
            service_id: None,
            enabled_only: false,
        })
        .await?;
    validate_services(&services, node_did)?;
    Ok(services)
}

pub(crate) async fn configured_mcp_service_on(
    access: &gents_store::config_client::ConfigAccess,
    node_did: &str,
    service_id: &str,
) -> Result<Option<gents_documents::ToolServiceRegistry>, RegistryReadError> {
    if node_did.trim().is_empty() {
        return Err(RegistryReadError::MissingNode);
    }
    if service_id.trim().is_empty() {
        return Err(RegistryReadError::MissingService);
    }
    let services = access
        .read_view(&super::views::Services {
            node_did,
            service_id: Some(service_id),
            enabled_only: false,
        })
        .await?;
    validate_services(&services, node_did)?;
    Ok(services.into_iter().next())
}

pub(crate) fn validate_services(
    services: &[gents_documents::ToolServiceRegistry],
    node_did: &str,
) -> Result<(), RegistryReadError> {
    let mut names = rapidhash::RapidHashSet::default();
    for service in services {
        if service.node_did != node_did || service.service_id.trim().is_empty() {
            return Err(RegistryReadError::InvalidIdentity);
        }
        if !names.insert(service.service_id.as_str()) {
            return Err(RegistryReadError::Duplicate {
                service_id: service.service_id.clone(),
                node_did: node_did.to_owned(),
            });
        }
    }
    Ok(())
}

#[derive(PartialEq, Eq)]
pub struct ResolvedMcpService {
    pub endpoint: String,
    pub send_node_did: bool,
}

pub fn resolve_mcp_service(
    entry: &gents_documents::ToolServiceRegistry,
    hostname: &str,
    subnet: Option<&str>,
) -> anyhow::Result<ResolvedMcpService> {
    let service = crate::mcp::health::McpHealthCheckService::from(entry.clone());
    let endpoint = service
        .endpoint(hostname, subnet)
        .map_err(|error| anyhow!("service '{}': {error}", entry.service_id))?;
    Ok(ResolvedMcpService {
        endpoint,
        send_node_did: entry.send_node_did,
    })
}

#[cfg(test)]
mod tests {
    use crate::mcp::health::McpHealthCheckService;
    use gents_documents::ToolServiceRegistry;

    fn service(endpoint_url: &str) -> McpHealthCheckService {
        McpHealthCheckService {
            service_id: "svc".into(),
            hostname: "remote-host".into(),
            tailscale_ip: String::new(),
            lan_ip: String::new(),
            mcp_port: Some(9201),
            mcp_path: "/mcp".into(),
            endpoint_url: endpoint_url.into(),
            send_node_did: false,
        }
    }

    #[test]
    fn endpoint_url_overrides_address_fields_and_keeps_its_scheme() {
        assert_eq!(
            service("").endpoint("local", None).unwrap(),
            "http://remote-host:9201/mcp"
        );
        assert_eq!(
            service("https://mcp.linear.app/mcp")
                .endpoint("remote-host", None)
                .unwrap(),
            "https://mcp.linear.app/mcp"
        );
        let mut without_address = service("https://mcp.linear.app/mcp");
        without_address.hostname.clear();
        without_address.mcp_port = None;
        assert!(without_address.endpoint("local", None).is_ok());
        assert_eq!(
            service("https://host/mcp?key=1").endpoint("local", None),
            Err(crate::mcp::health::McpEndpointDefect::InvalidEndpointUrl)
        );
    }

    #[test]
    fn endpoint_url_accepts_http_and_https_and_canonicalises() {
        assert_eq!(
            ToolServiceRegistry::parse_endpoint_url(" https://mcp.linear.app/mcp ")
                .unwrap()
                .as_str(),
            "https://mcp.linear.app/mcp"
        );
        assert_eq!(
            ToolServiceRegistry::parse_endpoint_url("HTTP://127.0.0.1:12799")
                .unwrap()
                .as_str(),
            "http://127.0.0.1:12799/"
        );
    }

    #[test]
    fn endpoint_url_refuses_other_schemes_credentials_and_queries() {
        for bad in [
            "",
            "mcp.linear.app/mcp",
            "ws://host/mcp",
            "file:///tmp/mcp",
            "https://user:secret@host/mcp",
            "https://token@host/mcp",
            "https://host/mcp?api_key=secret",
            "https://host/mcp#frag",
        ] {
            assert!(
                ToolServiceRegistry::parse_endpoint_url(bad).is_err(),
                "{bad} must be refused"
            );
        }
    }
}
