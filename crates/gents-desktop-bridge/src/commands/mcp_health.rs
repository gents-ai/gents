use anyhow::{anyhow, bail, Result};
use gents_desktop_core::client::ClientCore;
use gents_protocol::row::ToolServiceHealthStateRow;
use gents_protocol::tool_service_health::ToolServiceHealthState;
use gents_store::graphql::escape_graphql_string;
use gents_tools::mcp::pool::McpPool;

use super::super::types::{MCPServiceHealthView, McpServiceProbeResult};

/// The MCP services of the caller's node; a caller without a node has none.
pub async fn load_mcp_services_with_health(
    core: &ClientCore,
    node_did: Option<&str>,
) -> Result<Vec<MCPServiceHealthView>> {
    let Some(node_did) = node_did else {
        return Ok(Vec::new());
    };
    load_mcp_services_with_health_for_node(core, node_did).await
}

pub(crate) async fn load_mcp_services_with_health_for_node(
    core: &ClientCore,
    node_did: &str,
) -> Result<Vec<MCPServiceHealthView>> {
    let escaped_node = escape_graphql_string(node_did);
    let query = format!(
        r#"{{
            ToolServiceHealthState(
                filter: {{ node_did: {{ _eq: "{escaped_node}" }} }},
                order: [{{ service_id: ASC }}, {{ updated_at: DESC }}, {{ _docID: ASC }}]
            ) {{
                service_id
                node_did
                endpoint
                status
                tool_count
                failure_count
                k_max
                backoff_until
                last_probe_at
                last_seen
                last_error_class
                last_error_message
                updated_at
            }}
        }}"#
    );

    let response = core.operator_access(node_did)?.execute(&query).await?;

    let raw = response
        .pointer("/data/ToolServiceHealthState")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();

    let mut rows: Vec<ToolServiceHealthStateRow> = raw
        .into_iter()
        .map(serde_json::from_value)
        .collect::<std::result::Result<_, _>>()
        .map_err(|error| anyhow!("parsing ToolServiceHealthState rows: {error}"))?;
    // Twins of one service are not schema-unique; the order puts the
    // health checker's chosen row first.
    rows.dedup_by(|later, first| later.service_id == first.service_id);

    rows.into_iter().map(view_from_row).collect()
}

pub(crate) fn view_from_row(row: ToolServiceHealthStateRow) -> Result<MCPServiceHealthView> {
    let display_state = ToolServiceHealthState::parse_opt(row.status.as_deref())
        .ok_or_else(|| {
            anyhow!(
                "ToolServiceHealthState row for service_id={:?} has missing/unrecognized status {:?}",
                row.service_id,
                row.status
            )
        })?
        .project()
        .as_str()
        .to_string();

    Ok(MCPServiceHealthView {
        service_id: row.service_id,
        node_did: row.node_did,
        endpoint: row.endpoint,
        status: row.status,
        display_state,
        tool_count: row.tool_count,
        failure_count: row.failure_count,
        k_max: row.k_max,
        backoff_until: row.backoff_until,
        last_probe_at: row.last_probe_at,
        last_seen: row.last_seen,
        last_error_class: row.last_error_class,
        last_error_message: row.last_error_message,
        updated_at: row.updated_at,
    })
}

pub async fn probe_mcp_service(
    core: &ClientCore,
    node_did: &str,
    service_id: &str,
) -> Result<McpServiceProbeResult> {
    let node_did = node_did.trim();
    if node_did.is_empty() {
        bail!("node_did must not be empty");
    }
    let service_id = service_id.trim();
    if service_id.is_empty() {
        bail!("service_id must not be empty");
    }
    let local_hostname = hostname::get()
        .map(|host| host.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    let receipt = gents_tools::mcp::probe::probe(
        &core.operator_access(node_did)?,
        node_did,
        service_id,
        core.node_identity().did(),
        gents_tools::mcp::probe::default_timeout(),
        &local_hostname,
        None,
        &McpPool::new(),
    )
    .await;
    Ok(McpServiceProbeResult {
        service_id: receipt.service_id,
        status: receipt.health_state,
        latency_ms: receipt.latency_ms,
        last_error: receipt.last_error,
    })
}
