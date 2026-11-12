use gents_store::config_client::GraphqlEndpoint;
use std::collections::BTreeMap;

use anyhow::Result;
use chrono::{DateTime, Utc};
use gents_protocol::row::{ToolServiceHealthStateRow, ToolServiceRegistryRow};
use gents_protocol::tool_service_health::{ToolServiceHealthProjection, ToolServiceHealthState};
use serde::{Deserialize, Serialize};

use gents_store::config_client::ConfigAccess;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpPoolSnapshot {
    pub generated_at: String,
    pub node_did: String,
    pub totals: McpPoolTotals,
    pub services: Vec<McpPoolService>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpPoolTotals {
    pub registered: usize,
    pub online: usize,
    pub healthy: usize,
    pub degraded: usize,
    pub unreachable: usize,
    pub unknown: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpPoolService {
    pub service_id: String,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub hostname: Option<String>,
    pub tailscale_ip: Option<String>,
    pub lan_ip: Option<String>,
    pub mcp_port: Option<i64>,
    pub mcp_path: Option<String>,
    pub endpoint_url: Option<String>,
    pub send_node_did: bool,
    pub status: Option<String>,
    pub version: Option<String>,
    pub updated_at: Option<String>,
    pub health_status: Option<String>,
    pub tool_count: Option<i64>,
    pub endpoint: Option<String>,
    pub failure_count: Option<i64>,
    pub k_max: Option<i64>,
    pub backoff_until: Option<String>,
    pub last_probe_at: Option<String>,
    pub last_seen: Option<String>,
    pub last_error_class: Option<String>,
    pub last_error_message: Option<String>,
    pub health_updated_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct McpPoolEnvelope {
    #[serde(rename = "ToolServiceRegistry", default)]
    registry: Vec<ToolServiceRegistryRow>,
    #[serde(rename = "ToolServiceHealthState", default)]
    health: Vec<ToolServiceHealthStateRow>,
}

pub async fn load_mcp_pool_snapshot(
    graphql: &GraphqlEndpoint,
    node_did: &str,
) -> Result<McpPoolSnapshot> {
    let generated_at = Utc::now();
    let access = ConfigAccess::Graphql(graphql.clone());
    let (registry, health) = access.read_views(&(
        gents_tools::mcp::views::ServiceInventory {
            node_did,
            service_id: None,
            enabled_only: false,
        },
        gents_tools::mcp::views::Health { node_did },
    )).await?;
    let envelope = McpPoolEnvelope {
        registry: registry.into_iter().map(|row| {
            let config = row.config;
            ToolServiceRegistryRow {
                service_id: config.service_id,
                enabled: Some(config.enabled),
                display_name: config.display_name,
                description: config.description,
                hostname: config.hostname,
                tailscale_ip: config.tailscale_ip,
                lan_ip: config.lan_ip,
                mcp_port: config.mcp_port,
                mcp_path: config.mcp_path,
                endpoint_url: config.endpoint_url,
                send_node_did: config.send_node_did,
                tools: Vec::new(),
                status: row.status,
                version: row.version,
                updated_at: row.updated_at,
            }
        }).collect(),
        health,
    };
    Ok(build_mcp_pool_snapshot(
        generated_at,
        node_did.to_string(),
        envelope,
    ))
}

fn build_mcp_pool_snapshot(
    generated_at: DateTime<Utc>,
    node_did: String,
    envelope: McpPoolEnvelope,
) -> McpPoolSnapshot {
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
        let health = health_by_service.remove(&service_id);
        if registry.enabled != Some(false)
            && matches!(
                health
                    .as_ref()
                    .and_then(|row| ToolServiceHealthState::parse_opt(row.status.as_deref()))
                    .map(ToolServiceHealthState::project),
                Some(ToolServiceHealthProjection::Healthy)
            )
        {
            totals.online += 1;
        }
        count_health_status(
            &mut totals,
            if registry.enabled == Some(false) {
                None
            } else {
                health.as_ref().and_then(|row| row.status.as_deref())
            },
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
            endpoint_url: registry.endpoint_url,
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
        assert_eq!(snapshot.totals.online, 1);

    }

    #[test]
    fn disabled_or_unmeasured_services_are_not_reported_online() {
        let envelope: McpPoolEnvelope = serde_json::from_value(serde_json::json!({
            "ToolServiceRegistry": [
                {"service_id":"disabled", "enabled":false, "status":"online"},
                {"service_id":"unmeasured", "enabled":true, "status":"online"}
            ],
            "ToolServiceHealthState": [{"service_id":"disabled", "status":"healthy"}]
        }))
        .unwrap();
        let snapshot = build_mcp_pool_snapshot(Utc::now(), "node".into(), envelope);
        assert_eq!(snapshot.totals.online, 0);
    }
}
