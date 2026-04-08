use std::sync::Arc;

use anyhow::{anyhow, Context as _};
use defra_node::EmbeddedNode;
use serde::Deserialize;

use crate::health_checker::{HealthStatus, ServiceHealth, ServiceHealthMap};
use crate::mcp_pool::resolve_mcp_url;
use crate::mcp_pool::McpPool;

#[derive(Clone)]
pub struct MetaToolContext {
    pub node: Arc<EmbeddedNode>,
    pub mcp_pool: McpPool,
    pub health: ServiceHealthMap,
    pub local_hostname: String,
    pub local_subnet: Option<String>,
}

#[derive(Debug)]
pub struct MetaToolError(anyhow::Error);

impl std::fmt::Display for MetaToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for MetaToolError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.root_cause())
    }
}

impl From<anyhow::Error> for MetaToolError {
    fn from(error: anyhow::Error) -> Self {
        Self(error)
    }
}

pub(super) fn escape_graphql(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

#[derive(Debug, Clone, Deserialize)]
struct RegistryServiceEntry {
    #[serde(default)]
    hostname: String,
    #[serde(default)]
    tailscale_ip: String,
    #[serde(default)]
    lan_ip: String,
    mcp_port: Option<u16>,
    #[serde(default = "default_mcp_path")]
    mcp_path: String,
}

fn default_mcp_path() -> String {
    "/mcp".to_string()
}

pub(super) fn lookup_service_query(service_id: &str) -> String {
    let sid = escape_graphql(service_id);
    format!(
        r#"{{
  ToolServiceRegistry(
    filter: {{
      service_id: {{ _eq: "{sid}" }},
      status: {{ _eq: "online" }}
    }},
    order: {{ updated_at: DESC }},
    limit: 1
  ) {{
    service_id
    display_name
    description
    hostname
    tailscale_ip
    lan_ip
    mcp_port
    mcp_path
  }}
}}"#
    )
}

pub(super) async fn lookup_service(
    ctx: &MetaToolContext,
    service_id: &str,
) -> anyhow::Result<String> {
    let resp = ctx.node.execute(&lookup_service_query(service_id)).await;
    if resp.has_errors() {
        anyhow::bail!("lookup_service({service_id}): {:?}", resp.errors);
    }

    let entry = resp
        .data
        .as_ref()
        .and_then(|d| d.get("ToolServiceRegistry"))
        .cloned()
        .map(serde_json::from_value::<Vec<RegistryServiceEntry>>)
        .transpose()
        .context("parsing ToolServiceRegistry response")?
        .and_then(|mut entries| entries.drain(..).next())
        .ok_or_else(|| anyhow!("service '{service_id}' not found or offline"))?;

    let mcp_port = entry
        .mcp_port
        .filter(|port| *port != 0)
        .ok_or_else(|| anyhow!("service '{service_id}' is missing mcp_port in the registry"))?;

    if entry.hostname.is_empty() && entry.tailscale_ip.is_empty() && entry.lan_ip.is_empty() {
        return Err(anyhow!(
            "service '{service_id}' is missing hostname/tailscale_ip/lan_ip in the registry"
        ));
    }

    let endpoint = resolve_mcp_url(
        &entry.hostname,
        &entry.tailscale_ip,
        &entry.lan_ip,
        mcp_port,
        &entry.mcp_path,
        &ctx.local_hostname,
        ctx.local_subnet.as_deref(),
    );

    Ok(endpoint)
}

pub(super) fn extract_text(result: &rmcp::model::CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|c| c.raw.as_text().map(|t| t.text.as_str()))
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_elapsed(last_seen: chrono::DateTime<chrono::Utc>) -> String {
    let seconds = chrono::Utc::now()
        .signed_duration_since(last_seen)
        .num_seconds()
        .max(0);

    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h", seconds / 3600)
    } else {
        format!("{}d", seconds / 86_400)
    }
}

pub(super) fn format_health_status(health: Option<&ServiceHealth>) -> String {
    match health {
        Some(health) => match (&health.status, &health.last_error) {
            (HealthStatus::Unreachable, Some(error)) => format!(
                "{} (last seen {} ago, error: {})",
                health.status,
                format_elapsed(health.last_seen),
                error
            ),
            _ => format!(
                "{} (last seen {} ago)",
                health.status,
                format_elapsed(health.last_seen)
            ),
        },
        None => "unknown (awaiting first health check)".to_string(),
    }
}

pub(super) async fn enforce_health_gate(
    health_map: &ServiceHealthMap,
    service_id: &str,
) -> anyhow::Result<Option<ServiceHealth>> {
    let health = health_map.get(service_id).await;
    if let Some(health) = &health {
        match health.status {
            HealthStatus::Unreachable => {
                let suffix = health
                    .last_error
                    .as_deref()
                    .map(|error| format!(" (last error: {error})"))
                    .unwrap_or_default();
                anyhow::bail!("service '{service_id}' is currently unreachable{suffix}");
            }
            HealthStatus::Stale => {
                tracing::warn!(
                    service_id = %service_id,
                    last_seen = %health.last_seen,
                    "service heartbeat is stale; attempting tool request anyway"
                );
            }
            HealthStatus::Healthy => {}
        }
    }

    Ok(health)
}
