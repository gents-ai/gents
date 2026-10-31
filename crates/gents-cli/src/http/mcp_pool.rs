use gents::store::config_client::GraphqlEndpoint;
use std::collections::BTreeMap;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use gents::store::graphql::escape_graphql_string;
use gents_protocol::row::{ToolServiceHealthStateRow, ToolServiceRegistryRow};
use gents_protocol::tool_service_health::{ToolServiceHealthProjection, ToolServiceHealthState};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::post_graphql;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct McpPoolSnapshot {
    pub(crate) generated_at: String,
    pub(crate) node_did: String,
    pub(crate) totals: McpPoolTotals,
    pub(crate) services: Vec<McpPoolService>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct McpPoolTotals {
    pub(crate) registered: usize,
    pub(crate) online: usize,
    pub(crate) healthy: usize,
    pub(crate) degraded: usize,
    pub(crate) unreachable: usize,
    pub(crate) unknown: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct McpPoolService {
    pub(crate) service_id: String,
    pub(crate) display_name: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) hostname: Option<String>,
    pub(crate) tailscale_ip: Option<String>,
    pub(crate) lan_ip: Option<String>,
    pub(crate) mcp_port: Option<i64>,
    pub(crate) mcp_path: Option<String>,
    pub(crate) send_node_did: bool,
    pub(crate) status: Option<String>,
    pub(crate) version: Option<String>,
    pub(crate) updated_at: Option<String>,
    pub(crate) health_status: Option<String>,
    pub(crate) tool_count: Option<i64>,
    pub(crate) endpoint: Option<String>,
    pub(crate) failure_count: Option<i64>,
    pub(crate) k_max: Option<i64>,
    pub(crate) backoff_until: Option<String>,
    pub(crate) last_probe_at: Option<String>,
    pub(crate) last_seen: Option<String>,
    pub(crate) last_error_class: Option<String>,
    pub(crate) last_error_message: Option<String>,
    pub(crate) health_updated_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct McpPoolEnvelope {
    #[serde(rename = "ToolServiceRegistry", default)]
    registry: Vec<ToolServiceRegistryRow>,
    #[serde(rename = "ToolServiceHealthState", default)]
    health: Vec<ToolServiceHealthStateRow>,
}

pub(crate) async fn load_mcp_pool_snapshot(
    graphql: &GraphqlEndpoint,
    node_did: &str,
) -> Result<McpPoolSnapshot> {
    let generated_at = Utc::now();
    let response = post_graphql(graphql, &mcp_pool_query(node_did)).await?;
    let envelope = decode_mcp_pool_response(response)?;
    Ok(build_mcp_pool_snapshot(
        generated_at,
        node_did.to_string(),
        envelope,
    ))
}

fn mcp_pool_query(node_did: &str) -> String {
    let node_did = escape_graphql_string(node_did);
    format!(
        r#"{{
            ToolServiceRegistry(order: {{ service_id: ASC }}) {{
                service_id
                display_name
                description
                hostname
                tailscale_ip
                lan_ip
                mcp_port
                mcp_path
                send_node_did
                status
                version
                updated_at
            }}
            ToolServiceHealthState(
                filter: {{ node_did: {{ _eq: "{node_did}" }} }},
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
    )
}

fn decode_mcp_pool_response(response: Value) -> Result<McpPoolEnvelope> {
    let data = response
        .get("data")
        .filter(|data| data.is_object())
        .cloned()
        .with_context(|| format!("mcp pool query response missing object data: {response}"))?;
    serde_json::from_value(data).context("decoding mcp pool query response")
}

fn build_mcp_pool_snapshot(
    generated_at: DateTime<Utc>,
    node_did: String,
    envelope: McpPoolEnvelope,
) -> McpPoolSnapshot {
    // Twins of one service are not schema-unique; the query order puts the
    // health checker's chosen row first.
    let mut health_by_service = BTreeMap::new();
    for row in envelope.health {
        health_by_service
            .entry(row.service_id.clone())
            .or_insert(row);
    }
    let mut totals = McpPoolTotals::default();
    let mut services = Vec::new();

    for registry in envelope.registry {
        let service_id = registry.service_id.trim().to_string();
        if service_id.is_empty() {
            continue;
        }
        totals.registered += 1;
        if status_eq(registry.status.as_deref(), "online") {
            totals.online += 1;
        }

        let health = health_by_service.remove(&service_id);
        count_health_status(
            &mut totals,
            health.as_ref().and_then(|row| row.status.as_deref()),
        );

        services.push(McpPoolService {
            service_id,
            display_name: registry.display_name,
            description: registry.description,
            hostname: registry.hostname,
            tailscale_ip: registry.tailscale_ip,
            lan_ip: registry.lan_ip,
            mcp_port: registry.mcp_port,
            mcp_path: registry.mcp_path,
            send_node_did: registry.send_node_did,
            status: registry.status,
            version: registry.version,
            updated_at: registry.updated_at,
            health_status: health.as_ref().and_then(|row| row.status.clone()),
            tool_count: health.as_ref().and_then(|row| row.tool_count),
            endpoint: health.as_ref().and_then(|row| row.endpoint.clone()),
            failure_count: health.as_ref().and_then(|row| row.failure_count),
            k_max: health.as_ref().and_then(|row| row.k_max),
            backoff_until: health.as_ref().and_then(|row| row.backoff_until.clone()),
            last_probe_at: health.as_ref().and_then(|row| row.last_probe_at.clone()),
            last_seen: health.as_ref().and_then(|row| row.last_seen.clone()),
            last_error_class: health.as_ref().and_then(|row| row.last_error_class.clone()),
            last_error_message: health
                .as_ref()
                .and_then(|row| row.last_error_message.clone()),
            health_updated_at: health.as_ref().and_then(|row| row.updated_at.clone()),
        });
    }

    McpPoolSnapshot {
        generated_at: generated_at.to_rfc3339(),
        node_did,
        totals,
        services,
    }
}

fn status_eq(value: Option<&str>, expected: &str) -> bool {
    value
        .map(str::trim)
        .is_some_and(|value| value.eq_ignore_ascii_case(expected))
}

fn count_health_status(totals: &mut McpPoolTotals, status: Option<&str>) {
    match ToolServiceHealthState::parse_opt(status).map(ToolServiceHealthState::project) {
        Some(ToolServiceHealthProjection::Healthy) => totals.healthy += 1,
        Some(ToolServiceHealthProjection::Stale) => totals.degraded += 1,
        Some(ToolServiceHealthProjection::Unreachable) => totals.unreachable += 1,
        None => totals.unknown += 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn twin_health_rows_resolve_to_the_first_in_query_order() {
        let envelope: McpPoolEnvelope = serde_json::from_value(serde_json::json!({
            "ToolServiceRegistry": [{"service_id": "obs-mcp", "send_node_did": false}],
            "ToolServiceHealthState": [
                {"service_id": "obs-mcp", "status": "healthy", "updated_at": "2026-06-05T00:01:00Z"},
                {"service_id": "obs-mcp", "status": "evicted", "updated_at": "2026-06-05T00:00:00Z"}
            ]
        }))
        .unwrap();
        let generated_at = DateTime::parse_from_rfc3339("2026-06-05T00:02:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let snapshot =
            build_mcp_pool_snapshot(generated_at, "did:key:zAgent".to_string(), envelope);
        assert_eq!(
            snapshot.services[0].health_status.as_deref(),
            Some("healthy")
        );
        assert!(mcp_pool_query("did:key:zAgent")
            .contains("[{ service_id: ASC }, { updated_at: DESC }, { _docID: ASC }]"));
    }
}
