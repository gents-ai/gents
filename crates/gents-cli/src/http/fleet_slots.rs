use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use gents::tool_call_lifecycle::deadline_is_expired;
use gents::{call_state_holds_backend_slot, UNKNOWN_PROBE_STATUS};
use gents_protocol::row::{
    project_behavior_readiness_summary, AgentBehaviorReadinessRow, AgentRequestRow,
    BehaviorReadinessState, ProjectedBehaviorReadinessSummary,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::post_graphql;

const SNAPSHOT_SOURCE: &str = "graphql.derived_admission_state";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct FleetSlotSnapshot {
    pub(crate) generated_at: String,
    pub(crate) source: String,
    pub(crate) totals: FleetSlotTotals,
    pub(crate) expired: FleetExpiredCounts,
    pub(crate) behaviors: Vec<FleetBehaviorSlotUsage>,
    pub(crate) backends: Vec<FleetBackendAdmissionCounters>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct FleetSlotTotals {
    pub(crate) assigned: i64,
    pub(crate) available: i64,
    pub(crate) max: i64,
    pub(crate) queued: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct FleetExpiredCounts {
    pub(crate) processing_requests: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct FleetBehaviorSlotUsage {
    pub(crate) behavior_id: String,
    pub(crate) agent_did: String,
    pub(crate) backend_id: String,
    pub(crate) configured: bool,
    pub(crate) enabled: bool,
    pub(crate) backend_available: bool,
    pub(crate) assigned: i64,
    pub(crate) available: i64,
    pub(crate) max: i64,
    pub(crate) queued: i64,
    pub(crate) expired_processing: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct FleetBackendAdmissionCounters {
    pub(crate) backend_id: String,
    pub(crate) agent_did: String,
    pub(crate) configured: bool,
    pub(crate) enabled: bool,
    pub(crate) probe_status: String,
    /// Last reported semantic readiness, not current peer connectivity.
    pub(crate) accepting_admission: bool,
    pub(crate) running: i64,
    pub(crate) queued: i64,
    pub(crate) available: i64,
    pub(crate) max_concurrent: i64,
    pub(crate) max_queue_depth: i64,
}

#[derive(Debug, Deserialize)]
struct FleetSlotQueryEnvelope {
    #[serde(rename = "AgentBehavior", default)]
    behaviors: Vec<BehaviorRow>,
    #[serde(rename = "InferenceProfile", default)]
    profiles: Vec<InferenceProfileRow>,
    #[serde(rename = "InferenceBackend", default)]
    backends: Vec<BackendRow>,
    #[serde(rename = "InferenceCall", default)]
    calls: Vec<InferenceCallRow>,
    #[serde(rename = "AgentRequest", default)]
    requests: Vec<AgentRequestRow>,
    #[serde(rename = "AgentBehaviorReadiness", default)]
    behavior_readiness: Vec<AgentBehaviorReadinessRow>,
}

#[derive(Debug, Clone, Deserialize)]
struct BehaviorRow {
    #[serde(default)]
    behavior_id: String,
    #[serde(default)]
    agent_did: String,
    #[serde(default)]
    inference_profile_id: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
}

impl BehaviorRow {
    fn normalized_behavior_id(&self) -> String {
        clean_string(&self.behavior_id)
    }

    fn is_enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }
}

#[derive(Debug, Clone, Deserialize)]
struct InferenceProfileRow {
    #[serde(default)]
    profile_id: String,
    #[serde(default)]
    agent_did: String,
    #[serde(default)]
    backend_id: String,
}

#[derive(Debug, Clone)]
struct ResolvedBehaviorRow {
    behavior: BehaviorRow,
    backend_id: String,
}

impl ResolvedBehaviorRow {
    fn normalized_backend_id(&self) -> String {
        clean_string(&self.backend_id)
    }
}

#[derive(Debug, Clone, Deserialize)]
struct BackendRow {
    #[serde(default)]
    backend_id: String,
    #[serde(default)]
    agent_did: String,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    max_concurrent: Option<i64>,
    #[serde(default)]
    max_queue_depth: Option<i64>,
    #[serde(default)]
    probe_status: Option<String>,
}

impl BackendRow {
    fn normalized_backend_id(&self) -> String {
        clean_string(&self.backend_id)
    }

    fn is_enabled(&self) -> bool {
        self.enabled.unwrap_or(false)
    }

    /// Runtime-observed `probe_status` for display only — never a truth source
    /// for admission (that's the readiness projection; see
    /// `backend_admission_from_readiness`).
    fn display_probe_status(&self) -> String {
        let probe_status = clean_optional_string(self.probe_status.as_deref());
        if probe_status.is_empty() {
            UNKNOWN_PROBE_STATUS.to_string()
        } else {
            probe_status
        }
    }

    fn max_concurrent(&self) -> i64 {
        self.max_concurrent.unwrap_or_default().max(0)
    }

    fn max_queue_depth(&self) -> i64 {
        self.max_queue_depth.unwrap_or_default().max(0)
    }
}

#[derive(Debug, Clone, Deserialize)]
struct InferenceCallRow {
    #[serde(default)]
    backend_id: Option<String>,
    #[serde(default)]
    behavior_id: Option<String>,
    #[serde(default)]
    agent_did: Option<String>,
    #[serde(default)]
    call_state: String,
}

#[derive(Clone, Default)]
struct SlotCounts {
    assigned: i64,
    queued: i64,
    expired_processing: i64,
}

pub(crate) async fn load_fleet_slot_snapshot(graphql: &str) -> Result<FleetSlotSnapshot> {
    let generated_at = Utc::now();
    let response = post_graphql(graphql, fleet_slot_snapshot_query()).await?;
    let envelope = decode_fleet_slot_query_response(response)?;
    Ok(build_fleet_slot_snapshot(generated_at, envelope))
}

fn decode_fleet_slot_query_response(response: Value) -> Result<FleetSlotQueryEnvelope> {
    let data = response
        .get("data")
        .filter(|data| data.is_object())
        .cloned()
        .with_context(|| {
            format!("fleet slot snapshot query response missing object data: {response}")
        })?;
    serde_json::from_value(data).context("decoding fleet slot snapshot query response")
}

fn fleet_slot_snapshot_query() -> &'static str {
    r#"{
        AgentBehavior(order: { behavior_id: ASC }) {
            behavior_id
            agent_did
            inference_profile_id
            enabled
        }
        InferenceProfile(order: { profile_id: ASC }) {
            profile_id
            agent_did
            backend_id
        }
        InferenceBackend(order: { backend_id: ASC }) {
            backend_id
            agent_did
            enabled
            max_concurrent
            max_queue_depth
            probe_status
        }
        InferenceCall(filter: { call_state: { _in: ["queued", "running"] } }) {
            backend_id
            behavior_id
            agent_did
            call_state
        }
        AgentRequest(filter: {
            lifecycle_state: { _eq: "processing" }
        }) {
            request_id
            agent_did
            behavior_id
            deadline
        }
        AgentBehaviorReadiness(order: { agent_did: ASC }) {
            agent_did
            snapshot_json
            updated_at
        }
    }"#
}

fn build_fleet_slot_snapshot(
    generated_at: DateTime<Utc>,
    envelope: FleetSlotQueryEnvelope,
) -> FleetSlotSnapshot {
    let FleetSlotQueryEnvelope {
        behaviors,
        profiles,
        backends,
        calls,
        requests,
        behavior_readiness,
    } = envelope;
    let backends = backends
        .into_iter()
        .filter_map(|backend| {
            let agent_did = clean_string(&backend.agent_did);
            let backend_id = backend.normalized_backend_id();
            (!agent_did.is_empty() && !backend_id.is_empty())
                .then_some(((agent_did, backend_id), backend))
        })
        .collect::<BTreeMap<_, _>>();

    let profile_backends = profiles
        .into_iter()
        .filter_map(|profile| {
            let agent_did = clean_string(&profile.agent_did);
            let profile_id = clean_string(&profile.profile_id);
            let backend_id = clean_string(&profile.backend_id);
            (!agent_did.is_empty() && !profile_id.is_empty() && !backend_id.is_empty())
                .then_some(((agent_did, profile_id), backend_id))
        })
        .collect::<BTreeMap<_, _>>();

    let behaviors = behaviors
        .into_iter()
        .filter_map(|behavior| {
            let agent_did = clean_string(&behavior.agent_did);
            let behavior_id = behavior.normalized_behavior_id();
            if agent_did.is_empty() || behavior_id.is_empty() {
                return None;
            }
            let profile_id = clean_optional_string(behavior.inference_profile_id.as_deref());
            let backend_id = profile_backends
                .get(&(agent_did.clone(), profile_id))
                .cloned()
                .unwrap_or_default();
            Some((
                (agent_did, behavior_id),
                ResolvedBehaviorRow {
                    behavior,
                    backend_id,
                },
            ))
        })
        .collect::<BTreeMap<_, _>>();

    let readiness_by_agent = behavior_readiness
        .into_iter()
        .map(|row| (row.agent_did.clone(), row))
        .collect::<BTreeMap<_, _>>();

    // Measured backend health is never persisted to `InferenceBackend`
    // (#640), so out-of-process readers like this one cannot compute
    // admission from that document's `enabled`/`probe_status` fields —
    // only the runtime that owns the live `BackendHealthMap` can. The
    // per-backend "accepting" flag instead comes from the readiness rows
    // for behaviors bound to that backend, published by each behavior's
    // own runtime.
    let backend_accepting =
        backend_admission_from_readiness(&behaviors, &readiness_by_agent, generated_at);

    let mut backend_counts = BTreeMap::<(String, String), SlotCounts>::new();
    let mut behavior_counts = BTreeMap::<(String, String), SlotCounts>::new();
    let mut active_behavior_backends = BTreeMap::<(String, String), String>::new();
    let mut active_backend_ids = BTreeSet::<(String, String)>::new();

    for call in calls {
        let backend_id = clean_optional_string(call.backend_id.as_deref());
        let behavior_id = clean_optional_string(call.behavior_id.as_deref());
        let agent_did = clean_optional_string(call.agent_did.as_deref());

        if !agent_did.is_empty() && !backend_id.is_empty() {
            let backend_key = (agent_did.clone(), backend_id.clone());
            active_backend_ids.insert(backend_key.clone());
            let counts = backend_counts.entry(backend_key).or_default();
            apply_call_state(&call.call_state, counts);
        }
        if !agent_did.is_empty() && !behavior_id.is_empty() {
            let behavior_key = (agent_did, behavior_id);
            let counts = behavior_counts.entry(behavior_key.clone()).or_default();
            apply_call_state(&call.call_state, counts);
            active_behavior_backends
                .entry(behavior_key)
                .or_insert(backend_id);
        }
    }

    let mut expired = FleetExpiredCounts::default();
    for request in requests {
        if deadline_is_expired(generated_at, request.deadline.as_deref()) {
            expired.processing_requests += 1;
            let agent_did = clean_optional_string(request.agent_did.as_deref());
            let behavior_id = clean_optional_string(request.behavior_id.as_deref());
            if !agent_did.is_empty() && !behavior_id.is_empty() {
                behavior_counts
                    .entry((agent_did, behavior_id))
                    .or_default()
                    .expired_processing += 1;
            }
        }
    }

    let mut backend_ids = backends.keys().cloned().collect::<BTreeSet<_>>();
    backend_ids.extend(active_backend_ids);
    let mut backend_snapshots = Vec::new();
    for (agent_did, backend_id) in backend_ids {
        let backend_key = (agent_did.clone(), backend_id.clone());
        let configured = backends.get(&backend_key);
        let counts = backend_counts
            .get(&backend_key)
            .cloned()
            .unwrap_or_default();
        let max_concurrent = configured
            .map(BackendRow::max_concurrent)
            .unwrap_or_default();
        let accepting_admission = backend_accepting
            .get(&backend_key)
            .copied()
            .unwrap_or(false);
        backend_snapshots.push(FleetBackendAdmissionCounters {
            backend_id,
            agent_did,
            configured: configured.is_some(),
            enabled: configured.map(BackendRow::is_enabled).unwrap_or(false),
            probe_status: configured
                .map(BackendRow::display_probe_status)
                .unwrap_or_else(|| UNKNOWN_PROBE_STATUS.to_string()),
            accepting_admission,
            running: counts.assigned,
            queued: counts.queued,
            available: if accepting_admission {
                max_concurrent.saturating_sub(counts.assigned)
            } else {
                0
            },
            max_concurrent,
            max_queue_depth: configured
                .map(BackendRow::max_queue_depth)
                .unwrap_or_default(),
        });
    }

    let mut behavior_ids = behaviors.keys().cloned().collect::<BTreeSet<_>>();
    behavior_ids.extend(active_behavior_backends.keys().cloned());
    let mut behavior_snapshots = Vec::new();
    for (agent_did, behavior_id) in behavior_ids {
        let behavior_key = (agent_did.clone(), behavior_id.clone());
        let configured = behaviors.get(&behavior_key);
        let active_backend_id = active_behavior_backends.get(&behavior_key);
        let backend_id = configured
            .map(ResolvedBehaviorRow::normalized_backend_id)
            .or_else(|| active_backend_id.cloned())
            .unwrap_or_default();
        let counts = behavior_counts
            .get(&behavior_key)
            .cloned()
            .unwrap_or_default();
        let backend_key = (agent_did.clone(), backend_id.clone());
        let backend = backends.get(&backend_key);
        let max = backend.map(BackendRow::max_concurrent).unwrap_or_default();
        let backend_available = backend_accepting
            .get(&backend_key)
            .copied()
            .unwrap_or(false);
        let enabled = configured
            .map(|row| row.behavior.is_enabled())
            .unwrap_or(false);
        let backend_running = backend_counts
            .get(&backend_key)
            .map(|counts| counts.assigned)
            .unwrap_or_default();
        behavior_snapshots.push(FleetBehaviorSlotUsage {
            behavior_id: behavior_id.clone(),
            agent_did,
            backend_id,
            configured: configured.is_some(),
            enabled,
            backend_available,
            assigned: counts.assigned,
            available: if enabled && backend_available {
                max.saturating_sub(backend_running)
            } else {
                0
            },
            max,
            queued: counts.queued,
            expired_processing: counts.expired_processing,
        });
    }

    let totals = FleetSlotTotals {
        assigned: backend_snapshots
            .iter()
            .map(|backend| backend.running)
            .sum(),
        available: backend_snapshots
            .iter()
            .map(|backend| backend.available)
            .sum(),
        max: backend_snapshots
            .iter()
            .map(|backend| backend.max_concurrent)
            .sum(),
        queued: backend_snapshots.iter().map(|backend| backend.queued).sum(),
    };

    FleetSlotSnapshot {
        generated_at: generated_at.to_rfc3339(),
        source: SNAPSHOT_SOURCE.to_string(),
        totals,
        expired,
        behaviors: behavior_snapshots,
        backends: backend_snapshots,
    }
}

/// Per-backend "accepting admission" from the readiness rows of the
/// behaviors currently bound to it — never from `InferenceBackend`'s
/// `enabled`/`probe_status` (measured health stays unpersisted; see
/// `backend_health.rs`). A backend accepts once any bound behavior's own
/// runtime reports it `Ready`; with no such signal the projection is
/// not-accepting. This is last-known capacity information, not an admission
/// gate or a connectivity probe. The runtime's admission owner and transport
/// health remain authoritative for live execution and reachability.
fn backend_admission_from_readiness(
    behaviors: &BTreeMap<(String, String), ResolvedBehaviorRow>,
    readiness_by_agent: &BTreeMap<String, AgentBehaviorReadinessRow>,
    observed_at: DateTime<Utc>,
) -> BTreeMap<(String, String), bool> {
    let mut accepting = BTreeMap::<(String, String), bool>::new();
    for ((agent_did, behavior_id), behavior) in behaviors {
        let backend_id = behavior.normalized_backend_id();
        if backend_id.is_empty() {
            continue;
        }
        let readiness_row = readiness_by_agent.get(agent_did);
        let projected =
            project_behavior_readiness_summary(readiness_row, agent_did.as_str(), observed_at);
        let ready = matches!(
            &projected,
            ProjectedBehaviorReadinessSummary::Observed(summary)
                if summary.snapshot.behaviors.iter().any(|entry| {
                    &entry.behavior_id == behavior_id
                        && entry.state == BehaviorReadinessState::Ready
                })
        );
        let entry = accepting
            .entry((agent_did.clone(), backend_id))
            .or_insert(false);
        *entry = *entry || ready;
    }
    accepting
}

fn apply_call_state(call_state: &str, counts: &mut SlotCounts) {
    if call_state_holds_backend_slot(call_state) {
        counts.assigned += 1;
    } else if call_state == "queued" {
        counts.queued += 1;
    }
}

fn clean_optional_string(value: Option<&str>) -> String {
    value.map(clean_string).unwrap_or_default()
}

fn clean_string(value: &str) -> String {
    value.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents_protocol::row::{
        BehaviorReadinessEntry, BehaviorReadinessProcessState, BehaviorReadinessSnapshot,
        BEHAVIOR_READINESS_FORMAT_VERSION,
    };
    use serde_json::json;

    fn empty_request_row() -> AgentRequestRow {
        serde_json::from_value(json!({ "request_id": "" })).unwrap()
    }

    fn readiness_row(
        agent_did: &str,
        default_behavior_id: &str,
        ready_behavior_ids: &[&str],
        updated_at: &str,
    ) -> AgentBehaviorReadinessRow {
        AgentBehaviorReadinessRow {
            agent_did: agent_did.to_string(),
            snapshot_json: serde_json::to_string(&BehaviorReadinessSnapshot {
                format_version: BEHAVIOR_READINESS_FORMAT_VERSION,
                process_state: BehaviorReadinessProcessState::Ready,
                active_generation: 1,
                router_generation: 1,
                default_behavior_id: default_behavior_id.to_string(),
                behaviors: ready_behavior_ids
                    .iter()
                    .map(|behavior_id| BehaviorReadinessEntry {
                        behavior_id: behavior_id.to_string(),
                        state: BehaviorReadinessState::Ready,
                        reason: None,
                    })
                    .collect(),
            })
            .unwrap(),
            updated_at: updated_at.to_string(),
        }
    }

    fn find_backend<'a>(
        snapshot: &'a FleetSlotSnapshot,
        agent_did: &str,
        backend_id: &str,
    ) -> &'a FleetBackendAdmissionCounters {
        snapshot
            .backends
            .iter()
            .find(|backend| backend.agent_did == agent_did && backend.backend_id == backend_id)
            .unwrap()
    }

    fn find_behavior<'a>(
        snapshot: &'a FleetSlotSnapshot,
        agent_did: &str,
        behavior_id: &str,
    ) -> &'a FleetBehaviorSlotUsage {
        snapshot
            .behaviors
            .iter()
            .find(|behavior| behavior.agent_did == agent_did && behavior.behavior_id == behavior_id)
            .unwrap()
    }

    #[test]
    fn snapshot_reconstructs_slots_by_backend_and_behavior() {
        let now = DateTime::parse_from_rfc3339("2026-05-20T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let snapshot = build_fleet_slot_snapshot(
            now,
            FleetSlotQueryEnvelope {
                behaviors: vec![
                    BehaviorRow {
                        behavior_id: "behavior-a".to_string(),
                        agent_did: "did:test:test".to_string(),
                        inference_profile_id: Some("profile-a".to_string()),
                        enabled: Some(true),
                    },
                    BehaviorRow {
                        behavior_id: "behavior-b".to_string(),
                        agent_did: "did:test:test".to_string(),
                        inference_profile_id: Some("profile-b".to_string()),
                        enabled: Some(true),
                    },
                ],
                profiles: vec![
                    InferenceProfileRow {
                        profile_id: "profile-a".to_string(),
                        agent_did: "did:test:test".to_string(),
                        backend_id: "backend-a".to_string(),
                    },
                    InferenceProfileRow {
                        profile_id: "profile-b".to_string(),
                        agent_did: "did:test:test".to_string(),
                        backend_id: "backend-a".to_string(),
                    },
                ],
                backends: vec![BackendRow {
                    backend_id: "backend-a".to_string(),
                    agent_did: "did:test:test".to_string(),
                    enabled: Some(true),
                    max_concurrent: Some(2),
                    max_queue_depth: Some(4),
                    probe_status: Some("healthy".to_string()),
                }],
                calls: vec![
                    InferenceCallRow {
                        backend_id: Some("backend-a".to_string()),
                        behavior_id: Some("behavior-a".to_string()),
                        agent_did: Some("did:test:test".to_string()),
                        call_state: "running".to_string(),
                    },
                    InferenceCallRow {
                        backend_id: Some("backend-a".to_string()),
                        behavior_id: Some("behavior-b".to_string()),
                        agent_did: Some("did:test:test".to_string()),
                        call_state: "queued".to_string(),
                    },
                ],
                requests: vec![AgentRequestRow {
                    agent_did: Some("did:test:test".to_string()),
                    behavior_id: Some("behavior-a".to_string()),
                    deadline: Some("2026-05-20T11:59:00Z".to_string()),
                    ..empty_request_row()
                }],
                behavior_readiness: vec![readiness_row(
                    "did:test:test",
                    "behavior-a",
                    &["behavior-a", "behavior-b"],
                    "2026-05-20T11:59:50Z",
                )],
            },
        );

        assert_eq!(snapshot.source, SNAPSHOT_SOURCE);
        assert_eq!(
            snapshot.totals,
            FleetSlotTotals {
                assigned: 1,
                available: 1,
                max: 2,
                queued: 1,
            }
        );
        assert_eq!(snapshot.expired.processing_requests, 1);
        assert_eq!(snapshot.backends[0].running, 1);
        assert_eq!(snapshot.backends[0].queued, 1);
        assert_eq!(snapshot.backends[0].available, 1);

        let behavior_a = snapshot
            .behaviors
            .iter()
            .find(|behavior| behavior.behavior_id == "behavior-a")
            .unwrap();
        assert_eq!(behavior_a.assigned, 1);
        assert_eq!(behavior_a.available, 1);
        assert_eq!(behavior_a.max, 2);
        assert_eq!(behavior_a.expired_processing, 1);

        let behavior_b = snapshot
            .behaviors
            .iter()
            .find(|behavior| behavior.behavior_id == "behavior-b")
            .unwrap();
        assert_eq!(behavior_b.assigned, 0);
        assert_eq!(behavior_b.queued, 1);
        assert_eq!(behavior_b.available, 1);
    }

    #[test]
    fn snapshot_preserves_unavailable_and_unconfigured_edges() {
        let now = DateTime::parse_from_rfc3339("2026-05-20T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let snapshot = build_fleet_slot_snapshot(
            now,
            FleetSlotQueryEnvelope {
                behaviors: vec![BehaviorRow {
                    behavior_id: "behavior-disabled".to_string(),
                    agent_did: "did:test:test".to_string(),
                    inference_profile_id: Some("profile-unhealthy".to_string()),
                    enabled: Some(false),
                }],
                profiles: vec![InferenceProfileRow {
                    profile_id: "profile-unhealthy".to_string(),
                    agent_did: "did:test:test".to_string(),
                    backend_id: "backend-unhealthy".to_string(),
                }],
                backends: vec![
                    BackendRow {
                        backend_id: "backend-unhealthy".to_string(),
                        agent_did: "did:test:test".to_string(),
                        enabled: Some(true),
                        max_concurrent: Some(3),
                        max_queue_depth: Some(4),
                        probe_status: Some("unhealthy".to_string()),
                    },
                    BackendRow {
                        backend_id: "backend-missing-flags".to_string(),
                        agent_did: "did:test:test".to_string(),
                        enabled: None,
                        max_concurrent: Some(2),
                        max_queue_depth: Some(1),
                        probe_status: None,
                    },
                ],
                calls: vec![
                    InferenceCallRow {
                        backend_id: Some("backend-unhealthy".to_string()),
                        behavior_id: Some("behavior-disabled".to_string()),
                        agent_did: Some("did:test:test".to_string()),
                        call_state: "running".to_string(),
                    },
                    InferenceCallRow {
                        backend_id: Some("backend-stale".to_string()),
                        behavior_id: Some("behavior-stale".to_string()),
                        agent_did: Some("did:test:stale".to_string()),
                        call_state: "running".to_string(),
                    },
                ],
                requests: vec![
                    AgentRequestRow {
                        agent_did: Some("did:test:test".to_string()),
                        behavior_id: Some("behavior-disabled".to_string()),
                        deadline: Some("2026-05-20T11:59:00Z".to_string()),
                        ..empty_request_row()
                    },
                    AgentRequestRow {
                        agent_did: Some("did:test:test".to_string()),
                        behavior_id: Some("behavior-disabled".to_string()),
                        deadline: Some("not-a-date".to_string()),
                        ..empty_request_row()
                    },
                    AgentRequestRow {
                        agent_did: Some("did:test:test".to_string()),
                        behavior_id: Some("behavior-disabled".to_string()),
                        deadline: None,
                        ..empty_request_row()
                    },
                ],
                behavior_readiness: Vec::new(),
            },
        );

        assert_eq!(
            snapshot.totals,
            FleetSlotTotals {
                assigned: 2,
                available: 0,
                max: 5,
                queued: 0,
            }
        );
        assert_eq!(snapshot.expired.processing_requests, 1);

        let unhealthy = find_backend(&snapshot, "did:test:test", "backend-unhealthy");
        assert!(unhealthy.configured);
        assert!(unhealthy.enabled);
        assert_eq!(unhealthy.probe_status, "unhealthy");
        assert!(!unhealthy.accepting_admission);
        assert_eq!(unhealthy.running, 1);
        assert_eq!(unhealthy.available, 0);
        assert_eq!(unhealthy.max_concurrent, 3);

        let missing_flags = find_backend(&snapshot, "did:test:test", "backend-missing-flags");
        assert!(missing_flags.configured);
        assert!(!missing_flags.enabled);
        assert_eq!(missing_flags.probe_status, UNKNOWN_PROBE_STATUS);
        assert!(!missing_flags.accepting_admission);
        assert_eq!(missing_flags.available, 0);

        let stale_backend = find_backend(&snapshot, "did:test:stale", "backend-stale");
        assert!(!stale_backend.configured);
        assert!(!stale_backend.enabled);
        assert_eq!(stale_backend.probe_status, UNKNOWN_PROBE_STATUS);
        assert_eq!(stale_backend.running, 1);
        assert_eq!(stale_backend.max_concurrent, 0);

        let disabled = find_behavior(&snapshot, "did:test:test", "behavior-disabled");
        assert!(disabled.configured);
        assert!(!disabled.enabled);
        assert!(!disabled.backend_available);
        assert_eq!(disabled.assigned, 1);
        assert_eq!(disabled.available, 0);
        assert_eq!(disabled.max, 3);
        assert_eq!(disabled.expired_processing, 1);

        let stale_behavior = find_behavior(&snapshot, "did:test:stale", "behavior-stale");
        assert!(!stale_behavior.configured);
        assert!(!stale_behavior.enabled);
        assert_eq!(stale_behavior.agent_did, "did:test:stale");
        assert_eq!(stale_behavior.backend_id, "backend-stale");
        assert_eq!(stale_behavior.assigned, 1);
        assert_eq!(stale_behavior.available, 0);
        assert_eq!(stale_behavior.max, 0);
    }

    #[test]
    fn snapshot_scopes_same_ids_to_their_owning_agent() {
        let now = DateTime::parse_from_rfc3339("2026-05-20T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let behavior = |agent_did: &str| BehaviorRow {
            behavior_id: "shared-behavior".to_string(),
            agent_did: agent_did.to_string(),
            inference_profile_id: Some("shared-profile".to_string()),
            enabled: Some(true),
        };
        let profile = |agent_did: &str| InferenceProfileRow {
            profile_id: "shared-profile".to_string(),
            agent_did: agent_did.to_string(),
            backend_id: "shared-backend".to_string(),
        };
        let backend = |agent_did: &str, max_concurrent| BackendRow {
            backend_id: "shared-backend".to_string(),
            agent_did: agent_did.to_string(),
            enabled: Some(true),
            max_concurrent: Some(max_concurrent),
            max_queue_depth: Some(4),
            probe_status: Some("healthy".to_string()),
        };
        let call = |agent_did: &str, call_state: &str| InferenceCallRow {
            backend_id: Some("shared-backend".to_string()),
            behavior_id: Some("shared-behavior".to_string()),
            agent_did: Some(agent_did.to_string()),
            call_state: call_state.to_string(),
        };

        let snapshot = build_fleet_slot_snapshot(
            now,
            FleetSlotQueryEnvelope {
                behaviors: vec![behavior("did:test:a"), behavior("did:test:b")],
                profiles: vec![profile("did:test:a"), profile("did:test:b")],
                backends: vec![backend("did:test:a", 2), backend("did:test:b", 4)],
                calls: vec![call("did:test:a", "running"), call("did:test:b", "queued")],
                requests: vec![
                    AgentRequestRow {
                        agent_did: Some("did:test:a".to_string()),
                        behavior_id: Some("shared-behavior".to_string()),
                        deadline: Some("2026-05-20T11:59:00Z".to_string()),
                        ..empty_request_row()
                    },
                    AgentRequestRow {
                        agent_did: Some("did:test:b".to_string()),
                        behavior_id: Some("shared-behavior".to_string()),
                        deadline: Some("2026-05-20T12:01:00Z".to_string()),
                        ..empty_request_row()
                    },
                ],
                behavior_readiness: vec![
                    readiness_row(
                        "did:test:a",
                        "shared-behavior",
                        &["shared-behavior"],
                        "2026-05-20T11:59:50Z",
                    ),
                    readiness_row(
                        "did:test:b",
                        "shared-behavior",
                        &["shared-behavior"],
                        "2026-05-20T11:59:50Z",
                    ),
                ],
            },
        );

        let backend_a = find_backend(&snapshot, "did:test:a", "shared-backend");
        assert_eq!(backend_a.running, 1);
        assert_eq!(backend_a.queued, 0);
        assert_eq!(backend_a.available, 1);
        assert_eq!(backend_a.max_concurrent, 2);
        let backend_b = find_backend(&snapshot, "did:test:b", "shared-backend");
        assert_eq!(backend_b.running, 0);
        assert_eq!(backend_b.queued, 1);
        assert_eq!(backend_b.available, 4);
        assert_eq!(backend_b.max_concurrent, 4);

        let behavior_a = find_behavior(&snapshot, "did:test:a", "shared-behavior");
        assert_eq!(behavior_a.assigned, 1);
        assert_eq!(behavior_a.queued, 0);
        assert_eq!(behavior_a.expired_processing, 1);
        assert_eq!(behavior_a.available, 1);
        let behavior_b = find_behavior(&snapshot, "did:test:b", "shared-behavior");
        assert_eq!(behavior_b.assigned, 0);
        assert_eq!(behavior_b.queued, 1);
        assert_eq!(behavior_b.expired_processing, 0);
        assert_eq!(behavior_b.available, 4);

        assert_eq!(snapshot.expired.processing_requests, 1);
        assert_eq!(snapshot.totals.assigned, 1);
        assert_eq!(snapshot.totals.queued, 1);
        assert_eq!(snapshot.totals.available, 5);
        assert_eq!(snapshot.totals.max, 6);
    }

    #[test]
    fn decode_rejects_missing_data_object() {
        let error = decode_fleet_slot_query_response(json!({ "data": null })).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("fleet slot snapshot query response missing object data"),
            "{error:#}"
        );
    }
}
