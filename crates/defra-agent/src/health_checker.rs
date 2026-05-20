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
use defra_agent_protocol::graphql::escape_graphql_string;
use defra_node::EmbeddedNode;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::mcp_pool::{resolve_mcp_url, McpPool};

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

#[derive(Debug, Clone)]
struct ServiceHealthEntry {
    health: ServiceHealth,
    model: ServiceModelInternal,
    endpoint: Option<String>,
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
            last_probe_at,
        }
    }

    fn from_model(
        model: ServiceModelInternal,
        last_seen: DateTime<Utc>,
        last_error: Option<String>,
        endpoint: Option<String>,
        last_probe_at: DateTime<Utc>,
    ) -> Self {
        Self {
            health: ServiceHealth {
                status: model.state.project(),
                last_seen,
                last_error,
            },
            model,
            endpoint,
            last_probe_at,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HealthStateInternal {
    Healthy,
    Degraded,
    Evicted,
    Reconnecting,
}

impl HealthStateInternal {
    fn project(self) -> HealthStatus {
        match self {
            Self::Healthy => HealthStatus::Healthy,
            Self::Degraded => HealthStatus::Stale,
            Self::Evicted | Self::Reconnecting => HealthStatus::Unreachable,
        }
    }

    /// String form persisted to the `ToolServiceHealthState` DefraDB
    /// collection. Mirrors `HealthState.toDefraDB` in
    /// `Proofs/MCPHealth/State.lean` exactly — including `degraded` rather
    /// than the public `HealthStatus::Stale` projection name. Operators read
    /// the precise internal-state vocabulary so the panel can distinguish
    /// the staleness flavor of degraded (heartbeat lag, `failure_count = 0`)
    /// from the failure-count flavor (`1 <= failure_count < K`).
    ///
    /// `Reconnecting` is reachable only via `ProbeEvent::BackoffExpiry` in
    /// the Lean model; the production cycle does not emit that event today
    /// (the loop skips while backoff is active, then probes directly), so
    /// rows with `status: "reconnecting"` will only appear once the cycle
    /// is extended to bridge the gap. The persisted vocabulary covers it
    /// so future production emission needs no schema change.
    fn to_defradb(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Degraded => "degraded",
            Self::Evicted => "evicted",
            Self::Reconnecting => "reconnecting",
        }
    }
}

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

/// Full MCP service health snapshot exposed to operators (CLI table, desktop
/// panel, and the persisted `ToolServiceHealthState` DefraDB row).
///
/// Carries the K-model fields (`failure_count`, `k_max`, `backoff_until`)
/// that the public `HealthStatus` projection collapses. `status` is the
/// internal `HealthStateInternal` projected to its DefraDB string
/// vocabulary (`healthy` / `stale` / `evicted` / `reconnecting`).
#[derive(Debug, Clone, Serialize)]
pub struct MCPServiceHealthSnapshot {
    pub service_id: String,
    pub endpoint: Option<String>,
    pub status: String,
    pub failure_count: u32,
    pub k_max: u32,
    pub backoff_until: Option<DateTime<Utc>>,
    pub last_probe_at: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub last_error: Option<String>,
}

#[derive(Clone)]
pub struct ServiceHealthMap {
    inner: Arc<RwLock<HashMap<String, ServiceHealthEntry>>>,
}

impl ServiceHealthMap {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn get(&self, service_id: &str) -> Option<ServiceHealth> {
        self.inner
            .read()
            .await
            .get(service_id)
            .map(|entry| entry.health.clone())
    }

    pub async fn snapshot(&self) -> HashMap<String, ServiceHealth> {
        self.inner
            .read()
            .await
            .iter()
            .map(|(service_id, entry)| (service_id.clone(), entry.health.clone()))
            .collect()
    }

    /// Full per-service snapshot including the K-model fields. The
    /// `snapshot()` projection drops `failure_count` / `backoff_until` /
    /// the internal `HealthState`; this one preserves them for operator
    /// surfaces (the desktop panel, the persisted DefraDB row).
    pub async fn snapshot_full(&self, k_max: u32) -> Vec<MCPServiceHealthSnapshot> {
        let k_max = k_max.max(1);
        self.inner
            .read()
            .await
            .iter()
            .map(|(service_id, entry)| MCPServiceHealthSnapshot {
                service_id: service_id.clone(),
                endpoint: entry.endpoint.clone(),
                status: entry.model.state.to_defradb().to_string(),
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

    async fn retain_services(&self, service_ids: &HashSet<String>) {
        self.inner
            .write()
            .await
            .retain(|service_id, _| service_ids.contains(service_id));
    }
}

/// Context for persisting `MCPServiceHealthSnapshot` rows to DefraDB at the
/// end of every health-check cycle. The production `spawn_health_checker`
/// builds one of these per agent; CLI one-shot probes (`defra-agent mcp
/// probe`) and tests pass `None` to skip persistence.
#[derive(Clone, Copy)]
pub struct HealthPersistenceContext<'a> {
    pub node: &'a EmbeddedNode,
    pub agent_did: &'a str,
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

pub fn spawn_health_checker(
    node: Arc<EmbeddedNode>,
    mcp_pool: McpPool,
    health_map: ServiceHealthMap,
    local_hostname: String,
    local_subnet: Option<String>,
    cancel: CancellationToken,
    options: HealthCheckerOptions,
    agent_did: String,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let persistence = HealthPersistenceContext {
            node: node.as_ref(),
            agent_did: agent_did.as_str(),
        };
        if let Err(error) = run_health_check(
            node.as_ref(),
            &mcp_pool,
            &health_map,
            &local_hostname,
            local_subnet.as_deref(),
            &options,
            Some(persistence),
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
                        node: node.as_ref(),
                        agent_did: agent_did.as_str(),
                    };
                    if let Err(error) = run_health_check(
                        node.as_ref(),
                        &mcp_pool,
                        &health_map,
                        &local_hostname,
                        local_subnet.as_deref(),
                        &options,
                        Some(persistence),
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
    options: &HealthCheckerOptions,
    persistence: Option<HealthPersistenceContext<'_>>,
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

    let services: Vec<McpHealthCheckService> =
        serde_json::from_value(raw_services).context("parsing ToolServiceRegistry entries")?;

    run_health_check_cycle(
        services,
        Utc::now(),
        mcp_pool,
        health_map,
        local_hostname,
        local_subnet,
        options,
        persistence,
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
    options: &HealthCheckerOptions,
    persistence: Option<HealthPersistenceContext<'_>>,
) -> Result<()> {
    let mut online_service_ids = HashSet::new();

    for service in services {
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

        // The Lean model (`Proofs/MCPHealth/Transition.lean`) reaches the
        // `Reconnecting` state via `ProbeEvent::BackoffExpiry` between an
        // `Evicted` cycle and the next probe. The production loop does not
        // emit that event today — it skips while backoff is active, then
        // probes directly into `Healthy` / `Degraded` / back into `Evicted`.
        // So persisted rows never carry `status: "reconnecting"` until the
        // cycle is extended to bridge that gap (a future task; tracked
        // alongside the K≥2 design pass in #303). The persisted vocabulary
        // and the desktop panel already cover the state for that point.
        if backoff_is_active(&previous_model, now, options) {
            continue;
        }

        let Some(mcp_port) = service.mcp_port.filter(|port| *port != 0) else {
            apply_probe_failure(
                mcp_pool,
                health_map,
                &service_id,
                previous_model,
                previous_last_seen,
                previous_endpoint,
                "registry entry missing mcp_port".to_string(),
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
                &service_id,
                previous_model,
                previous_last_seen,
                previous_endpoint,
                "registry entry missing address fields".to_string(),
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

        match tokio::time::timeout(
            options.probe_timeout,
            mcp_pool.list_tools(&service_id, &endpoint),
        )
        .await
        {
            Ok(Ok(_)) => {
                let next_model = step_service(
                    previous_model,
                    ProbeEvent::ProbeSuccess { stale: is_stale },
                    now,
                    options,
                )
                .expect("probeSuccess must preserve the service model");
                health_map
                    .set_entry(
                        service_id.clone(),
                        ServiceHealthEntry::from_model(
                            next_model,
                            now,
                            None,
                            Some(endpoint.clone()),
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
                    &service_id,
                    previous_model,
                    previous_last_seen,
                    Some(endpoint.clone()),
                    error.to_string(),
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
                    &service_id,
                    previous_model,
                    previous_last_seen,
                    Some(endpoint.clone()),
                    "probe timed out".to_string(),
                    now,
                    options,
                )
                .await;
            }
        }
    }

    health_map.retain_services(&online_service_ids).await;

    if let Some(persistence) = persistence {
        let snapshot = health_map.snapshot_full(options.failure_threshold_k).await;
        persist_health_snapshot(&persistence, &snapshot, now).await;
        // Source the stale-row set from DefraDB scoped to this agent rather
        // than from the in-memory map: the in-memory entry is dropped by
        // `retain_services` above, and the map is empty on a fresh start, so
        // an in-memory diff misses rows persisted by a previous run (or by
        // an earlier failed delete). Querying the persisted collection makes
        // both restart-after-shutdown and delete-retry-on-error work.
        match load_persisted_service_ids(&persistence).await {
            Ok(persisted_ids) => {
                for service_id in persisted_ids
                    .difference(&online_service_ids)
                    .cloned()
                    .collect::<Vec<_>>()
                {
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

async fn apply_probe_failure(
    mcp_pool: &McpPool,
    health_map: &ServiceHealthMap,
    service_id: &str,
    previous_model: ServiceModelInternal,
    previous_last_seen: DateTime<Utc>,
    endpoint: Option<String>,
    error: String,
    now: DateTime<Utc>,
    options: &HealthCheckerOptions,
) {
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

/// Coarse classification of `last_error` strings persisted alongside the
/// raw message so the panel can render a stable error-class chip
/// independent of the freeform error text. Drift to the message is
/// expected — the class is best-effort.
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
    snapshot: &[MCPServiceHealthSnapshot],
    now: DateTime<Utc>,
) {
    for entry in snapshot {
        if let Err(error) = upsert_persisted_health_state(persistence, entry, now).await {
            tracing::warn!(
                service_id = %entry.service_id,
                error = %error,
                "failed to persist ToolServiceHealthState",
            );
        }
    }
}

async fn upsert_persisted_health_state(
    persistence: &HealthPersistenceContext<'_>,
    entry: &MCPServiceHealthSnapshot,
    now: DateTime<Utc>,
) -> Result<()> {
    let service_id = escape_graphql_string(&entry.service_id);
    let agent_did = escape_graphql_string(persistence.agent_did);
    let endpoint = entry.endpoint.as_deref().unwrap_or("");
    let endpoint = escape_graphql_string(endpoint);
    let status = escape_graphql_string(&entry.status);
    let last_seen = entry.last_seen.to_rfc3339();
    let last_probe_at = entry.last_probe_at.to_rfc3339();
    let updated_at = now.to_rfc3339();
    let backoff_until_fragment = match entry.backoff_until {
        Some(dt) => format!(r#""{}""#, dt.to_rfc3339()),
        None => "null".to_string(),
    };
    let (last_error_class, last_error_message) = match entry.last_error.as_deref() {
        Some(error) => (
            format!(r#""{}""#, classify_last_error(error)),
            format!(r#""{}""#, escape_graphql_string(error)),
        ),
        None => ("null".to_string(), "null".to_string()),
    };

    // The persisted-row identity is the compound (service_id, agent_did) —
    // see the schema comment in
    // crates/defra-agent-protocol/schemas/services/tool_service_health_state.graphql.
    // The upsert filter must match on both so two agents that register the
    // same service_id don't overwrite each other's row.
    //
    // Predictable cost: one upsert per service per health-check cycle.
    // Default `cycle_interval` = 30s + handful of services = far under 1 write/s.
    let mutation = format!(
        r#"mutation {{
            upsert_ToolServiceHealthState(
                filter: {{ _and: [
                    {{ service_id: {{ _eq: "{service_id}" }} }},
                    {{ agent_did: {{ _eq: "{agent_did}" }} }}
                ] }},
                add: {{
                    service_id: "{service_id}",
                    agent_did: "{agent_did}",
                    endpoint: "{endpoint}",
                    status: "{status}",
                    failure_count: {failure_count},
                    k_max: {k_max},
                    backoff_until: {backoff_until_fragment},
                    last_probe_at: "{last_probe_at}",
                    last_seen: "{last_seen}",
                    last_error_class: {last_error_class},
                    last_error_message: {last_error_message},
                    updated_at: "{updated_at}"
                }},
                update: {{
                    endpoint: "{endpoint}",
                    status: "{status}",
                    failure_count: {failure_count},
                    k_max: {k_max},
                    backoff_until: {backoff_until_fragment},
                    last_probe_at: "{last_probe_at}",
                    last_seen: "{last_seen}",
                    last_error_class: {last_error_class},
                    last_error_message: {last_error_message},
                    updated_at: "{updated_at}"
                }}
            ) {{ _docID }}
        }}"#,
        failure_count = entry.failure_count,
        k_max = entry.k_max,
    );

    let resp = persistence.node.execute(&mutation).await;
    if resp.has_errors() {
        anyhow::bail!(
            "upsert_ToolServiceHealthState failed: {:?}",
            resp.errors
        );
    }
    Ok(())
}

async fn delete_persisted_health_state(
    persistence: &HealthPersistenceContext<'_>,
    service_id: &str,
) {
    let escaped_service = escape_graphql_string(service_id);
    let escaped_agent = escape_graphql_string(persistence.agent_did);
    let mutation = format!(
        r#"mutation {{
            delete_ToolServiceHealthState(
                filter: {{ _and: [
                    {{ service_id: {{ _eq: "{escaped_service}" }} }},
                    {{ agent_did: {{ _eq: "{escaped_agent}" }} }}
                ] }}
            ) {{ _docID }}
        }}"#
    );
    let resp = persistence.node.execute(&mutation).await;
    if resp.has_errors() {
        tracing::warn!(
            service_id = %service_id,
            errors = ?resp.errors,
            "failed to delete stale ToolServiceHealthState row",
        );
    }
}

/// Source-of-truth read of every persisted `ToolServiceHealthState`
/// service_id scoped to the writing agent — used by the cycle's stale-row
/// reconciliation so restart-after-shutdown and one-off delete failures
/// converge to the registry's current state.
async fn load_persisted_service_ids(
    persistence: &HealthPersistenceContext<'_>,
) -> Result<HashSet<String>> {
    let agent_did = escape_graphql_string(persistence.agent_did);
    let query = format!(
        r#"{{
            ToolServiceHealthState(
                filter: {{ agent_did: {{ _eq: "{agent_did}" }} }}
            ) {{
                service_id
            }}
        }}"#
    );
    let resp = persistence.node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!(
            "load_persisted_service_ids failed: {:?}",
            resp.errors
        );
    }
    let raw = resp
        .data
        .as_ref()
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

// Inline test module preserved: single-test smoke check, deliberately not extracted to keep it co-located with the narrow code it tests.
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
            "updated_at": null,
        });

        let entry: McpHealthCheckService =
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
    use rmcp::model::ListToolsResult;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    use super::{
        backoff_duration, run_health_check_cycle, step_service, HealthCheckerOptions,
        HealthStateInternal, McpHealthCheckService, ProbeEvent, ServiceHealth, ServiceHealthMap,
        ServiceModelInternal,
    };
    use crate::lean_vocab_test::{lean_mcp_health_cases, LeanMcpHealthCase};
    use crate::mcp_pool::McpPool;

    fn health_state_from_lean(case: &LeanMcpHealthCase, state: &str) -> HealthStateInternal {
        match state {
            "healthy" => HealthStateInternal::Healthy,
            "degraded" => HealthStateInternal::Degraded,
            "evicted" => HealthStateInternal::Evicted,
            "reconnecting" => HealthStateInternal::Reconnecting,
            other => panic!(
                "Lean MCP health case {} produced unknown state {:?}",
                case.name, other
            ),
        }
    }

    fn health_state_name(state: HealthStateInternal) -> &'static str {
        match state {
            HealthStateInternal::Healthy => "healthy",
            HealthStateInternal::Degraded => "degraded",
            HealthStateInternal::Evicted => "evicted",
            HealthStateInternal::Reconnecting => "reconnecting",
        }
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
}
