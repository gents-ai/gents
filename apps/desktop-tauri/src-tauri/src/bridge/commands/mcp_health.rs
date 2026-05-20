use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use chrono::Utc;
use defra_agent::graphql::escape_graphql_string;
use defra_agent::{
    run_health_check_cycle, HealthCheckerOptions, McpHealthCheckService, McpPool,
    ServiceHealthMap,
};
use defra_agent_desktop_core::client::ClientCore;
use defra_agent_protocol::row::{ToolServiceHealthStateRow, ToolServiceRegistryRow};

use super::super::types::{MCPServiceHealthView, McpServiceProbeResult};

/// Read every persisted `ToolServiceHealthState` row from the local
/// DefraDB node. Bridges directly to the GraphQL store rather than the
/// in-memory `ServiceHealthMap` because the agent runtime (and therefore
/// the in-memory state) lives in a separate process — the persisted
/// collection is the only path the desktop has to the K-model state.
///
/// Each returned `MCPServiceHealthView` represents a single MCP service's
/// most recent probe result; the agent's health checker rewrites the row
/// every `cycle_interval` (30 s by default) per the design in
/// `Proofs/MCPHealth/{State,Transition}.lean`.
pub(crate) async fn load_mcp_services_with_health(
    core: &ClientCore,
) -> Result<Vec<MCPServiceHealthView>> {
    let query = r#"{
        ToolServiceHealthState(order: { service_id: ASC }) {
            service_id
            agent_did
            endpoint
            status
            failure_count
            k_max
            backoff_until
            last_probe_at
            last_seen
            last_error_class
            last_error_message
            updated_at
        }
    }"#;

    let response = core.node().execute(query).await;
    if response.has_errors() {
        bail!(
            "list_mcp_services_with_health query failed: {:?}",
            response.errors
        );
    }

    let raw = response
        .data
        .as_ref()
        .and_then(|data| data.get("ToolServiceHealthState"))
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();

    let rows: Vec<ToolServiceHealthStateRow> = raw
        .into_iter()
        .map(serde_json::from_value)
        .collect::<std::result::Result<_, _>>()
        .map_err(|error| anyhow!("parsing ToolServiceHealthState rows: {error}"))?;

    Ok(rows.into_iter().map(view_from_row).collect())
}

fn view_from_row(row: ToolServiceHealthStateRow) -> MCPServiceHealthView {
    MCPServiceHealthView {
        service_id: row.service_id,
        agent_did: row.agent_did,
        endpoint: row.endpoint,
        status: row.status,
        failure_count: row.failure_count,
        k_max: row.k_max,
        backoff_until: row.backoff_until,
        last_probe_at: row.last_probe_at,
        last_seen: row.last_seen,
        last_error_class: row.last_error_class,
        last_error_message: row.last_error_message,
        updated_at: row.updated_at,
    }
}

/// One-shot probe of a single registered MCP service. Mirrors
/// `defra-agent mcp probe` (see `crates/defra-agent-cli/src/commands/mcp.rs`):
/// reads the `ToolServiceRegistry` entry, runs `run_health_check_cycle`
/// once against a fresh `McpPool` + `ServiceHealthMap`, and reports the
/// snapshot.
///
/// The returned `failure_count` is always 0 because the cycle starts from
/// `ServiceModel::initial` — for accumulated K-state across many probe
/// cycles, the desktop reads the persisted `ToolServiceHealthState` row
/// via `load_mcp_services_with_health`.
pub(crate) async fn probe_mcp_service(
    core: &ClientCore,
    service_id: &str,
) -> Result<McpServiceProbeResult> {
    let service_id = service_id.trim();
    if service_id.is_empty() {
        bail!("service_id must not be empty");
    }
    let registry_entry = load_registry_entry(core, service_id).await?;
    let service = McpHealthCheckService {
        service_id: registry_entry.service_id.clone(),
        hostname: registry_entry.hostname.unwrap_or_default(),
        tailscale_ip: registry_entry.tailscale_ip.unwrap_or_default(),
        lan_ip: registry_entry.lan_ip.unwrap_or_default(),
        mcp_port: registry_entry
            .mcp_port
            .and_then(|port| u16::try_from(port).ok()),
        mcp_path: registry_entry.mcp_path.unwrap_or_default(),
        updated_at: registry_entry.updated_at,
    };
    let health_map = ServiceHealthMap::new();
    let pool = McpPool::new();
    let started = std::time::Instant::now();
    let options = HealthCheckerOptions::default();
    let timeout = options.probe_timeout * 2;
    let cycle = run_health_check_cycle(
        vec![service],
        Utc::now(),
        &pool,
        &health_map,
        "",
        None,
        &options,
        None,
    );
    let result = tokio::time::timeout(timeout, cycle).await;
    let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

    match result {
        Ok(Ok(())) => match health_map.get(service_id).await {
            Some(health) => Ok(McpServiceProbeResult {
                service_id: service_id.to_string(),
                status: health.status.to_string(),
                latency_ms,
                last_error: health.last_error,
            }),
            None => Ok(McpServiceProbeResult {
                service_id: service_id.to_string(),
                status: "unreachable".to_string(),
                latency_ms,
                last_error: Some("probe produced no health snapshot".to_string()),
            }),
        },
        Ok(Err(error)) => Ok(McpServiceProbeResult {
            service_id: service_id.to_string(),
            status: "unreachable".to_string(),
            latency_ms,
            last_error: Some(error.to_string()),
        }),
        Err(_) => Ok(McpServiceProbeResult {
            service_id: service_id.to_string(),
            status: "unreachable".to_string(),
            latency_ms,
            last_error: Some(format!(
                "probe timed out after {}ms",
                Duration::from_millis(latency_ms).as_millis()
            )),
        }),
    }
}

async fn load_registry_entry(core: &ClientCore, service_id: &str) -> Result<ToolServiceRegistryRow> {
    let escaped = escape_graphql_string(service_id);
    let query = format!(
        r#"{{
            ToolServiceRegistry(
                filter: {{ service_id: {{ _eq: "{escaped}" }} }},
                limit: 1
            ) {{
                service_id
                hostname
                tailscale_ip
                lan_ip
                mcp_port
                mcp_path
                status
                updated_at
            }}
        }}"#
    );
    let response = core.node().execute(&query).await;
    if response.has_errors() {
        bail!(
            "probe_mcp_service registry query failed: {:?}",
            response.errors
        );
    }
    let row = response
        .data
        .as_ref()
        .and_then(|data| data.get("ToolServiceRegistry"))
        .and_then(|rows| rows.as_array())
        .and_then(|rows| rows.first())
        .cloned()
        .ok_or_else(|| anyhow!("no ToolServiceRegistry row for service_id={service_id}"))?;
    serde_json::from_value(row).map_err(Into::into)
}
