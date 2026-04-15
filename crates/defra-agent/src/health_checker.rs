//! Background health checker for registered data services.
//!
//! Runs periodically to:
//! 1. Query ToolServiceRegistry for online services
//! 2. Detect stale heartbeats
//! 3. Probe MCP endpoints
//! 4. Evict dead connections
//! 5. Publish local service health state for the meta-tools

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
use serde::Deserialize;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::mcp_pool::{resolve_mcp_url, McpPool};

const HEALTH_CHECK_INTERVAL: Duration = Duration::from_secs(30);
const STALENESS_THRESHOLD_SECS: i64 = 120;
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Health status of a single service.
#[derive(Debug, Clone)]
pub struct ServiceHealth {
    pub status: HealthStatus,
    pub last_seen: DateTime<Utc>,
    pub last_error: Option<String>,
}

/// Possible health states for a service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthStatus {
    Healthy,
    Stale,
    Unreachable,
}

impl std::fmt::Display for HealthStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Healthy => write!(f, "healthy"),
            Self::Stale => write!(f, "stale"),
            Self::Unreachable => write!(f, "unreachable"),
        }
    }
}

/// Shared health state for all discovered services.
#[derive(Clone)]
pub struct ServiceHealthMap {
    inner: Arc<RwLock<HashMap<String, ServiceHealth>>>,
}

impl ServiceHealthMap {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn get(&self, service_id: &str) -> Option<ServiceHealth> {
        self.inner.read().await.get(service_id).cloned()
    }

    pub async fn snapshot(&self) -> HashMap<String, ServiceHealth> {
        self.inner.read().await.clone()
    }

    async fn set(&self, service_id: String, health: ServiceHealth) {
        self.inner.write().await.insert(service_id, health);
    }

    async fn retain_services(&self, service_ids: &HashSet<String>) {
        self.inner
            .write()
            .await
            .retain(|service_id, _| service_ids.contains(service_id));
    }
}

impl Default for ServiceHealthMap {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Deserialize)]
struct RegistryServiceEntry {
    service_id: String,
    #[serde(default, deserialize_with = "crate::registry::null_as_empty_string")]
    hostname: String,
    #[serde(default, deserialize_with = "crate::registry::null_as_empty_string")]
    tailscale_ip: String,
    #[serde(default, deserialize_with = "crate::registry::null_as_empty_string")]
    lan_ip: String,
    mcp_port: Option<u16>,
    #[serde(default, deserialize_with = "crate::registry::null_as_empty_string")]
    mcp_path: String,
    updated_at: Option<String>,
}

/// Spawn the background health checker task.
pub fn spawn_health_checker(
    node: Arc<EmbeddedNode>,
    mcp_pool: McpPool,
    health_map: ServiceHealthMap,
    local_hostname: String,
    local_subnet: Option<String>,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if let Err(error) = run_health_check(
            node.as_ref(),
            &mcp_pool,
            &health_map,
            &local_hostname,
            local_subnet.as_deref(),
        )
        .await
        {
            tracing::warn!(error = %error, "initial health check cycle failed");
        }

        let mut ticker = tokio::time::interval(HEALTH_CHECK_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        ticker.tick().await;

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    tracing::debug!("health checker cancelled");
                    return;
                }
                _ = ticker.tick() => {
                    if let Err(error) = run_health_check(
                        node.as_ref(),
                        &mcp_pool,
                        &health_map,
                        &local_hostname,
                        local_subnet.as_deref(),
                    ).await {
                        tracing::warn!(error = %error, "health check cycle failed");
                    }
                }
            }
        }
    })
}

async fn run_health_check(
    node: &EmbeddedNode,
    mcp_pool: &McpPool,
    health_map: &ServiceHealthMap,
    local_hostname: &str,
    local_subnet: Option<&str>,
) -> Result<()> {
    let query = r#"{
  ToolServiceRegistry(
    filter: { status: { _eq: "online" } }
  ) {
    service_id
    hostname
    tailscale_ip
    lan_ip
    mcp_port
    mcp_path
    updated_at
  }
}"#;

    let resp = node.execute(query).await;
    if resp.has_errors() {
        anyhow::bail!("health check registry query failed: {:?}", resp.errors);
    }

    let raw_services = resp
        .data
        .as_ref()
        .and_then(|data| data.get("ToolServiceRegistry"))
        .cloned()
        .unwrap_or_else(|| serde_json::Value::Array(Vec::new()));

    let services: Vec<RegistryServiceEntry> =
        serde_json::from_value(raw_services).context("parsing ToolServiceRegistry entries")?;

    let now = Utc::now();
    let mut online_service_ids = HashSet::new();

    for service in services {
        let service_id = service.service_id.clone();
        online_service_ids.insert(service_id.clone());

        let heartbeat_seen_at = parse_updated_at(service.updated_at.as_deref()).unwrap_or(now);
        let previous = health_map.get(&service_id).await;

        let Some(mcp_port) = service.mcp_port.filter(|port| *port != 0) else {
            health_map
                .set(
                    service_id.clone(),
                    ServiceHealth {
                        status: HealthStatus::Unreachable,
                        last_seen: previous
                            .map(|health| health.last_seen)
                            .unwrap_or(heartbeat_seen_at),
                        last_error: Some("registry entry missing mcp_port".to_string()),
                    },
                )
                .await;
            continue;
        };

        if service.hostname.is_empty()
            && service.tailscale_ip.is_empty()
            && service.lan_ip.is_empty()
        {
            health_map
                .set(
                    service_id.clone(),
                    ServiceHealth {
                        status: HealthStatus::Unreachable,
                        last_seen: previous
                            .map(|health| health.last_seen)
                            .unwrap_or(heartbeat_seen_at),
                        last_error: Some("registry entry missing address fields".to_string()),
                    },
                )
                .await;
            continue;
        }

        let endpoint = resolve_mcp_url(
            &service.hostname,
            &service.tailscale_ip,
            &service.lan_ip,
            mcp_port,
            &service.mcp_path,
            local_hostname,
            local_subnet,
        );

        let is_stale =
            now.signed_duration_since(heartbeat_seen_at).num_seconds() > STALENESS_THRESHOLD_SECS;

        match tokio::time::timeout(PROBE_TIMEOUT, mcp_pool.list_tools(&service_id, &endpoint)).await
        {
            Ok(Ok(_)) => {
                health_map
                    .set(
                        service_id.clone(),
                        ServiceHealth {
                            status: if is_stale {
                                HealthStatus::Stale
                            } else {
                                HealthStatus::Healthy
                            },
                            last_seen: now,
                            last_error: None,
                        },
                    )
                    .await;
            }
            Ok(Err(error)) => {
                tracing::warn!(
                    service_id = %service_id,
                    endpoint = %endpoint,
                    error = %error,
                    "MCP health probe failed"
                );
                mcp_pool.remove(&service_id).await;
                health_map
                    .set(
                        service_id.clone(),
                        ServiceHealth {
                            status: HealthStatus::Unreachable,
                            last_seen: previous
                                .map(|health| health.last_seen)
                                .unwrap_or(heartbeat_seen_at),
                            last_error: Some(error.to_string()),
                        },
                    )
                    .await;
            }
            Err(_) => {
                tracing::warn!(
                    service_id = %service_id,
                    endpoint = %endpoint,
                    "MCP health probe timed out"
                );
                mcp_pool.remove(&service_id).await;
                health_map
                    .set(
                        service_id,
                        ServiceHealth {
                            status: HealthStatus::Unreachable,
                            last_seen: previous
                                .map(|health| health.last_seen)
                                .unwrap_or(heartbeat_seen_at),
                            last_error: Some("probe timed out".to_string()),
                        },
                    )
                    .await;
            }
        }
    }

    health_map.retain_services(&online_service_ids).await;
    Ok(())
}

fn parse_updated_at(value: Option<&str>) -> Option<DateTime<Utc>> {
    let value = value?;
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

#[cfg(test)]
mod registry_parsing_tests {
    use super::RegistryServiceEntry;
    use serde_json::json;

    #[test]
    fn tolerates_null_address_fields() {
        let raw = json!({
            "service_id": "observability-mcp",
            "hostname": null,
            "tailscale_ip": null,
            "lan_ip": null,
            "mcp_port": 9201,
            "mcp_path": null,
            "updated_at": null,
        });

        let entry: RegistryServiceEntry =
            serde_json::from_value(raw).expect("null address fields must parse");

        assert_eq!(entry.service_id, "observability-mcp");
        assert_eq!(entry.hostname, "");
        assert_eq!(entry.tailscale_ip, "");
        assert_eq!(entry.lan_ip, "");
        assert_eq!(entry.mcp_port, Some(9201));
    }

    #[test]
    fn tolerates_null_array_from_health_query() {
        let raw = json!([
            {
                "service_id": "s1",
                "hostname": "studio-1",
                "tailscale_ip": "100.69.4.79",
                "lan_ip": null,
                "mcp_port": 9201,
                "mcp_path": "/mcp",
                "updated_at": "2026-04-14T00:00:00Z"
            }
        ]);

        let entries: Vec<RegistryServiceEntry> =
            serde_json::from_value(raw).expect("null lan_ip must not fail the batch parse");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].lan_ip, "");
        assert_eq!(entries[0].tailscale_ip, "100.69.4.79");
    }
}
