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

pub async fn configured_mcp_services_on(
    access: &gents_store::config_client::ConfigAccess,
    node_did: &str,
) -> anyhow::Result<Vec<gents_documents::ToolServiceRegistry>> {
    let response = access.execute(&services_query(node_did)?).await?;
    parse_services(response.get("data"), node_did)
}

pub(crate) async fn configured_mcp_service_on(
    access: &gents_store::config_client::ConfigAccess,
    node_did: &str,
    service_id: &str,
) -> anyhow::Result<Option<gents_documents::ToolServiceRegistry>> {
    anyhow::ensure!(
        !service_id.trim().is_empty(),
        "MCP service identity is required"
    );
    let query = services_query_for(node_did, Some(service_id))?;
    let response = access.execute(&query).await?;
    Ok(parse_services(response.get("data"), node_did)?
        .into_iter()
        .next())
}

pub(crate) fn services_query(node_did: &str) -> anyhow::Result<String> {
    services_query_for(node_did, None)
}

fn services_query_for(node_did: &str, service_id: Option<&str>) -> anyhow::Result<String> {
    anyhow::ensure!(
        !node_did.trim().is_empty(),
        "MCP registry owner is required"
    );
    let (fields, _) = gents_documents::config_projection(
        gents_store::collection::Collection::ToolServiceRegistry,
        None,
    )?;
    let owner = gents_store::graphql::escape_graphql_string(node_did);
    let service_filter = service_id
        .map(|id| {
            format!(
                ", service_id: {{ _eq: \"{}\" }}",
                gents_store::graphql::escape_graphql_string(id)
            )
        })
        .unwrap_or_default();
    let query = format!(
        "{{ ToolServiceRegistry(filter: {{ node_did: {{ _eq: \"{owner}\" }}{service_filter} }}) {{ {} }} }}",
        fields.join(" ")
    );
    Ok(query)
}

pub(crate) fn parse_services(
    data: Option<&serde_json::Value>,
    node_did: &str,
) -> anyhow::Result<Vec<gents_documents::ToolServiceRegistry>> {
    let rows = data
        .and_then(|data| data.get("ToolServiceRegistry"))
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("MCP registry query returned no rows array"))?;
    let mut names = rapidhash::RapidHashSet::default();
    let mut services = Vec::with_capacity(rows.len());
    for row in rows {
        let service: gents_documents::ToolServiceRegistry = serde_json::from_value(row.clone())?;
        anyhow::ensure!(
            service.node_did == node_did && !service.service_id.trim().is_empty(),
            "MCP registry returned an invalid scoped identity"
        );
        anyhow::ensure!(
            names.insert(service.service_id.clone()),
            "duplicate MCP service {} for node {node_did}",
            service.service_id
        );
        services.push(service);
    }
    Ok(services)
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
            updated_at: None,
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
