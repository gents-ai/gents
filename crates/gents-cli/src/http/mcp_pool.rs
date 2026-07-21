//! `/mcp/pool` surfacing: registered MCP services plus this agent's
//! persisted health-probe view. This is an operator-oriented projection over
//! `ToolServiceRegistry` and `ToolServiceHealthState`; it does not call remote
//! MCP servers itself.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use defra_agent_protocol::graphql::escape_graphql_string;
use defra_agent_protocol::row::{ToolServiceHealthStateRow, ToolServiceRegistryRow};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::post_graphql;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct McpPoolSnapshot {
    pub(crate) generated_at: String,
    pub(crate) agent_did: String,
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
    pub(crate) send_agent_did: bool,
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
    graphql: &str,
    agent_did: &str,
) -> Result<McpPoolSnapshot> {
    let generated_at = Utc::now();
    let response = post_graphql(graphql, &mcp_pool_query(agent_did)).await?;
    let envelope = decode_mcp_pool_response(response)?;
    Ok(build_mcp_pool_snapshot(
        generated_at,
        agent_did.to_string(),
        envelope,
    ))
}

fn mcp_pool_query(agent_did: &str) -> String {
    let agent_did = escape_graphql_string(agent_did);
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
                send_agent_did
                status
                version
                updated_at
            }}
            ToolServiceHealthState(
                filter: {{ agent_did: {{ _eq: "{agent_did}" }} }},
                order: {{ service_id: ASC }}
            ) {{
                service_id
                agent_did
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
    agent_did: String,
    envelope: McpPoolEnvelope,
) -> McpPoolSnapshot {
    let mut health_by_service = envelope
        .health
        .into_iter()
        .map(|row| (row.service_id.clone(), row))
        .collect::<BTreeMap<_, _>>();
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
            send_agent_did: registry.send_agent_did,
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
        agent_did,
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
    match status.map(|value| value.trim().to_ascii_lowercase()) {
        Some(status) if status == "healthy" => totals.healthy += 1,
        Some(status) if status == "degraded" || status == "stale" => totals.degraded += 1,
        Some(status)
            if status == "evicted" || status == "reconnecting" || status == "unreachable" =>
        {
            totals.unreachable += 1;
        }
        Some(_) | None => totals.unknown += 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn joins_registry_with_agent_scoped_health_state() {
        let snapshot = build_mcp_pool_snapshot(
            at("2026-06-05T00:00:00Z"),
            "did:key:zAgent".to_string(),
            McpPoolEnvelope {
                registry: vec![
                    ToolServiceRegistryRow {
                        service_id: "obs-mcp".to_string(),
                        display_name: Some("Observability".to_string()),
                        description: Some("Fleet observability".to_string()),
                        hostname: Some("studio-1".to_string()),
                        tailscale_ip: Some("100.64.0.10".to_string()),
                        lan_ip: None,
                        mcp_port: Some(9201),
                        mcp_path: Some("/mcp".to_string()),
                        send_agent_did: true,
                        tools: Vec::new(),
                        status: Some("online".to_string()),
                        version: Some("test".to_string()),
                        updated_at: Some("2026-06-05T00:00:00Z".to_string()),
                    },
                    ToolServiceRegistryRow {
                        service_id: "silent-mcp".to_string(),
                        display_name: None,
                        description: None,
                        hostname: None,
                        tailscale_ip: None,
                        lan_ip: None,
                        mcp_port: None,
                        mcp_path: None,
                        send_agent_did: false,
                        tools: Vec::new(),
                        status: Some("offline".to_string()),
                        version: None,
                        updated_at: None,
                    },
                ],
                health: vec![ToolServiceHealthStateRow {
                    service_id: "obs-mcp".to_string(),
                    agent_did: Some("did:key:zAgent".to_string()),
                    endpoint: Some("http://100.64.0.10:9201/mcp".to_string()),
                    status: Some("healthy".to_string()),
                    tool_count: Some(12),
                    failure_count: Some(0),
                    k_max: Some(3),
                    backoff_until: None,
                    last_probe_at: Some("2026-06-05T00:00:00Z".to_string()),
                    last_seen: Some("2026-06-05T00:00:00Z".to_string()),
                    last_error_class: None,
                    last_error_message: None,
                    updated_at: Some("2026-06-05T00:00:00Z".to_string()),
                }],
            },
        );

        assert_eq!(snapshot.generated_at, "2026-06-05T00:00:00+00:00");
        assert_eq!(snapshot.agent_did, "did:key:zAgent");
        assert_eq!(
            snapshot.totals,
            McpPoolTotals {
                registered: 2,
                online: 1,
                healthy: 1,
                degraded: 0,
                unreachable: 0,
                unknown: 1,
            }
        );

        let obs = snapshot
            .services
            .iter()
            .find(|service| service.service_id == "obs-mcp")
            .unwrap();
        assert_eq!(obs.tool_count, Some(12));
        assert_eq!(obs.health_status.as_deref(), Some("healthy"));
        assert_eq!(obs.endpoint.as_deref(), Some("http://100.64.0.10:9201/mcp"));
    }
}
