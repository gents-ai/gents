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

pub(crate) async fn configured_mcp_services(
    node: &defra_node::EmbeddedNode,
    node_did: &str,
) -> anyhow::Result<Vec<crate::documents::ToolServiceRegistry>> {
    let query = services_query(node_did)?;
    let response = crate::store::graphql::graphql_with_transaction_retry(
        node,
        &query,
        "configured_mcp_services",
    )
    .await?;
    parse_services(response.data.as_ref(), node_did)
}

pub async fn configured_mcp_services_on(
    access: &crate::ConfigAccess,
    node_did: &str,
) -> anyhow::Result<Vec<crate::documents::ToolServiceRegistry>> {
    let response = access.execute(&services_query(node_did)?).await?;
    parse_services(response.get("data"), node_did)
}

fn services_query(node_did: &str) -> anyhow::Result<String> {
    anyhow::ensure!(
        !node_did.trim().is_empty(),
        "MCP registry owner is required"
    );
    let (fields, _) = crate::store::config_client::config_projection(
        crate::Collection::ToolServiceRegistry,
        None,
    )?;
    let owner = crate::store::graphql::escape_graphql_string(node_did);
    let query = format!(
        "{{ ToolServiceRegistry(filter: {{ node_did: {{ _eq: \"{owner}\" }} }}) {{ {} }} }}",
        fields.join(" ")
    );
    Ok(query)
}

fn parse_services(
    data: Option<&serde_json::Value>,
    node_did: &str,
) -> anyhow::Result<Vec<crate::documents::ToolServiceRegistry>> {
    let rows = data
        .and_then(|data| data.get("ToolServiceRegistry"))
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("MCP registry query returned no rows array"))?;
    let mut names = rapidhash::RapidHashSet::default();
    let mut services = Vec::with_capacity(rows.len());
    for row in rows {
        let service: crate::documents::ToolServiceRegistry = serde_json::from_value(row.clone())?;
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

pub struct ResolvedMcpService {
    pub endpoint: String,
    pub send_node_did: bool,
}

pub fn resolve_mcp_service(
    entry: &crate::documents::ToolServiceRegistry,
    hostname: &str,
    subnet: Option<&str>,
) -> anyhow::Result<ResolvedMcpService> {
    let service = crate::tools::mcp::health::McpHealthCheckService::from(entry.clone());
    let endpoint = service
        .endpoint(hostname, subnet)
        .map_err(|error| anyhow!("service '{}': {error}", entry.service_id))?;
    Ok(ResolvedMcpService {
        endpoint,
        send_node_did: entry.send_node_did,
    })
}

/// Validates an operator-supplied MCP endpoint and returns its canonical form.
/// Credentials and query strings are refused so no secret can travel in the URL
/// or be echoed back through health rows, which record the endpoint verbatim.
pub fn parse_endpoint_url(raw: &str) -> anyhow::Result<reqwest::Url> {
    let url = reqwest::Url::parse(raw.trim())
        .map_err(|error| anyhow!("endpoint_url must be an absolute URL: {error}"))?;
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

#[cfg(test)]
mod tests {
    use super::parse_endpoint_url;
    use crate::tools::mcp::health::McpHealthCheckService;

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
            Err("registry entry has invalid endpoint_url")
        );
    }

    #[test]
    fn endpoint_url_accepts_http_and_https_and_canonicalises() {
        assert_eq!(
            parse_endpoint_url(" https://mcp.linear.app/mcp ")
                .unwrap()
                .as_str(),
            "https://mcp.linear.app/mcp"
        );
        assert_eq!(
            parse_endpoint_url("HTTP://127.0.0.1:12799")
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
            assert!(parse_endpoint_url(bad).is_err(), "{bad} must be refused");
        }
    }
}
