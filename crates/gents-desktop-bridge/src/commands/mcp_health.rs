use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use chrono::Utc;
use gents::document_config::ToolServiceRegistry;
use gents::graphql::{escape_graphql_string, graphql_with_transaction_retry};
use gents::{
    run_health_check_cycle, HealthCheckerOptions, McpHealthCheckService, McpPool, ServiceHealthMap,
};
use gents_desktop_core::client::ClientCore;
use gents_protocol::row::ToolServiceHealthStateRow;
use gents_protocol::tool_service_health::ToolServiceHealthState;

use super::super::types::{MCPServiceHealthView, McpServiceProbeResult};

/// Read every persisted `ToolServiceHealthState` row scoped to the
/// desktop's currently selected node. Bridges directly to the GraphQL
/// store rather than the in-memory `ServiceHealthMap` because the node
/// runtime (and therefore the in-memory state) lives in a separate
/// process — the persisted collection is the only path the desktop has
/// to the K-model state.
///
/// Rows are written by a `node_did`; on a replicated desktop node the local
/// DefraDB sees rows from every replicated node. The selected-node
/// filter keeps the rail's view consistent with the rest of the desktop
/// (the same `selected_node_did` scopes config, transcripts, and
/// triggers). Returns an empty Vec when no node is selected — the rail
/// renders the existing empty state.
pub async fn load_mcp_services_with_health(core: &ClientCore) -> Result<Vec<MCPServiceHealthView>> {
    let Some(node_did) = core.selected_node_did() else {
        return Ok(Vec::new());
    };
    load_mcp_services_with_health_for_node(core, &node_did).await
}

pub(crate) async fn load_mcp_services_with_health_for_node(
    core: &ClientCore,
    node_did: &str,
) -> Result<Vec<MCPServiceHealthView>> {
    let escaped_node = escape_graphql_string(&node_did);
    let query = format!(
        r#"{{
            ToolServiceHealthState(
                filter: {{ node_did: {{ _eq: "{escaped_node}" }} }},
                order: {{ service_id: ASC }}
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

    let response =
        graphql_with_transaction_retry(&core.node(), &query, "list_mcp_services_with_health query")
            .await?;

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

    rows.into_iter().map(view_from_row).collect()
}

/// `display_state` is the collapsed three-state projection
/// (`ToolServiceHealthState::project`) of the persisted `status` column.
/// A row with a missing or unrecognized `status` is a data error — this
/// command surfaces it as `Err` rather than inventing a synthetic
/// classification, so a corrupt/legacy row fails loudly instead of
/// silently rendering as healthy.
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
    let registry_entry = load_registry_entry(core, node_did, service_id).await?;
    let service = McpHealthCheckService {
        service_id: registry_entry.service_id.clone(),
        hostname: registry_entry.hostname.unwrap_or_default(),
        tailscale_ip: registry_entry.tailscale_ip.unwrap_or_default(),
        lan_ip: registry_entry.lan_ip.unwrap_or_default(),
        mcp_port: registry_entry
            .mcp_port
            .and_then(|port| u16::try_from(port).ok()),
        mcp_path: registry_entry.mcp_path.unwrap_or_default(),
        send_node_did: registry_entry.send_node_did,
        updated_at: None,
    };
    let health_map = ServiceHealthMap::new();
    let pool = McpPool::new();
    let started = std::time::Instant::now();
    let options = one_shot_probe_options();
    let timeout = options.probe_timeout * 2;
    let local_hostname = hostname::get()
        .map(|host| host.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    let cycle = run_health_check_cycle(
        vec![service],
        Utc::now(),
        &pool,
        &health_map,
        &local_hostname,
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

fn one_shot_probe_options() -> HealthCheckerOptions {
    HealthCheckerOptions {
        failure_threshold_k: 1,
        ..HealthCheckerOptions::default()
    }
}

async fn load_registry_entry(
    core: &ClientCore,
    node_did: &str,
    service_id: &str,
) -> Result<ToolServiceRegistry> {
    let escaped = escape_graphql_string(service_id);
    let escaped_node = escape_graphql_string(node_did);
    let query = format!(
        r#"{{
            ToolServiceRegistry(
                filter: {{ _and: [
                    {{ node_did: {{ _eq: "{escaped_node}" }} }},
                    {{ enabled: {{ _ne: false }} }},
                    {{ service_id: {{ _eq: "{escaped}" }} }}
                ] }},
                limit: 1
            ) {{
                node_did
                service_id
                hostname
                tailscale_ip
                lan_ip
                mcp_port
                mcp_path
                send_node_did
                enabled
            }}
        }}"#
    );
    let response =
        graphql_with_transaction_retry(&core.node(), &query, "probe_mcp_service registry query")
            .await?;
    let row = response
        .data
        .as_ref()
        .and_then(|data| data.get("ToolServiceRegistry"))
        .and_then(|rows| rows.as_array())
        .and_then(|rows| rows.first())
        .cloned()
        .ok_or_else(|| anyhow!("no enabled ToolServiceRegistry row for service_id={service_id}"))?;
    serde_json::from_value(row).map_err(Into::into)
}

#[cfg(test)]
mod probe_scope_tests {
    use super::*;

    #[tokio::test]
    async fn registry_probe_resolves_the_node_and_service_compound_key() -> Result<()> {
        let (core, _tempdir) = crate::tests::support::boot_core().await;
        gents::config_client::ConfigAccess::write_local(
            &core.node(),
            "test.mcp_probe_scope",
            r#"mutation {
                create_ToolServiceRegistry(input: {
                    node_did: "did:test:first", service_id: "shared", hostname: "first", enabled: true
                }) { _docID }
                create_ToolServiceRegistry(input: {
                    node_did: "did:test:second", service_id: "shared", hostname: "second", enabled: true
                }) { _docID }
            }"#,
        ).await?;
        for (node, hostname) in [("did:test:first", "first"), ("did:test:second", "second")] {
            let row = load_registry_entry(&core, node, "shared").await?;
            assert_eq!(row.node_did, node);
            assert_eq!(row.hostname.as_deref(), Some(hostname));
        }
        assert!(load_registry_entry(&core, "did:test:missing", "shared")
            .await
            .is_err());
        core.shutdown().await?;
        Ok(())
    }
}
