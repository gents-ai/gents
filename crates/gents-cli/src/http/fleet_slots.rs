use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use gents::AuthenticatedGraphql;
use gents::{HEALTHY_PROBE_STATUS, UNKNOWN_PROBE_STATUS};
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
    pub(crate) configured: bool,
    pub(crate) enabled: bool,
    pub(crate) probe_status: String,
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
    #[serde(rename = "InferenceBackend", default)]
    backends: Vec<BackendRow>,
    #[serde(rename = "InferenceCall", default)]
    calls: Vec<InferenceCallRow>,
    #[serde(rename = "AgentRequest", default)]
    requests: Vec<RequestRow>,
}

#[derive(Debug, Clone, Deserialize)]
struct BehaviorRow {
    #[serde(default)]
    behavior_id: String,
    #[serde(default)]
    agent_did: String,
    #[serde(default)]
    backend_id: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
}

impl BehaviorRow {
    fn normalized_behavior_id(&self) -> String {
        clean_string(&self.behavior_id)
    }

    fn normalized_backend_id(&self) -> String {
        clean_optional_string(self.backend_id.as_deref())
    }

    fn is_enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }
}

#[derive(Debug, Clone, Deserialize)]
struct BackendRow {
    #[serde(default)]
    backend_id: String,
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

    fn normalized_probe_status(&self) -> String {
        let probe_status = clean_optional_string(self.probe_status.as_deref());
        if probe_status.is_empty() {
            UNKNOWN_PROBE_STATUS.to_string()
        } else {
            probe_status
        }
    }

    fn accepting_admission(&self) -> bool {
        self.is_enabled() && self.normalized_probe_status() == HEALTHY_PROBE_STATUS
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

#[derive(Debug, Clone, Deserialize)]
struct RequestRow {
    #[serde(default)]
    behavior_id: Option<String>,
    #[serde(default)]
    deadline: Option<String>,
}

#[derive(Clone, Default)]
struct SlotCounts {
    assigned: i64,
    queued: i64,
    expired_processing: i64,
}

pub(crate) async fn load_fleet_slot_snapshot(
    graphql: &AuthenticatedGraphql,
) -> Result<FleetSlotSnapshot> {
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
            backend_id
            enabled
        }
        InferenceBackend(order: { backend_id: ASC }) {
            backend_id
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
            status: { _eq: "processing" },
            lifecycle_state: { _eq: "processing" }
        }) {
            behavior_id
            deadline
        }
    }"#
}

fn build_fleet_slot_snapshot(
    generated_at: DateTime<Utc>,
    envelope: FleetSlotQueryEnvelope,
) -> FleetSlotSnapshot {
    let backends = envelope
        .backends
        .into_iter()
        .filter_map(|backend| {
            let backend_id = backend.normalized_backend_id();
            (!backend_id.is_empty()).then_some((backend_id, backend))
        })
        .collect::<BTreeMap<_, _>>();

    let behaviors = envelope
        .behaviors
        .into_iter()
        .filter_map(|behavior| {
            let behavior_id = behavior.normalized_behavior_id();
            (!behavior_id.is_empty()).then_some((behavior_id, behavior))
        })
        .collect::<BTreeMap<_, _>>();

    let mut backend_counts = BTreeMap::<String, SlotCounts>::new();
    let mut behavior_counts = BTreeMap::<String, SlotCounts>::new();
    let mut active_behavior_metadata = BTreeMap::<String, (String, String)>::new();
    let mut active_backend_ids = BTreeSet::<String>::new();

    for call in envelope.calls {
        let backend_id = clean_optional_string(call.backend_id.as_deref());
        let behavior_id = clean_optional_string(call.behavior_id.as_deref());
        let agent_did = clean_optional_string(call.agent_did.as_deref());

        if !backend_id.is_empty() {
            active_backend_ids.insert(backend_id.clone());
            let counts = backend_counts.entry(backend_id.clone()).or_default();
            apply_call_state(&call.call_state, counts);
        }
        if !behavior_id.is_empty() {
            let counts = behavior_counts.entry(behavior_id.clone()).or_default();
            apply_call_state(&call.call_state, counts);
            active_behavior_metadata
                .entry(behavior_id)
                .or_insert((agent_did, backend_id));
        }
    }

    let mut expired = FleetExpiredCounts::default();
    for request in envelope.requests {
        if deadline_is_expired(generated_at, request.deadline.as_deref()) {
            expired.processing_requests += 1;
            let behavior_id = clean_optional_string(request.behavior_id.as_deref());
            if !behavior_id.is_empty() {
                behavior_counts
                    .entry(behavior_id)
                    .or_default()
                    .expired_processing += 1;
            }
        }
    }

    let mut backend_ids = backends.keys().cloned().collect::<BTreeSet<_>>();
    backend_ids.extend(active_backend_ids);
    let mut backend_snapshots = Vec::new();
    for backend_id in backend_ids {
        let configured = backends.get(&backend_id);
        let counts = backend_counts.get(&backend_id).cloned().unwrap_or_default();
        let max_concurrent = configured
            .map(BackendRow::max_concurrent)
            .unwrap_or_default();
        let accepting_admission = configured
            .map(BackendRow::accepting_admission)
            .unwrap_or(false);
        backend_snapshots.push(FleetBackendAdmissionCounters {
            backend_id,
            configured: configured.is_some(),
            enabled: configured.map(BackendRow::is_enabled).unwrap_or(false),
            probe_status: configured
                .map(BackendRow::normalized_probe_status)
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
    behavior_ids.extend(active_behavior_metadata.keys().cloned());
    let mut behavior_snapshots = Vec::new();
    for behavior_id in behavior_ids {
        let configured = behaviors.get(&behavior_id);
        let active_metadata = active_behavior_metadata.get(&behavior_id);
        let backend_id = configured
            .map(BehaviorRow::normalized_backend_id)
            .or_else(|| active_metadata.map(|(_, backend_id)| backend_id.clone()))
            .unwrap_or_default();
        let counts = behavior_counts
            .get(&behavior_id)
            .cloned()
            .unwrap_or_default();
        let backend = backends.get(&backend_id);
        let max = backend.map(BackendRow::max_concurrent).unwrap_or_default();
        let backend_available = backend
            .map(BackendRow::accepting_admission)
            .unwrap_or(false);
        let enabled = configured.map(BehaviorRow::is_enabled).unwrap_or(false);
        let backend_running = backend_counts
            .get(&backend_id)
            .map(|counts| counts.assigned)
            .unwrap_or_default();
        behavior_snapshots.push(FleetBehaviorSlotUsage {
            behavior_id: behavior_id.clone(),
            agent_did: configured
                .map(|behavior| clean_string(&behavior.agent_did))
                .or_else(|| active_metadata.map(|(agent_did, _)| agent_did.clone()))
                .unwrap_or_default(),
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

fn apply_call_state(call_state: &str, counts: &mut SlotCounts) {
    match call_state {
        "running" => counts.assigned += 1,
        "queued" => counts.queued += 1,
        _ => {}
    }
}

fn deadline_is_expired(now: DateTime<Utc>, deadline: Option<&str>) -> bool {
    let Some(deadline) = deadline.map(str::trim).filter(|value| !value.is_empty()) else {
        return false;
    };
    DateTime::parse_from_rfc3339(deadline)
        .map(|deadline| deadline.with_timezone(&Utc) < now)
        .unwrap_or(false)
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
    use serde_json::json;

    fn find_backend<'a>(
        snapshot: &'a FleetSlotSnapshot,
        backend_id: &str,
    ) -> &'a FleetBackendAdmissionCounters {
        snapshot
            .backends
            .iter()
            .find(|backend| backend.backend_id == backend_id)
            .unwrap()
    }

    fn find_behavior<'a>(
        snapshot: &'a FleetSlotSnapshot,
        behavior_id: &str,
    ) -> &'a FleetBehaviorSlotUsage {
        snapshot
            .behaviors
            .iter()
            .find(|behavior| behavior.behavior_id == behavior_id)
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
                        backend_id: Some("backend-a".to_string()),
                        enabled: Some(true),
                    },
                    BehaviorRow {
                        behavior_id: "behavior-b".to_string(),
                        agent_did: "did:test:test".to_string(),
                        backend_id: Some("backend-a".to_string()),
                        enabled: Some(true),
                    },
                ],
                backends: vec![BackendRow {
                    backend_id: "backend-a".to_string(),
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
                requests: vec![RequestRow {
                    behavior_id: Some("behavior-a".to_string()),
                    deadline: Some("2026-05-20T11:59:00Z".to_string()),
                }],
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
                    backend_id: Some("backend-unhealthy".to_string()),
                    enabled: Some(false),
                }],
                backends: vec![
                    BackendRow {
                        backend_id: "backend-unhealthy".to_string(),
                        enabled: Some(true),
                        max_concurrent: Some(3),
                        max_queue_depth: Some(4),
                        probe_status: Some("unhealthy".to_string()),
                    },
                    BackendRow {
                        backend_id: "backend-missing-flags".to_string(),
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
                    RequestRow {
                        behavior_id: Some("behavior-disabled".to_string()),
                        deadline: Some("2026-05-20T11:59:00Z".to_string()),
                    },
                    RequestRow {
                        behavior_id: Some("behavior-disabled".to_string()),
                        deadline: Some("not-a-date".to_string()),
                    },
                    RequestRow {
                        behavior_id: Some("behavior-disabled".to_string()),
                        deadline: None,
                    },
                ],
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

        let unhealthy = find_backend(&snapshot, "backend-unhealthy");
        assert!(unhealthy.configured);
        assert!(unhealthy.enabled);
        assert_eq!(unhealthy.probe_status, "unhealthy");
        assert!(!unhealthy.accepting_admission);
        assert_eq!(unhealthy.running, 1);
        assert_eq!(unhealthy.available, 0);
        assert_eq!(unhealthy.max_concurrent, 3);

        let missing_flags = find_backend(&snapshot, "backend-missing-flags");
        assert!(missing_flags.configured);
        assert!(!missing_flags.enabled);
        assert_eq!(missing_flags.probe_status, UNKNOWN_PROBE_STATUS);
        assert!(!missing_flags.accepting_admission);
        assert_eq!(missing_flags.available, 0);

        let stale_backend = find_backend(&snapshot, "backend-stale");
        assert!(!stale_backend.configured);
        assert!(!stale_backend.enabled);
        assert_eq!(stale_backend.probe_status, UNKNOWN_PROBE_STATUS);
        assert_eq!(stale_backend.running, 1);
        assert_eq!(stale_backend.max_concurrent, 0);

        let disabled = find_behavior(&snapshot, "behavior-disabled");
        assert!(disabled.configured);
        assert!(!disabled.enabled);
        assert!(!disabled.backend_available);
        assert_eq!(disabled.assigned, 1);
        assert_eq!(disabled.available, 0);
        assert_eq!(disabled.max, 3);
        assert_eq!(disabled.expired_processing, 1);

        let stale_behavior = find_behavior(&snapshot, "behavior-stale");
        assert!(!stale_behavior.configured);
        assert!(!stale_behavior.enabled);
        assert_eq!(stale_behavior.agent_did, "did:test:stale");
        assert_eq!(stale_behavior.backend_id, "backend-stale");
        assert_eq!(stale_behavior.assigned, 1);
        assert_eq!(stale_behavior.available, 0);
        assert_eq!(stale_behavior.max, 0);
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
