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

#[derive(Debug, Clone)]
pub struct ServiceHealth {
    pub status: HealthStatus,
    pub last_seen: DateTime<Utc>,
    pub last_error: Option<String>,
}

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

    #[cfg(test)]
    pub(crate) async fn set_for_test(&self, service_id: impl Into<String>, health: ServiceHealth) {
        self.set(service_id.into(), health).await;
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
pub struct McpHealthCheckService {
    pub service_id: String,
    #[serde(default, deserialize_with = "crate::registry::null_as_empty_string")]
    pub hostname: String,
    #[serde(default, deserialize_with = "crate::registry::null_as_empty_string")]
    pub tailscale_ip: String,
    #[serde(default, deserialize_with = "crate::registry::null_as_empty_string")]
    pub lan_ip: String,
    pub mcp_port: Option<u16>,
    #[serde(default, deserialize_with = "crate::registry::null_as_empty_string")]
    pub mcp_path: String,
    pub updated_at: Option<String>,
}

type RegistryServiceEntry = McpHealthCheckService;

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

    run_health_check_cycle(
        services,
        Utc::now(),
        mcp_pool,
        health_map,
        local_hostname,
        local_subnet,
    )
    .await
}

// Production transition step once the registry query has supplied online rows.
pub async fn run_health_check_cycle(
    services: Vec<McpHealthCheckService>,
    now: DateTime<Utc>,
    mcp_pool: &McpPool,
    health_map: &ServiceHealthMap,
    local_hostname: &str,
    local_subnet: Option<&str>,
) -> Result<()> {
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

// Inline test module preserved: single-test smoke check, deliberately not extracted to keep it co-located with the narrow code it tests.
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

#[cfg(test)]
mod transitions_tests {
    use chrono::Duration as ChronoDuration;
    use rmcp::model::ListToolsResult;

    use super::{
        run_health_check_cycle, HealthStatus, RegistryServiceEntry, ServiceHealth,
        ServiceHealthMap, STALENESS_THRESHOLD_SECS,
    };
    use crate::lean_vocab_test::{lean_mcp_health_k1_cases, LeanMcpHealthCase};
    use crate::mcp_pool::McpPool;

    /// Project a Lean K=1 case's `rust_projection` to a `HealthStatus` for
    /// today's Rust to assert against. `None` means the service was removed
    /// via `registryAbsent`; tests skip those rows (Rust represents removal
    /// by dropping the map entry, not by a `HealthStatus`).
    fn projected_health_status(case: &LeanMcpHealthCase) -> Option<HealthStatus> {
        case.rust_projection.as_deref().map(|s| match s {
            "healthy" => HealthStatus::Healthy,
            "stale" => HealthStatus::Stale,
            "unreachable" => HealthStatus::Unreachable,
            other => panic!(
                "Lean MCP health case {} produced unknown rust_projection {:?}",
                case.name, other
            ),
        })
    }

    fn start_status(case: &LeanMcpHealthCase) -> HealthStatus {
        match case.start_state.as_str() {
            "healthy" => HealthStatus::Healthy,
            // K=1 collapses: degraded can only be entered via probeSuccess(stale=true),
            // which projects to Stale.
            "degraded" => HealthStatus::Stale,
            "evicted" | "reconnecting" => HealthStatus::Unreachable,
            other => panic!(
                "Lean MCP health case {} produced unknown start_state {:?}",
                case.name, other
            ),
        }
    }

    async fn run_health_check_projection(case: &LeanMcpHealthCase) -> Option<HealthStatus> {
        const SERVICE_ID: &str = "lean-mcp-health-service";

        let now = chrono::Utc::now();
        let health_map = ServiceHealthMap::new();
        health_map
            .set_for_test(
                SERVICE_ID,
                ServiceHealth {
                    status: start_status(case),
                    last_seen: now,
                    last_error: None,
                },
            )
            .await;

        let services = match case.event.as_str() {
            "registryAbsent" => Vec::new(),
            "probeSuccessFresh" | "probeFail" => vec![registry_entry(SERVICE_ID, now)],
            "probeSuccessStale" => vec![registry_entry(
                SERVICE_ID,
                now - ChronoDuration::seconds(STALENESS_THRESHOLD_SECS + 30),
            )],
            "backoffExpiry" => {
                // At K=1 Rust does not arm a backoff timer, so there is no
                // runtime health-check tick to drive for this Lean event. The
                // observable production state is unchanged until Stage 2 adds
                // K>=2 backoff behavior.
                return Some(start_status(case));
            }
            other => panic!(
                "Lean MCP health case {} produced unknown event {:?}",
                case.name, other
            ),
        };

        let pool = mcp_pool_for_event(&case.event);
        run_health_check_cycle(services, now, &pool, &health_map, "local-host", None)
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "run_health_check_cycle failed for Lean MCP health case {}: {error}",
                    case.name
                )
            });

        health_map.get(SERVICE_ID).await.map(|health| health.status)
    }

    fn mcp_pool_for_event(event: &str) -> McpPool {
        let probe_fails = event == "probeFail";
        McpPool::new_with_list_tools_handler(move |_service_id, _endpoint| async move {
            if probe_fails {
                anyhow::bail!("Lean probeFail")
            }
            Ok(ListToolsResult::default())
        })
    }

    fn registry_entry(
        service_id: &str,
        updated_at: chrono::DateTime<chrono::Utc>,
    ) -> RegistryServiceEntry {
        RegistryServiceEntry {
            service_id: service_id.to_string(),
            hostname: "remote-host".to_string(),
            tailscale_ip: "100.64.0.1".to_string(),
            lan_ip: String::new(),
            mcp_port: Some(9201),
            mcp_path: "/mcp".to_string(),
            updated_at: Some(updated_at.to_rfc3339()),
        }
    }

    #[tokio::test]
    async fn generated_mcp_health_k1_cases_match_health_checker_transitions() {
        let cases = lean_mcp_health_k1_cases();
        assert!(
            !cases.is_empty(),
            "Lean must emit at least one K=1 MCP health case"
        );

        for case in cases {
            assert_eq!(case.threshold_k, 1, "test must only consume K=1 rows");
            assert_eq!(
                case.start_count, 0,
                "K=1 Lean MCP health rows must start at failureCount=0"
            );

            let expected = projected_health_status(case);
            let actual = run_health_check_projection(case).await;
            assert_eq!(
                actual, expected,
                "Lean MCP health K=1 case {} must match Rust HealthStatus assignment",
                case.name
            );
        }
    }
}
