use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use chrono::Utc;
use gents::documents::ToolServiceRegistry;
use gents::store::graphql::escape_graphql_string;
use gents::{
    run_health_check_cycle_for_actor, HealthCheckerOptions, McpHealthCheckService, McpPool,
    ServiceHealthMap,
};
use gents_desktop_core::client::ClientCore;
use gents_protocol::row::ToolServiceHealthStateRow;
use gents_protocol::tool_service_health::ToolServiceHealthState;

use super::super::types::{MCPServiceHealthView, McpServiceProbeResult};

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
    let registry_entry = load_registry_entry(core, node_did, service_id).await?;
    let expected_entry = registry_entry.clone();
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
    anyhow::ensure!(
        load_registry_entry(core, node_did, service_id).await? == expected_entry,
        "MCP service changed before the probe was sent"
    );
    let cycle = run_health_check_cycle_for_actor(
        vec![service],
        Utc::now(),
        &pool,
        &health_map,
        &local_hostname,
        None,
        &options,
        core.node_identity().did(),
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
    let response = core.operator_access(node_did)?.execute(&query).await?;
    let row = response
        .pointer("/data/ToolServiceRegistry")
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
        let (core, runtime, _directory) = crate::tests::support::runtime_core().await;
        let (other_core, other_runtime, _other_directory) =
            crate::tests::support::runtime_core().await;
        let first = runtime.node_identity_did().unwrap();
        let second = other_runtime.node_identity_did().unwrap();
        let endpoint = other_core.operator_graphql(second).unwrap();
        core.add_local_standard_peer_route_for_test(
            "Other runtime",
            "127.0.0.1:56002/p2p/6fe391e1c69d66de633034ca40cda6d39ca1a3c94792f2f510add7d1421ea7bb",
            second,
            endpoint.url(),
            "/tmp/mcp-test-other",
        )
        .await?;
        for (node, hostname) in [(first, "first"), (second, "second")] {
            use gents::store::config_client::{
                apply_desired_state_plan, DesiredStateApplyDocument, DesiredStateApplyPlan,
            };
            let value = serde_json::json!({"node_did":node,"service_id":"shared","hostname":hostname,"enabled":true});
            let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
                collection: gents::Collection::ToolServiceRegistry,
                add: value.clone(),
                update: value,
            }])?;
            core.operator_access(node)?
                .transact_write("test.mcp_probe_scope", |txn| {
                    let plan = &plan;
                    Box::pin(async move { apply_desired_state_plan(txn, plan).await })
                })
                .await?;
            let row = load_registry_entry(&core, node, "shared").await?;
            assert_eq!(row.node_did, node);
            assert_eq!(row.hostname.as_deref(), Some(hostname));
        }
        assert!(load_registry_entry(&core, "did:test:missing", "shared")
            .await
            .is_err());
        core.shutdown().await?;
        other_core.shutdown().await?;
        runtime.shutdown().await.unwrap();
        other_runtime.shutdown().await.unwrap();
        Ok(())
    }
}
