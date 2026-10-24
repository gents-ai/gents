use serde::{Deserialize, Deserializer};

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("MCP registry query returned no rows array")]
    NoRowsArray,
    #[error("MCP registry returned an invalid scoped identity")]
    InvalidScopedIdentity,
    #[error("duplicate MCP service {service_id} for node {node_did}")]
    DuplicateService {
        service_id: String,
        node_did: String,
    },
    #[error(transparent)]
    Decode(#[from] serde_json::Error),
    #[error("service '{service_id}' has no valid MCP port")]
    NoValidMcpPort { service_id: String },
    #[error("service '{service_id}' has no MCP address")]
    NoMcpAddress { service_id: String },
}

type Result<T, E = RegistryError> = std::result::Result<T, E>;

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
) -> anyhow::Result<Vec<crate::document_config::ToolServiceRegistry>> {
    let query = services_query(node_did)?;
    let response =
        crate::graphql::graphql_with_transaction_retry(node, &query, "configured_mcp_services")
            .await?;
    Ok(parse_services(response.data.as_ref(), node_did)?)
}

pub async fn configured_mcp_services_on(
    access: &crate::ConfigAccess,
    node_did: &str,
) -> anyhow::Result<Vec<crate::document_config::ToolServiceRegistry>> {
    let response = access.execute(&services_query(node_did)?).await?;
    Ok(parse_services(response.get("data"), node_did)?)
}

fn services_query(node_did: &str) -> anyhow::Result<String> {
    anyhow::ensure!(
        !node_did.trim().is_empty(),
        "MCP registry owner is required"
    );
    let (fields, _) =
        crate::config_client::config_projection(crate::Collection::ToolServiceRegistry, None)?;
    let owner = crate::graphql::escape_graphql_string(node_did);
    let query = format!(
        "{{ ToolServiceRegistry(filter: {{ node_did: {{ _eq: \"{owner}\" }} }}) {{ {} }} }}",
        fields.join(" ")
    );
    Ok(query)
}

fn parse_services(
    data: Option<&serde_json::Value>,
    node_did: &str,
) -> Result<Vec<crate::document_config::ToolServiceRegistry>> {
    let rows = data
        .and_then(|data| data.get("ToolServiceRegistry"))
        .and_then(serde_json::Value::as_array)
        .ok_or(RegistryError::NoRowsArray)?;
    let mut names = rapidhash::RapidHashSet::default();
    let mut services = Vec::with_capacity(rows.len());
    for row in rows {
        let service: crate::document_config::ToolServiceRegistry =
            serde_json::from_value(row.clone())?;
        if service.node_did != node_did || service.service_id.trim().is_empty() {
            return Err(RegistryError::InvalidScopedIdentity);
        }
        if !names.insert(service.service_id.clone()) {
            return Err(RegistryError::DuplicateService {
                service_id: service.service_id.clone(),
                node_did: node_did.to_owned(),
            });
        }
        services.push(service);
    }
    Ok(services)
}

pub struct ResolvedMcpService {
    pub endpoint: String,
    pub send_node_did: bool,
}

pub fn resolve_mcp_service(
    entry: &crate::document_config::ToolServiceRegistry,
    hostname: &str,
    subnet: Option<&str>,
) -> Result<ResolvedMcpService> {
    let port = entry
        .mcp_port
        .and_then(|port| u16::try_from(port).ok())
        .filter(|port| *port > 0)
        .ok_or_else(|| RegistryError::NoValidMcpPort {
            service_id: entry.service_id.clone(),
        })?;
    let host = entry.hostname.as_deref().unwrap_or_default();
    let tailscale = entry.tailscale_ip.as_deref().unwrap_or_default();
    let lan = entry.lan_ip.as_deref().unwrap_or_default();
    if host.is_empty() && tailscale.is_empty() && lan.is_empty() {
        return Err(RegistryError::NoMcpAddress {
            service_id: entry.service_id.clone(),
        });
    }
    Ok(ResolvedMcpService {
        endpoint: crate::mcp_pool::resolve_mcp_url(
            host,
            tailscale,
            lan,
            port,
            entry.mcp_path.as_deref().unwrap_or_default(),
            hostname,
            subnet,
        ),
        send_node_did: entry.send_node_did,
    })
}
