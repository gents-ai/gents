//! Shared helpers for parsing `ToolServiceRegistry` rows.
//!
//! The registry schema allows nullable address fields (`hostname`,
//! `tailscale_ip`, `lan_ip`, `mcp_path`). Default serde behavior rejects
//! explicit JSON `null` when deserializing into `String`, so consumers
//! that model these fields as `String` need a null-tolerant deserializer.

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

#[cfg(test)]
mod tests;

/// Read principal-local service configuration, independently of health observations.
/// Duplicate logical names fail closed rather than selecting an arbitrary route.
pub(crate) async fn configured_mcp_services(
    node: &defra_node::EmbeddedNode,
    agent_did: &str,
) -> anyhow::Result<Vec<crate::document_config::ToolServiceRegistry>> {
    anyhow::ensure!(
        !agent_did.trim().is_empty(),
        "MCP registry owner is required"
    );
    let (fields, _) =
        crate::config_client::config_projection(crate::Collection::ToolServiceRegistry, None)?;
    let owner = crate::graphql::escape_graphql_string(agent_did);
    let query = format!(
        "{{ ToolServiceRegistry(filter: {{ agent_did: {{ _eq: \"{owner}\" }} }}) {{ {} }} }}",
        fields.join(" ")
    );
    let response = node.execute(&query).await;
    anyhow::ensure!(
        !response.has_errors(),
        "MCP registry query failed: {:?}",
        response.errors
    );
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("ToolServiceRegistry"))
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("MCP registry query returned no rows array"))?;
    let mut names = std::collections::HashSet::new();
    let mut services = Vec::with_capacity(rows.len());
    for row in rows {
        let service: crate::document_config::ToolServiceRegistry =
            serde_json::from_value(row.clone())?;
        anyhow::ensure!(
            service.agent_did == agent_did && !service.service_id.trim().is_empty(),
            "MCP registry returned an invalid scoped identity"
        );
        anyhow::ensure!(
            names.insert(service.service_id.clone()),
            "duplicate MCP service {} for principal {agent_did}",
            service.service_id
        );
        services.push(service);
    }
    Ok(services)
}
