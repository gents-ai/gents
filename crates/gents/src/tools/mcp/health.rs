use rapidhash::{HashMapExt, HashSetExt, RapidHashMap, RapidHashSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use defra_node::Node as EmbeddedNode;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use gents_protocol::tool_service_health::ToolServiceHealthProjection;

use crate::store::graphql::escape_graphql_string;
use crate::tools::mcp::pool::{resolve_mcp_url, McpPool};

/// Node-local service observation settings; these do not change an agent's
/// remote-tool call deadlines.
#[derive(Clone, Debug)]
pub struct HealthCheckerOptions {
    pub cycle_interval: Duration,
    pub probe_timeout: Duration,
    pub staleness_threshold: Duration,
    pub failure_threshold_k: u32,
    pub backoff_initial: Duration,
    pub backoff_max: Duration,
}

impl Default for HealthCheckerOptions {
    fn default() -> Self {
        Self {
            cycle_interval: Duration::from_secs(30),
            probe_timeout: Duration::from_secs(5),
            staleness_threshold: Duration::from_secs(120),
            failure_threshold_k: 3,
            backoff_initial: Duration::from_secs(30),
            backoff_max: Duration::from_secs(600),
        }
    }
}

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

impl From<ToolServiceHealthProjection> for HealthStatus {
    fn from(projection: ToolServiceHealthProjection) -> Self {
        match projection {
            ToolServiceHealthProjection::Healthy => Self::Healthy,
            ToolServiceHealthProjection::Stale => Self::Stale,
            ToolServiceHealthProjection::Unreachable => Self::Unreachable,
        }
    }
}

#[derive(Debug, Clone)]
struct ServiceHealthEntry {
    health: ServiceHealth,
    model: ServiceModelInternal,
    endpoint: Option<String>,
    tool_count: Option<i64>,
    last_probe_at: DateTime<Utc>,
}

impl ServiceHealthEntry {
    #[cfg(test)]
    fn from_public(health: ServiceHealth) -> Self {
        let model = ServiceModelInternal::from_status(health.status);
        let last_probe_at = health.last_seen;
        Self {
            health,
            model,
            endpoint: None,
            tool_count: None,
            last_probe_at,
        }
    }

    fn from_model(
        model: ServiceModelInternal,
        last_seen: DateTime<Utc>,
        last_error: Option<String>,
        endpoint: Option<String>,
        tool_count: Option<i64>,
        last_probe_at: DateTime<Utc>,
    ) -> Self {
        Self {
            health: ServiceHealth {
                status: model.state.project().into(),
                last_seen,
                last_error,
            },
            model,
            endpoint,
            tool_count,
            last_probe_at,
        }
    }
}

type HealthStateInternal = gents_protocol::tool_service_health::ToolServiceHealthState;

#[derive(Debug, Clone, PartialEq, Eq)]
struct ServiceModelInternal {
    state: HealthStateInternal,
    failure_count: u32,
    backoff_until: Option<DateTime<Utc>>,
}

impl ServiceModelInternal {
    fn initial() -> Self {
        Self {
            state: HealthStateInternal::Healthy,
            failure_count: 0,
            backoff_until: None,
        }
    }

    #[cfg(test)]
    fn from_status(status: HealthStatus) -> Self {
        Self {
            state: match status {
                HealthStatus::Healthy => HealthStateInternal::Healthy,
                HealthStatus::Stale => HealthStateInternal::Degraded,
                HealthStatus::Unreachable => HealthStateInternal::Evicted,
            },
            failure_count: 0,
            backoff_until: None,
        }
    }
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProbeEvent {
    ProbeSuccess { stale: bool },
    ProbeFail,
    BackoffExpiry,
    RegistryAbsent,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MCPServiceHealthSnapshot {
    pub service_id: String,
    pub endpoint: Option<String>,
    pub status: String,
    pub tool_count: Option<i64>,
    pub failure_count: u32,
    pub k_max: u32,
    pub backoff_until: Option<DateTime<Utc>>,
    pub last_probe_at: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub last_error: Option<String>,
}

#[derive(Clone)]
pub struct ServiceHealthMap {
    inner: Arc<RwLock<RapidHashMap<String, ServiceHealthEntry>>>,
    /// A failed or cancelled write has an ambiguous outcome. Removing the last
    /// confirmed snapshot before awaiting it forces the next publish to name
    /// every field. This belongs to the same owner as the live observations.
    persisted: Arc<tokio::sync::Mutex<RapidHashMap<(String, String), MCPServiceHealthSnapshot>>>,
}

impl ServiceHealthMap {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(RapidHashMap::new())),
            persisted: Arc::new(tokio::sync::Mutex::new(RapidHashMap::new())),
        }
    }

    pub async fn get(&self, service_id: &str) -> Option<ServiceHealth> {
        self.inner
            .read()
            .await
            .get(service_id)
            .map(|entry| entry.health.clone())
    }

    pub async fn snapshot(&self) -> RapidHashMap<String, ServiceHealth> {
        self.inner
            .read()
            .await
            .iter()
            .map(|(service_id, entry)| (service_id.clone(), entry.health.clone()))
            .collect()
    }

    pub async fn snapshot_full(&self, k_max: u32) -> Vec<MCPServiceHealthSnapshot> {
        let k_max = k_max.max(1);
        self.inner
            .read()
            .await
            .iter()
            .map(|(service_id, entry)| MCPServiceHealthSnapshot {
                service_id: service_id.clone(),
                endpoint: entry.endpoint.clone(),
                status: entry.model.state.as_str().to_string(),
                tool_count: entry.tool_count,
                failure_count: entry.model.failure_count,
                k_max,
                backoff_until: entry.model.backoff_until,
                last_probe_at: entry.last_probe_at,
                last_seen: entry.health.last_seen,
                last_error: entry.health.last_error.clone(),
            })
            .collect()
    }

    #[cfg(test)]
    async fn set(&self, service_id: String, health: ServiceHealth) {
        self.inner
            .write()
            .await
            .insert(service_id, ServiceHealthEntry::from_public(health));
    }

    async fn get_entry(&self, service_id: &str) -> Option<ServiceHealthEntry> {
        self.inner.read().await.get(service_id).cloned()
    }

    async fn set_entry(&self, service_id: String, entry: ServiceHealthEntry) {
        self.inner.write().await.insert(service_id, entry);
    }

    #[cfg(test)]
    pub(crate) async fn set_for_test(&self, service_id: impl Into<String>, health: ServiceHealth) {
        self.set(service_id.into(), health).await;
    }

    async fn retain_services(&self, service_ids: &RapidHashSet<String>) {
        self.inner
            .write()
            .await
            .retain(|service_id, _| service_ids.contains(service_id));
    }
}

#[derive(Clone, Copy)]
pub struct HealthPersistenceContext<'a> {
    pub node: &'a Arc<EmbeddedNode>,
    pub node_did: &'a str,
}

impl HealthPersistenceContext<'_> {
    fn runtime_access(&self) -> Result<crate::store::config_client::ConfigAccess> {
        anyhow::ensure!(
            self.node.node_identity_did() == Some(self.node_did),
            "health persistence requires the configured runtime identity"
        );
        Ok(crate::store::config_client::ConfigAccess::local_as(
            self.node.clone(),
            identity::Did::new(self.node_did)?,
        ))
    }
}

impl Default for ServiceHealthMap {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct McpHealthCheckService {
    pub service_id: String,
    #[serde(
        default,
        deserialize_with = "crate::tools::mcp::registry::null_as_empty_string"
    )]
    pub hostname: String,
    #[serde(
        default,
        deserialize_with = "crate::tools::mcp::registry::null_as_empty_string"
    )]
    pub tailscale_ip: String,
    #[serde(
        default,
        deserialize_with = "crate::tools::mcp::registry::null_as_empty_string"
    )]
    pub lan_ip: String,
    pub mcp_port: Option<u16>,
    #[serde(
        default,
        deserialize_with = "crate::tools::mcp::registry::null_as_empty_string"
    )]
    pub mcp_path: String,
    #[serde(
        default,
        deserialize_with = "crate::tools::mcp::registry::null_as_default"
    )]
    pub send_node_did: bool,
    pub updated_at: Option<String>,
}

impl From<crate::documents::ToolServiceRegistry> for McpHealthCheckService {
    fn from(service: crate::documents::ToolServiceRegistry) -> Self {
        Self {
            service_id: service.service_id,
            hostname: service.hostname.unwrap_or_default(),
            tailscale_ip: service.tailscale_ip.unwrap_or_default(),
            lan_ip: service.lan_ip.unwrap_or_default(),
            mcp_port: service.mcp_port.and_then(|port| u16::try_from(port).ok()),
            mcp_path: service.mcp_path.unwrap_or_default(),
            send_node_did: service.send_node_did,
            updated_at: None,
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "public signature called from other crates"
)]
pub fn spawn_health_checker(
    node: Arc<EmbeddedNode>,
    mcp_pool: McpPool,
    health_map: ServiceHealthMap,
    local_hostname: String,
    local_subnet: Option<String>,
    cancel: CancellationToken,
    options: HealthCheckerOptions,
    node_did: String,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let persistence = HealthPersistenceContext {
            node: &node,
            node_did: node_did.as_str(),
        };
        if let Err(error) = run_health_check_with_cancel(
            node.as_ref(),
            &mcp_pool,
            &health_map,
            &local_hostname,
            local_subnet.as_deref(),
            &options,
            Some(persistence),
            Some(&cancel),
        )
        .await
        {
            tracing::warn!(error = %error, "initial health check cycle failed");
        }

        let mut ticker = tokio::time::interval(options.cycle_interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        ticker.tick().await;

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    tracing::debug!("health checker cancelled");
                    return;
                }
                _ = ticker.tick() => {
                    let persistence = HealthPersistenceContext {
                        node: &node,
                        node_did: node_did.as_str(),
                    };
                    if let Err(error) = run_health_check_with_cancel(
                        node.as_ref(),
                        &mcp_pool,
                        &health_map,
                        &local_hostname,
                        local_subnet.as_deref(),
                        &options,
                        Some(persistence),
                        Some(&cancel),
                    ).await {
                        tracing::warn!(error = %error, "health check cycle failed");
                    }
                }
            }
        }
    })
}

#[cfg(test)]
async fn run_health_check(
    node: &EmbeddedNode,
    mcp_pool: &McpPool,
    health_map: &ServiceHealthMap,
    local_hostname: &str,
    local_subnet: Option<&str>,
    options: &HealthCheckerOptions,
    persistence: Option<HealthPersistenceContext<'_>>,
) -> Result<()> {
    run_health_check_with_cancel(
        node,
        mcp_pool,
        health_map,
        local_hostname,
        local_subnet,
        options,
        persistence,
        None,
    )
    .await
}

#[expect(
    clippy::too_many_arguments,
    reason = "registry health inputs and explicit shutdown scope"
)]
async fn run_health_check_with_cancel(
    node: &EmbeddedNode,
    mcp_pool: &McpPool,
    health_map: &ServiceHealthMap,
    local_hostname: &str,
    local_subnet: Option<&str>,
    options: &HealthCheckerOptions,
    persistence: Option<HealthPersistenceContext<'_>>,
    cancel: Option<&CancellationToken>,
) -> Result<()> {
    if cancel.is_some_and(CancellationToken::is_cancelled) {
        return Ok(());
    }
    let context = persistence
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("registry health checks require a node scope"))?;
    anyhow::ensure!(
        std::ptr::eq(context.node.as_ref(), node),
        "health registry node does not match persistence"
    );
    let owner = context.node_did;
    let services =
        crate::tools::mcp::registry::configured_mcp_services_on(&context.runtime_access()?, owner)
            .await?
            .into_iter()
            .filter(|service| service.enabled)
            .map(McpHealthCheckService::from)
            .collect();
    let mcp_pool = mcp_pool.for_agent(owner);

    run_health_check_cycle_with_cancel(
        services,
        Utc::now(),
        &mcp_pool,
        health_map,
        local_hostname,
        local_subnet,
        options,
        persistence,
        persistence.as_ref().map(|context| context.node_did),
        cancel,
    )
    .await
}

#[expect(
    clippy::too_many_arguments,
    reason = "public signature called from other crates"
)]
pub async fn run_health_check_cycle(
    services: Vec<McpHealthCheckService>,
    now: DateTime<Utc>,
    mcp_pool: &McpPool,
    health_map: &ServiceHealthMap,
    local_hostname: &str,
    local_subnet: Option<&str>,
    options: &HealthCheckerOptions,
    persistence: Option<HealthPersistenceContext<'_>>,
) -> Result<()> {
    run_health_check_cycle_with_cancel(
        services,
        now,
        mcp_pool,
        health_map,
        local_hostname,
        local_subnet,
        options,
        persistence,
        persistence.as_ref().map(|context| context.node_did),
        None,
    )
    .await
}

/// A client probe sends its authenticated caller DID only when the service
/// opts in; the client never substitutes a runtime's Node identity.
#[expect(
    clippy::too_many_arguments,
    reason = "client probe inputs are explicit"
)]
pub async fn run_health_check_cycle_for_actor(
    services: Vec<McpHealthCheckService>,
    now: DateTime<Utc>,
    mcp_pool: &McpPool,
    health_map: &ServiceHealthMap,
    local_hostname: &str,
    local_subnet: Option<&str>,
    options: &HealthCheckerOptions,
    actor: &str,
) -> Result<()> {
    identity::Did::new(actor)?;
    run_health_check_cycle_with_cancel(
        services,
        now,
        mcp_pool,
        health_map,
        local_hostname,
        local_subnet,
        options,
        None,
        Some(actor),
        None,
    )
    .await
}

/// Cancelling a remote observation is not evidence of a failed service. Persisted
/// observations already being written drain through their existing owner.
#[expect(
    clippy::too_many_arguments,
    reason = "scoped health-cycle inputs and shutdown signal"
)]
async fn run_health_check_cycle_with_cancel(
    services: Vec<McpHealthCheckService>,
    now: DateTime<Utc>,
    mcp_pool: &McpPool,
    health_map: &ServiceHealthMap,
    local_hostname: &str,
    local_subnet: Option<&str>,
    options: &HealthCheckerOptions,
    persistence: Option<HealthPersistenceContext<'_>>,
    outbound_actor: Option<&str>,
    cancel: Option<&CancellationToken>,
) -> Result<()> {
    let mut online_service_ids = RapidHashSet::new();

    for service in services {
        if cancel.is_some_and(CancellationToken::is_cancelled) {
            return Ok(());
        }
        let service = if let Some(context) = persistence.as_ref() {
            let current = crate::tools::mcp::registry::configured_mcp_service_on(
                &context.runtime_access()?,
                context.node_did,
                &service.service_id,
            )
            .await?;
            match current.filter(|service| service.enabled) {
                Some(service) => McpHealthCheckService::from(service),
                None => continue,
            }
        } else {
            service
        };
        let service_id = service.service_id.clone();
        online_service_ids.insert(service_id.clone());

        let heartbeat_seen_at = parse_updated_at(service.updated_at.as_deref()).unwrap_or(now);
        let previous = health_map.get_entry(&service_id).await;
        let previous_model = previous
            .as_ref()
            .map(|entry| entry.model.clone())
            .unwrap_or_else(ServiceModelInternal::initial);
        let previous_last_seen = previous
            .as_ref()
            .map(|entry| entry.health.last_seen)
            .unwrap_or(heartbeat_seen_at);
        let previous_endpoint = previous.as_ref().and_then(|entry| entry.endpoint.clone());
        let previous_tool_count = previous.as_ref().and_then(|entry| entry.tool_count);

        if backoff_is_active(&previous_model, now, options) {
            continue;
        }

        let Some(mcp_port) = service.mcp_port.filter(|port| *port != 0) else {
            apply_probe_failure(
                mcp_pool,
                health_map,
                ProbeFailure {
                    service_id: &service_id,
                    previous_model,
                    previous_last_seen,
                    endpoint: previous_endpoint,
                    tool_count: previous_tool_count,
                    error: "registry entry missing mcp_port".to_string(),
                },
                now,
                options,
            )
            .await;
            continue;
        };

        if service.hostname.is_empty()
            && service.tailscale_ip.is_empty()
            && service.lan_ip.is_empty()
        {
            apply_probe_failure(
                mcp_pool,
                health_map,
                ProbeFailure {
                    service_id: &service_id,
                    previous_model,
                    previous_last_seen,
                    endpoint: previous_endpoint,
                    tool_count: previous_tool_count,
                    error: "registry entry missing address fields".to_string(),
                },
                now,
                options,
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

        let is_stale = now
            .signed_duration_since(heartbeat_seen_at)
            .to_std()
            .map(|age| age > options.staleness_threshold)
            .unwrap_or(false);

        if let Some(context) = persistence.as_ref() {
            let current = crate::tools::mcp::registry::configured_mcp_service_on(
                &context.runtime_access()?,
                context.node_did,
                &service_id,
            )
            .await?;
            let Some(current) = current.filter(|service| service.enabled) else {
                online_service_ids.remove(&service_id);
                continue;
            };
            anyhow::ensure!(
                McpHealthCheckService::from(current) == service,
                "MCP service route changed before health observation"
            );
        }

        let outbound_actor = if service.send_node_did {
            Some(
                outbound_actor
                    .context("identity-aware MCP probe requires its authenticated actor")?,
            )
        } else {
            None
        };
        let cancellation = cancel.cloned().unwrap_or_default();
        let Some(probe_result) = cancellation
            .run_until_cancelled(tokio::time::timeout(
                options.probe_timeout,
                mcp_pool.list_tools_with_node_did(&service_id, &endpoint, outbound_actor),
            ))
            .await
        else {
            return Ok(());
        };
        if cancellation.is_cancelled() {
            return Ok(());
        }
        match probe_result {
            Ok(Ok(tools)) => {
                let next_model = step_service(
                    previous_model,
                    ProbeEvent::ProbeSuccess { stale: is_stale },
                    now,
                    options,
                )
                .expect("probeSuccess must preserve the service model");
                let tool_count = Some(tools.tools.len() as i64);
                health_map
                    .set_entry(
                        service_id.clone(),
                        ServiceHealthEntry::from_model(
                            next_model,
                            now,
                            None,
                            Some(endpoint.clone()),
                            tool_count,
                            now,
                        ),
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
                apply_probe_failure(
                    mcp_pool,
                    health_map,
                    ProbeFailure {
                        service_id: &service_id,
                        previous_model,
                        previous_last_seen,
                        endpoint: Some(endpoint.clone()),
                        tool_count: previous_tool_count,
                        error: error.to_string(),
                    },
                    now,
                    options,
                )
                .await;
            }
            Err(_) => {
                tracing::warn!(
                    service_id = %service_id,
                    endpoint = %endpoint,
                    "MCP health probe timed out"
                );
                apply_probe_failure(
                    mcp_pool,
                    health_map,
                    ProbeFailure {
                        service_id: &service_id,
                        previous_model,
                        previous_last_seen,
                        endpoint: Some(endpoint.clone()),
                        tool_count: previous_tool_count,
                        error: "probe timed out".to_string(),
                    },
                    now,
                    options,
                )
                .await;
            }
        }
    }

    if cancel.is_some_and(CancellationToken::is_cancelled) {
        return Ok(());
    }
    health_map.retain_services(&online_service_ids).await;

    if let Some(persistence) = persistence {
        let snapshot = health_map.snapshot_full(options.failure_threshold_k).await;
        persist_health_snapshot(&persistence, health_map, &snapshot, now).await;
        if cancel.is_some_and(CancellationToken::is_cancelled) {
            return Ok(());
        }
        match load_persisted_service_ids(&persistence).await {
            Ok(persisted_ids) => {
                for service_id in persisted_ids
                    .difference(&online_service_ids)
                    .cloned()
                    .collect::<Vec<_>>()
                {
                    if cancel.is_some_and(CancellationToken::is_cancelled) {
                        break;
                    }
                    delete_persisted_health_state(&persistence, &service_id).await;
                }
            }
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "failed to load persisted ToolServiceHealthState rows for stale-row reconciliation",
                );
            }
        }
    }

    Ok(())
}

struct ProbeFailure<'a> {
    service_id: &'a str,
    previous_model: ServiceModelInternal,
    previous_last_seen: DateTime<Utc>,
    endpoint: Option<String>,
    tool_count: Option<i64>,
    error: String,
}

async fn apply_probe_failure(
    mcp_pool: &McpPool,
    health_map: &ServiceHealthMap,
    failure: ProbeFailure<'_>,
    now: DateTime<Utc>,
    options: &HealthCheckerOptions,
) {
    let ProbeFailure {
        service_id,
        previous_model,
        previous_last_seen,
        endpoint,
        tool_count,
        error,
    } = failure;
    let next_model = step_service(previous_model, ProbeEvent::ProbeFail, now, options)
        .expect("probeFail must preserve the service model");
    if next_model.state == HealthStateInternal::Evicted {
        mcp_pool.remove(service_id).await;
    }
    health_map
        .set_entry(
            service_id.to_string(),
            ServiceHealthEntry::from_model(
                next_model,
                previous_last_seen,
                Some(error),
                endpoint,
                tool_count,
                now,
            ),
        )
        .await;
}

fn step_service(
    prev: ServiceModelInternal,
    event: ProbeEvent,
    now: DateTime<Utc>,
    options: &HealthCheckerOptions,
) -> Option<ServiceModelInternal> {
    match event {
        ProbeEvent::RegistryAbsent => None,
        ProbeEvent::BackoffExpiry => {
            let mut next = prev;
            if next.state == HealthStateInternal::Evicted {
                next.state = HealthStateInternal::Reconnecting;
                next.backoff_until = None;
            }
            Some(next)
        }
        ProbeEvent::ProbeSuccess { stale } => Some(ServiceModelInternal {
            state: if stale {
                HealthStateInternal::Degraded
            } else {
                HealthStateInternal::Healthy
            },
            failure_count: 0,
            backoff_until: None,
        }),
        ProbeEvent::ProbeFail => {
            let failure_count = prev.failure_count.saturating_add(1);
            let threshold = failure_threshold_k(options);
            let state = if failure_count >= threshold {
                HealthStateInternal::Evicted
            } else {
                HealthStateInternal::Degraded
            };
            let backoff_until = (state == HealthStateInternal::Evicted).then(|| {
                let attempts = failure_count.saturating_sub(threshold);
                now_plus_duration(now, backoff_duration(attempts, options))
            });
            Some(ServiceModelInternal {
                state,
                failure_count,
                backoff_until,
            })
        }
    }
}

fn backoff_is_active(
    model: &ServiceModelInternal,
    now: DateTime<Utc>,
    options: &HealthCheckerOptions,
) -> bool {
    model.failure_count >= failure_threshold_k(options)
        && model
            .backoff_until
            .map(|backoff_until| now < backoff_until)
            .unwrap_or(false)
}

fn backoff_duration(attempts: u32, options: &HealthCheckerOptions) -> Duration {
    let multiplier = 1u32.checked_shl(attempts).unwrap_or(u32::MAX);
    options
        .backoff_initial
        .saturating_mul(multiplier)
        .min(options.backoff_max)
}

fn failure_threshold_k(options: &HealthCheckerOptions) -> u32 {
    options.failure_threshold_k.max(1)
}

fn now_plus_duration(now: DateTime<Utc>, duration: Duration) -> DateTime<Utc> {
    chrono::Duration::from_std(duration)
        .ok()
        .and_then(|duration| now.checked_add_signed(duration))
        .unwrap_or(now)
}

fn parse_updated_at(value: Option<&str>) -> Option<DateTime<Utc>> {
    let value = value?;
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

/// External premise: an MCP health probe fails with transport and OS errors
/// (rmcp, reqwest, std::io) recorded as text in the health row; their kind
/// survives only in that wording.
fn classify_last_error(error: &str) -> &'static str {
    let lower = error.to_lowercase();
    if lower.contains("timed out") || lower.contains("timeout") {
        "timeout"
    } else if lower.contains("connection refused") || lower.contains("refused") {
        "connection_refused"
    } else if lower.contains("no route") || lower.contains("unreachable") {
        "network_unreachable"
    } else if lower.contains("missing mcp_port") || lower.contains("missing address") {
        "registry_invalid"
    } else if lower.contains("stream closed") || lower.contains("eof") {
        "stream_closed"
    } else {
        "other"
    }
}

async fn persist_health_snapshot(
    persistence: &HealthPersistenceContext<'_>,
    health_map: &ServiceHealthMap,
    snapshot: &[MCPServiceHealthSnapshot],
    now: DateTime<Utc>,
) {
    let mut persisted = health_map.persisted.lock().await;
    let service_ids: RapidHashSet<_> = snapshot.iter().map(|row| row.service_id.as_str()).collect();
    persisted.retain(|(node_did, service_id), _| {
        node_did != persistence.node_did || service_ids.contains(service_id.as_str())
    });
    for entry in snapshot {
        let key = (persistence.node_did.to_owned(), entry.service_id.clone());
        if persisted.get(&key) == Some(entry) {
            continue;
        }
        let previous = persisted.remove(&key);
        let mut result =
            upsert_persisted_health_state(persistence, previous.as_ref(), entry, now).await;
        if result.is_err() && collapse_health_state_twins(persistence, &entry.service_id).await {
            result = upsert_persisted_health_state(persistence, None, entry, now).await;
        }
        if let Err(error) = result {
            tracing::warn!(
                service_id = %entry.service_id,
                error = %error,
                "failed to persist ToolServiceHealthState",
            );
        } else {
            persisted.insert(key, entry.clone());
        }
    }
}

fn persisted_health_fields(
    entry: &MCPServiceHealthSnapshot,
) -> serde_json::Map<String, serde_json::Value> {
    serde_json::json!({
        "endpoint": entry.endpoint.as_deref().unwrap_or(""),
        "status": entry.status,
        "tool_count": entry.tool_count,
        "failure_count": entry.failure_count,
        "k_max": entry.k_max,
        "backoff_until": entry.backoff_until,
        "last_probe_at": entry.last_probe_at,
        "last_seen": entry.last_seen,
        "last_error_class": entry.last_error.as_deref().map(classify_last_error),
        "last_error_message": entry.last_error,
    })
    .as_object()
    .expect("health fields are an object")
    .clone()
}

async fn upsert_persisted_health_state(
    persistence: &HealthPersistenceContext<'_>,
    previous: Option<&MCPServiceHealthSnapshot>,
    entry: &MCPServiceHealthSnapshot,
    now: DateTime<Utc>,
) -> Result<()> {
    let actor = identity::Did::new(persistence.node_did)?;
    let service_id = escape_graphql_string(&entry.service_id);
    let node_did = escape_graphql_string(persistence.node_did);
    let mut fields = persisted_health_fields(entry);
    // `updated_at` moves on every write, so an unchanged row is skipped here;
    // DefraDB writes no block for the other fields an update leaves unchanged.
    if previous.map(persisted_health_fields).as_ref() == Some(&fields) {
        return Ok(());
    }
    fields.insert("updated_at".into(), serde_json::json!(now));
    let update =
        gents_protocol::graphql::graphql_input_literal(&serde_json::Value::Object(fields.clone()))?;
    fields.insert("service_id".into(), serde_json::json!(entry.service_id));
    fields.insert("node_did".into(), serde_json::json!(persistence.node_did));
    let add = gents_protocol::graphql::graphql_input_literal(&serde_json::Value::Object(fields))?;
    let mutation = format!(
        r#"mutation {{
            upsert_ToolServiceHealthState(
                filter: {{ service_id: {{ _eq: "{service_id}" }}, node_did: {{ _eq: "{node_did}" }} }},
                add: {add},
                update: {update}
            ) {{ _docID }}
        }}"#,
    );
    let access = persistence.runtime_access()?;
    access.transact_write("health_checker.upsert_service_state", |txn| {
        let mutation = &mutation;
        let service_id = &service_id;
        let node_did = &node_did;
        let actor = &actor;
        Box::pin(async move {
            let response = txn.execute(&format!(
                r#"{{ ToolServiceHealthState(filter: {{service_id: {{_eq: "{service_id}"}}, node_did: {{_eq: "{node_did}"}}}}, limit: 2) {{_docID}} }}"#
            )).await?;
            let rows = gents_protocol::graphql::graphql_rows_from_response(&response, "ToolServiceHealthState");
            anyhow::ensure!(rows.len() <= 1, "ToolServiceHealthState is ambiguous");
            if let Some(row) = rows.first() {
                let doc = row["_docID"].as_str().context("ToolServiceHealthState has no document ID")?.to_owned();
                let owners = txn.document_owners("ToolServiceHealthState", &[doc]).await?;
                anyhow::ensure!(owners.len() == 1 && owners[0].visible && owners[0].owner.as_ref() == Some(actor),
                    "ToolServiceHealthState has no matching native runtime owner");
            }
            let response = txn.execute(mutation).await?;
            let doc = gents_protocol::graphql::extract_mutation_doc_id(&response, "ToolServiceHealthState")?;
            if rows.is_empty() {
                crate::store::config_client::publish_node_child_in_txn(
                    txn, "ToolServiceHealthState", &doc, actor.as_str(),
                ).await?;
            }
            Ok::<_, anyhow::Error>(())
        })
    }).await
}

/// The schema cannot make (service_id, node_did) unique, and an upsert fails
/// on every later cycle once twins exist. Keep the row readers select (newest
/// `updated_at`, then lowest `_docID`) and delete this node's other rows.
/// Returns whether any twin was deleted.
async fn collapse_health_state_twins(
    persistence: &HealthPersistenceContext<'_>,
    service_id: &str,
) -> bool {
    let service = escape_graphql_string(service_id);
    let node_did = escape_graphql_string(persistence.node_did);
    let query = format!(
        r#"{{ ToolServiceHealthState(
            filter: {{ service_id: {{ _eq: "{service}" }}, node_did: {{ _eq: "{node_did}" }} }},
            order: [{{ updated_at: DESC }}, {{ _docID: ASC }}]
        ) {{ _docID }} }}"#
    );
    let access = match persistence.runtime_access() {
        Ok(access) => access,
        Err(error) => {
            tracing::warn!(service_id, error = %error, "failed to select health runtime actor");
            return false;
        }
    };
    let twins = match access.execute(&query).await {
        Ok(response) => response
            .get("data")
            .and_then(|data| data.get("ToolServiceHealthState"))
            .and_then(|rows| rows.as_array())
            .into_iter()
            .flatten()
            .skip(1)
            .filter_map(|row| row.get("_docID").and_then(|id| id.as_str()))
            .map(|id| format!(r#""{}""#, escape_graphql_string(id)))
            .collect::<Vec<_>>(),
        Err(error) => {
            tracing::warn!(service_id, error = %error, "failed to load ToolServiceHealthState twins");
            return false;
        }
    };
    if twins.is_empty() {
        return false;
    }
    let mutation = format!(
        r#"mutation {{ delete_ToolServiceHealthState(filter: {{ _docID: {{ _in: [{}] }} }}) {{ _docID }} }}"#,
        twins.join(", ")
    );
    match access
        .write("health_checker.collapse_service_state_twins", &mutation)
        .await
    {
        Ok(_) => true,
        Err(error) => {
            tracing::warn!(service_id, error = %error, "failed to delete ToolServiceHealthState twins");
            false
        }
    }
}

async fn delete_persisted_health_state(
    persistence: &HealthPersistenceContext<'_>,
    service_id: &str,
) {
    let escaped_service = escape_graphql_string(service_id);
    let escaped_agent = escape_graphql_string(persistence.node_did);
    let mutation = format!(
        r#"mutation {{
            delete_ToolServiceHealthState(
                filter: {{ _and: [
                    {{ service_id: {{ _eq: "{escaped_service}" }} }},
                    {{ node_did: {{ _eq: "{escaped_agent}" }} }}
                ] }}
            ) {{ _docID }}
        }}"#
    );
    let resp = match persistence.runtime_access() {
        Ok(access) => access
            .write("health_checker.delete_stale_service_state", &mutation)
            .await
            .map_err(anyhow::Error::from),
        Err(error) => Err(error),
    };
    if let Err(error) = resp {
        tracing::warn!(
            service_id = %service_id,
            error = %error,
            "failed to delete stale ToolServiceHealthState row",
        );
    }
}

async fn load_persisted_service_ids(
    persistence: &HealthPersistenceContext<'_>,
) -> Result<RapidHashSet<String>> {
    let node_did = escape_graphql_string(persistence.node_did);
    let query = format!(
        r#"{{
            ToolServiceHealthState(
                filter: {{ node_did: {{ _eq: "{node_did}" }} }}
            ) {{
                service_id
            }}
        }}"#
    );
    let resp = persistence.runtime_access()?.execute(&query).await?;
    let raw = resp
        .get("data")
        .and_then(|data| data.get("ToolServiceHealthState"))
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    Ok(raw
        .into_iter()
        .filter_map(|value| {
            value
                .get("service_id")
                .and_then(|v| v.as_str())
                .map(str::to_owned)
        })
        .collect())
}

#[cfg(test)]
mod registry_parsing_tests {
    use super::McpHealthCheckService;
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
            "send_node_did": null,
            "updated_at": null,
        });

        let entry: McpHealthCheckService =
            serde_json::from_value(raw).expect("null address fields must parse");

        assert_eq!(entry.service_id, "observability-mcp");
        assert_eq!(entry.hostname, "");
        assert_eq!(entry.tailscale_ip, "");
        assert_eq!(entry.lan_ip, "");
        assert_eq!(entry.mcp_port, Some(9201));
        assert!(!entry.send_node_did);
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

        let entries: Vec<McpHealthCheckService> =
            serde_json::from_value(raw).expect("null lan_ip must not fail the batch parse");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].lan_ip, "");
        assert_eq!(entries[0].tailscale_ip, "100.69.4.79");
    }
}

#[cfg(test)]
mod tests {
    use chrono::Duration as ChronoDuration;
    use defra_node::Node as EmbeddedNode;
    use rmcp::model::ListToolsResult;
    use serde_json::Value;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    use super::{
        backoff_duration, run_health_check, run_health_check_cycle, step_service,
        HealthCheckerOptions, HealthPersistenceContext, HealthStateInternal, McpHealthCheckService,
        ProbeEvent, ServiceHealth, ServiceHealthMap, ServiceModelInternal,
    };
    use crate::lean_vocab_test::{lean_mcp_health_cases, LeanMcpHealthCase};
    use crate::store::config_client::ConfigAccess;
    use crate::store::identity::NodeIdentity;
    use crate::tools::mcp::pool::McpPool;

    #[tokio::test]
    async fn service_health_publication_preserves_native_ownership_and_revocation(
    ) -> anyhow::Result<()> {
        use crate::lean_vocab_test::{lean_identity_household_cases, LeanIdentityAction};
        use crate::store::config_client::{GraphqlEndpoint, NodePermission};
        use crate::NodeIdentity;
        use anyhow::Context;

        let model = lean_identity_household_cases()
            .iter()
            .find(|case| case.name == "operations-parent-publication")
            .context("missing operations model")?;
        let expected: Vec<_> = model
            .actions
            .iter()
            .filter_map(|action| match action {
                LeanIdentityAction::Check { expected, .. } => Some(*expected),
                _ => None,
            })
            .collect();
        let dir = tempfile::tempdir()?;
        let user = crate::KeyIdentity::load_or_create(dir.path().join("user"), None)?;
        let runtime = crate::KeyIdentity::load_or_create(dir.path().join("runtime"), None)?;
        let agent = crate::KeyIdentity::load_or_create(dir.path().join("agent"), None)?;
        let sibling = crate::KeyIdentity::load_or_create(dir.path().join("sibling"), None)?;
        let outsider = crate::KeyIdentity::load_or_create(dir.path().join("outsider"), None)?;
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        let node = Arc::new(
            EmbeddedNode::builder()
                .with_node_identity_did(runtime.did())
                .with_node_acp_enabled()
                .with_http(defra_node::HttpConfig::with_listener(listener)?)
                .with_storage_backend(defra_node::StorageBackend::Regolith)
                .data_path(dir.path().join("node"))
                .build()
                .await?,
        );
        node.add_dac_policy_with_counter_as(
            &defra_core::RequestContext::default().with_identity(Some(runtime.did().to_owned())),
            gents_protocol::schemas::AGENT_OWNERSHIP_POLICY,
            0,
        )
        .await?;
        node.add_collection(gents_protocol::schemas::NODE).await?;
        node.add_collection(gents_protocol::schemas::TOOL_SERVICE_HEALTH_STATE)
            .await?;
        let access = |actor: &crate::KeyIdentity| -> anyhow::Result<ConfigAccess> {
            Ok(ConfigAccess::Graphql(
                GraphqlEndpoint::as_principal(
                    format!("http://{address}/api/v0/graphql"),
                    actor.did(),
                )
                .with_client_signing_capability(actor.client_signing_capability()?)?,
            ))
        };
        let user_access = access(&user)?;
        let runtime_access =
            ConfigAccess::local_as(node.clone(), identity::Did::new(runtime.did())?);
        for actor in [&user, &agent, &sibling, &outsider] {
            for permission in [
                NodePermission::DocumentRead,
                NodePermission::DocumentUpdate,
                NodePermission::DacRelationAdd,
            ] {
                runtime_access
                    .grant_node_permission(&identity::Did::new(actor.did())?, permission)
                    .await?;
            }
        }
        let parent = user_access
            .write(
                "test.health.node",
                &format!(
                    r#"mutation {{ create_Node(input: {{node_did: "{}"}}) {{_docID}} }}"#,
                    crate::store::graphql::escape_graphql_string(runtime.did()),
                ),
            )
            .await?;
        let parent = gents_protocol::graphql::extract_mutation_doc_id(&parent, "Node")?;
        user_access
            .add_actor_relationship(
                "Node",
                &parent,
                "runtime_reader",
                &identity::Did::new(runtime.did())?,
            )
            .await?;
        let context = HealthPersistenceContext {
            node: &node,
            node_did: runtime.did(),
        };
        let now = chrono::Utc::now();
        let entry = super::MCPServiceHealthSnapshot {
            service_id: "service".into(),
            endpoint: Some("http://localhost/mcp".into()),
            status: "healthy".into(),
            tool_count: Some(2),
            failure_count: 0,
            k_max: 3,
            backoff_until: None,
            last_probe_at: now,
            last_seen: now,
            last_error: None,
        };
        super::upsert_persisted_health_state(&context, None, &entry, now).await?;
        let visible = |response: &serde_json::Value| {
            !gents_protocol::graphql::graphql_rows_from_response(response, "ToolServiceHealthState")
                .is_empty()
        };
        let query = "{ ToolServiceHealthState { _docID status } }";
        let user_view = user_access.execute(query).await?;
        let doc = user_view["data"]["ToolServiceHealthState"][0]["_docID"]
            .as_str()
            .context("health not visible to Node owner")?;
        let update = format!(
            r#"mutation {{ update_ToolServiceHealthState(docID: "{}", input: {{status: "unreachable"}}) {{_docID}} }}"#,
            crate::store::graphql::escape_graphql_string(doc)
        );
        let affected = |result: anyhow::Result<serde_json::Value>| {
            result.is_ok_and(|response| {
                gents_protocol::graphql::extract_mutation_doc_id(
                    &response,
                    "ToolServiceHealthState",
                )
                .is_ok()
            })
        };
        let actual = [
            visible(&user_view),
            affected(
                user_access
                    .write("test.health.denied", &update)
                    .await
                    .map_err(Into::into),
            ),
            affected(
                runtime_access
                    .write("test.health.update", &update)
                    .await
                    .map_err(Into::into),
            ),
            visible(&access(&agent)?.execute(query).await?),
            visible(&access(&sibling)?.execute(query).await?),
            affected(
                access(&outsider)?
                    .write("test.health.outsider", &update)
                    .await
                    .map_err(Into::into),
            ),
        ];
        assert_eq!(
            actual,
            [
                expected[2],
                expected[4],
                expected[5],
                expected[6],
                expected[7],
                expected[8]
            ]
        );
        runtime_access
            .delete_resource_relationship("ToolServiceHealthState", doc, "parent", "nodes", &parent)
            .await?;
        let republished = super::upsert_persisted_health_state(&context, None, &entry, now).await;
        assert_eq!(republished.is_ok(), expected[10]);
        republished?;
        assert_eq!(visible(&user_access.execute(query).await?), expected[9]);
        assert!(super::load_persisted_service_ids(&context)
            .await?
            .contains("service"));
        drop(user_access);
        drop(runtime_access);
        node.shutdown().await?;
        drop(node);
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        let restarted = Arc::new(
            EmbeddedNode::builder()
                .with_node_identity_did(runtime.did())
                .with_node_acp_enabled()
                .with_http(defra_node::HttpConfig::with_listener(listener)?)
                .with_storage_backend(defra_node::StorageBackend::Regolith)
                .data_path(dir.path().join("node"))
                .build()
                .await?,
        );
        let user_access = ConfigAccess::Graphql(
            GraphqlEndpoint::as_principal(format!("http://{address}/api/v0/graphql"), user.did())
                .with_client_signing_capability(user.client_signing_capability()?)?,
        );
        let context = HealthPersistenceContext {
            node: &restarted,
            node_did: runtime.did(),
        };
        assert!(super::load_persisted_service_ids(&context)
            .await?
            .contains("service"));
        super::upsert_persisted_health_state(&context, None, &entry, now).await?;
        assert!(!visible(&user_access.execute(query).await?));
        restarted.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn health_persistence_skips_unchanged_rows_and_only_commits_changed_fields() {
        fn runtime_reader(node: &Arc<EmbeddedNode>) -> ConfigAccess {
            ConfigAccess::local_as(
                node.clone(),
                identity::Did::new(node.node_identity_did().unwrap()).unwrap(),
            )
        }
        async fn field_commits(
            node: &Arc<EmbeddedNode>,
        ) -> std::collections::BTreeMap<String, usize> {
            let access = runtime_reader(node);
            let rows = access
                .execute("{ ToolServiceHealthState { _docID } }")
                .await
                .unwrap();
            let doc_id = rows["data"]["ToolServiceHealthState"][0]["_docID"]
                .as_str()
                .unwrap();
            let doc_id = crate::store::graphql::escape_graphql_string(doc_id);
            let response = access
                .execute(&format!(
                    r#"{{ _commits(docID: "{doc_id}") {{ fieldName }} }}"#
                ))
                .await
                .unwrap();
            let mut counts = std::collections::BTreeMap::new();
            for commit in response["data"]["_commits"].as_array().unwrap() {
                if let Some(field) = commit["fieldName"].as_str() {
                    *counts.entry(field.to_owned()).or_default() += 1;
                }
            }
            counts
        }

        let (node, runtime) = crate::tools::mcp::meta::tests::test_node().await;
        let map = ServiceHealthMap::new();
        let context = HealthPersistenceContext {
            node: &node,
            node_did: runtime.did(),
        };
        let now = chrono::Utc::now();
        let mut entry = super::MCPServiceHealthSnapshot {
            service_id: "service".into(),
            endpoint: Some("http://localhost/mcp".into()),
            status: "healthy".into(),
            tool_count: Some(2),
            failure_count: 0,
            k_max: 3,
            backoff_until: None,
            last_probe_at: now,
            last_seen: now,
            last_error: None,
        };
        super::persist_health_snapshot(&context, &map, &[entry.clone()], now).await;
        let initial = field_commits(&node).await;
        assert_eq!(initial.get("status"), Some(&1));

        let later = now + ChronoDuration::seconds(30);
        super::persist_health_snapshot(&context, &map, &[entry.clone()], later).await;
        assert_eq!(
            field_commits(&node).await,
            initial,
            "unchanged backoff observation must not write"
        );

        entry.last_probe_at = later;
        entry.last_seen = later;
        super::persist_health_snapshot(&context, &map, &[entry.clone()], later).await;
        let probed = field_commits(&node).await;
        for field in ["status", "endpoint", "tool_count", "failure_count", "k_max"] {
            assert_eq!(
                probed.get(field),
                initial.get(field),
                "unchanged {field} was rewritten"
            );
        }
        for field in ["last_probe_at", "last_seen", "updated_at"] {
            assert_eq!(
                probed.get(field),
                Some(&(initial[field] + 1)),
                "fresh {field} was not published"
            );
        }

        let unavailable = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        let failed_context = HealthPersistenceContext {
            node: &unavailable,
            node_did: context.node_did,
        };
        entry.status = "stale".into();
        entry.last_error = Some("connection refused".into());
        super::persist_health_snapshot(&failed_context, &map, &[entry.clone()], later).await;
        assert!(map.persisted.lock().await.is_empty());
        unavailable.shutdown().await.expect("shut down test node");
        crate::store::config_client::ConfigAccess::write_local(
            &node,
            "test.health_repair",
            r#"mutation { update_ToolServiceHealthState(input: {endpoint: "http://stale.invalid/mcp"}) { _docID } }"#,
        )
        .await
        .unwrap();
        let unconfirmed = field_commits(&node).await;
        super::persist_health_snapshot(&context, &map, &[entry.clone()], later).await;
        let repaired = field_commits(&node).await;
        assert_eq!(
            repaired["endpoint"],
            unconfirmed["endpoint"] + 1,
            "unconfirmed publish must repair every field"
        );
        let row = runtime_reader(&node)
            .execute("{ ToolServiceHealthState { endpoint status last_error_class } }")
            .await
            .unwrap();
        assert_eq!(
            row["data"]["ToolServiceHealthState"][0]["endpoint"],
            "http://localhost/mcp"
        );
        assert_eq!(row["data"]["ToolServiceHealthState"][0]["status"], "stale");
        assert_eq!(
            row["data"]["ToolServiceHealthState"][0]["last_error_class"],
            "connection_refused"
        );
        node.shutdown().await.expect("shut down test node");
    }

    fn health_state_from_lean(case: &LeanMcpHealthCase, state: &str) -> HealthStateInternal {
        HealthStateInternal::parse(state).unwrap_or_else(|_| {
            panic!(
                "Lean MCP health case {} produced unknown state {:?}",
                case.name, state
            )
        })
    }

    fn health_state_name(state: HealthStateInternal) -> &'static str {
        state.as_str()
    }

    fn probe_event_from_lean(case: &LeanMcpHealthCase) -> ProbeEvent {
        match case.event.as_str() {
            "probeSuccessFresh" => ProbeEvent::ProbeSuccess { stale: false },
            "probeSuccessStale" => ProbeEvent::ProbeSuccess { stale: true },
            "probeFail" => ProbeEvent::ProbeFail,
            "backoffExpiry" => ProbeEvent::BackoffExpiry,
            "registryAbsent" => ProbeEvent::RegistryAbsent,
            other => panic!(
                "Lean MCP health case {} produced unknown event {:?}",
                case.name, other
            ),
        }
    }

    fn registry_entry(
        service_id: &str,
        updated_at: chrono::DateTime<chrono::Utc>,
    ) -> McpHealthCheckService {
        McpHealthCheckService {
            service_id: service_id.to_string(),
            hostname: "remote-host".to_string(),
            tailscale_ip: "100.64.0.1".to_string(),
            lan_ip: String::new(),
            mcp_port: Some(9201),
            mcp_path: "/mcp".to_string(),
            send_node_did: false,
            updated_at: Some(updated_at.to_rfc3339()),
        }
    }

    #[test]
    fn generated_mcp_health_cases_match_health_checker_transitions() {
        let cases = lean_mcp_health_cases();
        assert!(
            !cases.is_empty(),
            "Lean must emit at least one MCP health case"
        );

        for case in cases {
            let now = chrono::Utc::now();
            let options = HealthCheckerOptions {
                failure_threshold_k: case.threshold_k.try_into().unwrap(),
                ..Default::default()
            };
            let start_model = ServiceModelInternal {
                state: health_state_from_lean(case, &case.start_state),
                failure_count: case.start_count.try_into().unwrap(),
                backoff_until: None,
            };

            let actual = step_service(start_model, probe_event_from_lean(case), now, &options);
            let actual_projection = actual
                .as_ref()
                .map(|model| model.state.project().to_string());
            assert_eq!(
                actual_projection.as_deref(),
                case.rust_projection.as_deref(),
                "Lean MCP health case {} must match Rust HealthStatus projection",
                case.name
            );

            match (case.next_state.as_deref(), case.next_count, actual) {
                (None, None, None) => {}
                (Some(expected_state), Some(expected_count), Some(actual_model)) => {
                    assert_eq!(
                        health_state_name(actual_model.state),
                        expected_state,
                        "Lean MCP health case {} must match next_state",
                        case.name
                    );
                    assert_eq!(
                        actual_model.failure_count as usize, expected_count,
                        "Lean MCP health case {} must match next_count",
                        case.name
                    );
                }
                other => panic!(
                    "Lean MCP health case {} produced mismatched removal/count shape: {:?}",
                    case.name, other
                ),
            }
        }
    }

    #[tokio::test]
    async fn mcp_health_cycle_applies_k_threshold_and_backoff() {
        const SERVICE_ID: &str = "lean-mcp-health-service";

        let now = chrono::Utc::now();
        let options = HealthCheckerOptions {
            failure_threshold_k: 3,
            probe_timeout: std::time::Duration::from_secs(1),
            backoff_initial: std::time::Duration::from_secs(30),
            backoff_max: std::time::Duration::from_secs(60),
            ..Default::default()
        };
        let handler_calls = Arc::new(AtomicUsize::new(0));
        let probe_succeeds = Arc::new(AtomicBool::new(false));
        let pool = {
            let handler_calls = Arc::clone(&handler_calls);
            let probe_succeeds = Arc::clone(&probe_succeeds);
            McpPool::new_with_list_tools_handler(move |_service_id, _endpoint| {
                let handler_calls = Arc::clone(&handler_calls);
                let probe_succeeds = Arc::clone(&probe_succeeds);
                async move {
                    handler_calls.fetch_add(1, Ordering::SeqCst);
                    if probe_succeeds.load(Ordering::SeqCst) {
                        Ok(ListToolsResult::default())
                    } else {
                        anyhow::bail!("synthetic MCP health probe failure")
                    }
                }
            })
        };
        let health_map = ServiceHealthMap::new();

        for offset in 0..3 {
            let cycle_now = now + ChronoDuration::seconds(offset);
            run_health_check_cycle(
                vec![registry_entry(SERVICE_ID, cycle_now)],
                cycle_now,
                &pool,
                &health_map,
                "local-host",
                None,
                &options,
                None,
            )
            .await
            .unwrap();

            let entry = health_map.get_entry(SERVICE_ID).await.unwrap();
            assert_eq!(entry.model.failure_count, offset as u32 + 1);
            if offset < 2 {
                assert_eq!(entry.health.status, super::HealthStatus::Stale);
                assert_eq!(entry.model.backoff_until, None);
            } else {
                assert_eq!(entry.health.status, super::HealthStatus::Unreachable);
                assert_eq!(
                    entry.model.backoff_until,
                    Some(cycle_now + ChronoDuration::seconds(30))
                );
            }
        }

        let calls_after_eviction = handler_calls.load(Ordering::SeqCst);
        let during_backoff = now + ChronoDuration::seconds(12);
        run_health_check_cycle(
            vec![registry_entry(SERVICE_ID, during_backoff)],
            during_backoff,
            &pool,
            &health_map,
            "local-host",
            None,
            &options,
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            handler_calls.load(Ordering::SeqCst),
            calls_after_eviction,
            "health checker must suppress probes while backoff is active"
        );

        let expired = now + ChronoDuration::seconds(32);
        run_health_check_cycle(
            vec![registry_entry(SERVICE_ID, expired)],
            expired,
            &pool,
            &health_map,
            "local-host",
            None,
            &options,
            None,
        )
        .await
        .unwrap();
        let entry = health_map.get_entry(SERVICE_ID).await.unwrap();
        assert_eq!(entry.health.status, super::HealthStatus::Unreachable);
        assert_eq!(entry.model.failure_count, 4);
        assert_eq!(
            entry.model.backoff_until,
            Some(expired + ChronoDuration::seconds(60))
        );

        probe_succeeds.store(true, Ordering::SeqCst);
        let recovered = expired + ChronoDuration::seconds(60);
        run_health_check_cycle(
            vec![registry_entry(SERVICE_ID, recovered)],
            recovered,
            &pool,
            &health_map,
            "local-host",
            None,
            &options,
            None,
        )
        .await
        .unwrap();
        let entry = health_map.get_entry(SERVICE_ID).await.unwrap();
        assert_eq!(entry.health.status, super::HealthStatus::Healthy);
        assert_eq!(entry.model.failure_count, 0);
        assert_eq!(entry.model.backoff_until, None);

        assert_eq!(
            backoff_duration(0, &options),
            std::time::Duration::from_secs(30)
        );
        assert_eq!(
            backoff_duration(1, &options),
            std::time::Duration::from_secs(60)
        );
        assert_eq!(
            backoff_duration(9, &options),
            std::time::Duration::from_secs(60)
        );
    }

    #[tokio::test]
    async fn mcp_health_cycle_drops_registry_absent_services() {
        let now = chrono::Utc::now();
        let health_map = ServiceHealthMap::new();
        health_map
            .set_for_test(
                "removed-service",
                ServiceHealth {
                    status: super::HealthStatus::Healthy,
                    last_seen: now,
                    last_error: None,
                },
            )
            .await;

        run_health_check_cycle(
            Vec::new(),
            now,
            &McpPool::new(),
            &health_map,
            "local-host",
            None,
            &HealthCheckerOptions::default(),
            None,
        )
        .await
        .unwrap();
        assert!(health_map.get("removed-service").await.is_none());
    }

    #[tokio::test]
    async fn persisted_health_rows_are_node_scoped_and_stale_rows_sweep_within_the_agent() {
        let (node_a, _root_a) = crate::tools::mcp::meta::tests::test_node().await;
        let (node_b, _root_b) = crate::tools::mcp::meta::tests::test_node().await;
        for node in [&node_a, &node_b] {
            crate::tools::mcp::meta::tests::seed_registry_service(
                node,
                "observability-mcp",
                "local-host",
                9213,
            )
            .await;
        }
        let pool =
            McpPool::new_with_list_tools_handler(|_, _| async { Ok(ListToolsResult::default()) });
        async fn rows(node: &Arc<EmbeddedNode>) -> Vec<Value> {
            let actor = identity::Did::new(node.node_identity_did().unwrap()).unwrap();
            let access = crate::ConfigAccess::local_as(node.clone(), actor);
            access.execute("{ ToolServiceHealthState { _docID service_id node_did status tool_count failure_count k_max endpoint last_error_class last_error_message } }")
                .await.unwrap()["data"]["ToolServiceHealthState"].as_array().unwrap().clone()
        }
        let mut original_a = None;
        for node in [&node_a, &node_b, &node_a] {
            let did = node.node_identity_did().unwrap();
            run_health_check(
                node,
                &pool,
                &ServiceHealthMap::new(),
                "local-host",
                None,
                &HealthCheckerOptions::default(),
                Some(HealthPersistenceContext {
                    node,
                    node_did: did,
                }),
            )
            .await
            .unwrap();
            let current = rows(node).await;
            assert_eq!(current.len(), 1);
            assert_eq!(current[0]["node_did"], did);
            assert_eq!(current[0]["service_id"], "observability-mcp");
            assert_eq!(current[0]["status"], "healthy");
            assert_eq!(current[0]["tool_count"], 0);
            assert_eq!(current[0]["failure_count"], 0);
            assert_eq!(
                current[0]["k_max"],
                HealthCheckerOptions::default().failure_threshold_k
            );
            assert_eq!(current[0]["last_error_class"], Value::Null);
            assert_eq!(current[0]["last_error_message"], Value::Null);
            if Arc::ptr_eq(node, &node_a) {
                if let Some(original) = &original_a {
                    assert_eq!(&current[0]["_docID"], original);
                } else {
                    original_a = Some(current[0]["_docID"].clone());
                }
            }
        }
        let did = node_a.node_identity_did().unwrap();
        let access =
            crate::ConfigAccess::local_as(node_a.clone(), identity::Did::new(did).unwrap());
        access.write("test.health.disable", r#"mutation { update_ToolServiceRegistry(filter: {service_id:{_eq:"observability-mcp"}}, input: {enabled:false}) {_docID} }"#).await.unwrap();
        run_health_check(
            &node_a,
            &pool,
            &ServiceHealthMap::new(),
            "local-host",
            None,
            &HealthCheckerOptions::default(),
            Some(HealthPersistenceContext {
                node: &node_a,
                node_did: did,
            }),
        )
        .await
        .unwrap();
        assert!(rows(&node_a).await.is_empty());
        assert_eq!(rows(&node_b).await.len(), 1);
        node_a.shutdown().await.unwrap();
        node_b.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn queued_health_probe_rechecks_service_visibility_and_route() {
        for revoke in [false, true] {
            let (node, _runtime) = crate::tools::mcp::meta::tests::test_node().await;
            crate::tools::mcp::meta::tests::seed_registry_service(&node, "a", "first", 9001).await;
            crate::tools::mcp::meta::tests::seed_registry_service(&node, "b", "old", 9002).await;
            let did = node.node_identity_did().unwrap();
            let access =
                crate::ConfigAccess::local_as(node.clone(), identity::Did::new(did).unwrap());
            let mut services =
                crate::tools::mcp::registry::configured_mcp_services_on(&access, did)
                    .await
                    .unwrap()
                    .into_iter()
                    .map(McpHealthCheckService::from)
                    .collect::<Vec<_>>();
            services.sort_by(|a, b| a.service_id.cmp(&b.service_id));
            let observations = Arc::new(std::sync::Mutex::new(Vec::new()));
            let observed = observations.clone();
            let writer = access.clone();
            let pool = McpPool::new_with_list_tools_handler(move |service, endpoint| {
                let (writer, observed) = (writer.clone(), observed.clone());
                async move {
                    observed.lock().unwrap().push((service.clone(), endpoint));
                    if service == "a" {
                        let mutation = if revoke {
                            r#"mutation { update_ToolServiceRegistry(filter:{service_id:{_eq:"b"}}, input:{enabled:false}) {_docID} }"#
                        } else {
                            r#"mutation { update_ToolServiceRegistry(filter:{service_id:{_eq:"b"}}, input:{hostname:"new"}) {_docID} }"#
                        };
                        writer.write("test.health.rebind", mutation).await?;
                    }
                    Ok(ListToolsResult::default())
                }
            });
            super::run_health_check_cycle_with_cancel(
                services,
                chrono::Utc::now(),
                &pool,
                &ServiceHealthMap::new(),
                "local",
                None,
                &HealthCheckerOptions::default(),
                Some(HealthPersistenceContext {
                    node: &node,
                    node_did: did,
                }),
                Some(did),
                None,
            )
            .await
            .unwrap();
            {
                let seen = observations.lock().unwrap();
                assert_eq!(seen[0].0, "a");
                if revoke {
                    assert_eq!(seen.len(), 1);
                } else {
                    assert_eq!(seen.len(), 2);
                    assert_eq!(seen[1].0, "b");
                    assert_eq!(seen[1].1, "http://new:9002/mcp");
                }
            }
            node.shutdown().await.unwrap();
        }
    }

    #[tokio::test]
    async fn client_probe_sends_the_actual_actor_only_when_enabled() {
        use crate::NodeIdentity;
        let directory = tempfile::tempdir().unwrap();
        let client =
            crate::KeyIdentity::load_or_create(directory.path().join("client"), None).unwrap();
        let (endpoint, requests) =
            crate::tools::mcp::pool::tests::spawn_header_capture_mcp_server().await;
        let endpoint = reqwest::Url::parse(&endpoint).unwrap();
        let service = McpHealthCheckService {
            service_id: "client-service".into(),
            hostname: endpoint.host_str().unwrap().into(),
            tailscale_ip: String::new(),
            lan_ip: String::new(),
            mcp_port: endpoint.port(),
            mcp_path: endpoint.path().into(),
            send_node_did: true,
            updated_at: None,
        };
        let options = HealthCheckerOptions::default();
        super::run_health_check_cycle_for_actor(
            vec![service.clone()],
            chrono::Utc::now(),
            &McpPool::new(),
            &ServiceHealthMap::new(),
            "local",
            None,
            &options,
            client.did(),
        )
        .await
        .unwrap();
        {
            let captured = requests.lock().unwrap();
            assert!(captured
                .iter()
                .any(|request| request.method == "tools/list"));
            assert!(captured
                .iter()
                .all(|request| request.node_did_header.as_deref() == Some(client.did())));
        }
        let before = requests.lock().unwrap().len();
        assert!(super::run_health_check_cycle(
            vec![service.clone()],
            chrono::Utc::now(),
            &McpPool::new(),
            &ServiceHealthMap::new(),
            "local",
            None,
            &options,
            None
        )
        .await
        .is_err());
        assert_eq!(requests.lock().unwrap().len(), before);
        let mut opt_out = service;
        opt_out.send_node_did = false;
        super::run_health_check_cycle_for_actor(
            vec![opt_out],
            chrono::Utc::now(),
            &McpPool::new(),
            &ServiceHealthMap::new(),
            "local",
            None,
            &options,
            client.did(),
        )
        .await
        .unwrap();
        let captured = requests.lock().unwrap();
        assert!(captured.len() > before);
        assert!(captured[before..]
            .iter()
            .all(|request| request.node_did_header.is_none()));
    }

    #[tokio::test]
    async fn persistence_collapses_twins_to_the_row_readers_select() {
        let (node, runtime) = crate::tools::mcp::meta::tests::test_node().await;
        let node_did = runtime.did();
        let mut created = std::collections::BTreeMap::<&str, Vec<String>>::new();
        for (service, updated_at, endpoint) in [
            ("newest", "2026-01-01T00:00:00Z", "a"),
            ("newest", "2026-01-02T00:00:00Z", "b"),
            ("tied", "2026-01-01T00:00:00Z", "a"),
            ("tied", "2026-01-01T00:00:00Z", "b"),
        ] {
            let response = crate::store::config_client::ConfigAccess::write_local_response(
                &node,
                "test.health_twin",
                &format!(
                    r#"mutation {{ add_ToolServiceHealthState(input: {{service_id: "{service}", node_did: "{node_did}", status: "healthy", endpoint: "{endpoint}", updated_at: "{updated_at}"}}) {{ _docID }} }}"#
                ),
            )
            .await
            .unwrap();
            let data = response.data.unwrap();
            let doc_id = data["add_ToolServiceHealthState"][0]["_docID"]
                .as_str()
                .unwrap_or_else(|| panic!("add returned no _docID: {data}"));
            created.entry(service).or_default().push(doc_id.to_owned());
        }
        let reader_order = |node: Arc<EmbeddedNode>| async move {
            let actor = identity::Did::new(node.node_identity_did().unwrap()).unwrap();
            let response = ConfigAccess::local_as(node, actor)
                .execute(
                    r#"{ ToolServiceHealthState(order: [{ service_id: ASC }, { updated_at: DESC }, { _docID: ASC }]) { _docID service_id status } }"#,
                )
                .await
                .unwrap();
            response["data"]["ToolServiceHealthState"]
                .as_array()
                .unwrap()
                .clone()
        };
        let expected_newest = created["newest"][1].clone();
        let expected_tied = created["tied"].iter().min().unwrap().clone();
        let before = reader_order(node.clone()).await;
        assert_eq!(before[0]["_docID"], expected_newest.as_str());
        assert_eq!(before[2]["_docID"], expected_tied.as_str());

        let now = chrono::Utc::now();
        let snapshot = ["newest", "tied"].map(|service| super::MCPServiceHealthSnapshot {
            service_id: service.into(),
            endpoint: Some("http://localhost/mcp".into()),
            status: "stale".into(),
            tool_count: Some(1),
            failure_count: 1,
            k_max: 3,
            backoff_until: None,
            last_probe_at: now,
            last_seen: now,
            last_error: Some("connection refused".into()),
        });
        let map = ServiceHealthMap::new();
        let context = HealthPersistenceContext {
            node: &node,
            node_did,
        };
        super::persist_health_snapshot(&context, &map, &snapshot, now).await;

        let after = reader_order(node.clone()).await;
        assert_eq!(after.len(), 2, "{after:?}");
        assert_eq!(after[0]["_docID"], expected_newest.as_str());
        assert_eq!(after[1]["_docID"], expected_tied.as_str());
        assert!(after.iter().all(|row| row["status"] == "stale"));
        assert_eq!(map.persisted.lock().await.len(), 2);
        node.shutdown().await.expect("shut down test node");
    }
    #[tokio::test]
    async fn probe_shutdown_cancels_pending_mcp_without_pruning_or_failure() {
        let started = Arc::new(tokio::sync::Notify::new());
        let seen = started.clone();
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        let pool = McpPool::new_with_list_tools_handler(move |_, _| {
            let seen = seen.clone();
            let counted = counted.clone();
            async move {
                counted.fetch_add(1, Ordering::SeqCst);
                seen.notify_one();
                std::future::pending::<anyhow::Result<ListToolsResult>>().await
            }
        });
        let health = ServiceHealthMap::new();
        let now = chrono::Utc::now();
        for id in ["pending", "unobserved"] {
            health
                .set_entry(
                    id.into(),
                    super::ServiceHealthEntry::from_public(ServiceHealth {
                        status: super::HealthStatus::Healthy,
                        last_seen: now,
                        last_error: None,
                    }),
                )
                .await;
        }
        let options = HealthCheckerOptions {
            probe_timeout: std::time::Duration::from_secs(60),
            ..Default::default()
        };
        let before = health.snapshot_full(options.failure_threshold_k).await;
        let cancel = tokio_util::sync::CancellationToken::new();
        let run_cancel = cancel.clone();
        let run_health = health.clone();
        let handle = tokio::spawn(async move {
            super::run_health_check_cycle_with_cancel(
                vec![registry_entry("pending", now), registry_entry("later", now)],
                now,
                &pool,
                &run_health,
                "local-host",
                None,
                &options,
                None,
                None,
                Some(&run_cancel),
            )
            .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), started.notified())
            .await
            .unwrap();
        cancel.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(1), handle)
            .await
            .expect("pending MCP observation must stop on shutdown")
            .unwrap()
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(health.snapshot_full(3).await, before);
    }

    #[tokio::test]
    async fn probe_shutdown_prevents_new_mcp_probes_and_failure_events() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        let pool = McpPool::new_with_list_tools_handler(move |_, _| {
            counted.fetch_add(1, Ordering::SeqCst);
            async { anyhow::bail!("must not probe after shutdown") }
        });
        let cancel = tokio_util::sync::CancellationToken::new();
        cancel.cancel();
        let node = crate::inference::auth::credential::test_support::test_node().await;
        let health = ServiceHealthMap::new();
        health
            .set_entry(
                "unobserved".into(),
                super::ServiceHealthEntry::from_public(ServiceHealth {
                    status: super::HealthStatus::Healthy,
                    last_seen: chrono::Utc::now(),
                    last_error: None,
                }),
            )
            .await;
        let before = health.snapshot_full(3).await;
        let handle = super::spawn_health_checker(
            node.clone(),
            pool,
            health.clone(),
            "local-host".into(),
            None,
            cancel,
            HealthCheckerOptions::default(),
            "did:key:owner".into(),
        );
        tokio::time::timeout(std::time::Duration::from_secs(1), handle)
            .await
            .expect("an already-cancelled health worker must not start a cycle")
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(health.snapshot_full(3).await, before);
        node.shutdown().await.expect("shut down test node");
    }

    #[tokio::test]
    async fn probe_shutdown_wins_a_ready_mcp_failure_without_a_false_outage() {
        let cancel = tokio_util::sync::CancellationToken::new();
        let from_probe = cancel.clone();
        let pool = McpPool::new_with_list_tools_handler(move |_, _| {
            from_probe.cancel();
            async { anyhow::bail!("transport ended during shutdown") }
        });
        let health = ServiceHealthMap::new();
        super::run_health_check_cycle_with_cancel(
            vec![registry_entry("pending", chrono::Utc::now())],
            chrono::Utc::now(),
            &pool,
            &health,
            "local-host",
            None,
            &HealthCheckerOptions::default(),
            None,
            None,
            Some(&cancel),
        )
        .await
        .unwrap();
        assert!(health.snapshot().await.is_empty());
    }
}
