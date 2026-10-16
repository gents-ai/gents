//! Contract-section deserialization targets shared by the trigger, apply
//! publication, runtime-reconcile, client-agent-readiness, and
//! startup-readiness conformance consumers:
//!
//! - triggers: `Proofs/Conformance/Triggers/Contracts.lean`
//! - apply publication: `Proofs/ApplyReconcile/Publication.lean` +
//!   `Proofs/ApplyReconcile/ContractCases.lean`
//! - runtime reconcile / readiness: `Proofs/RuntimeReconcile/*.lean`

use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanTriggerDispatchCase {
    pub(crate) name: String,
    pub(crate) trigger_id: Option<String>,
    pub(crate) trigger_kind: String,
    pub(crate) concurrency: String,
    pub(crate) active_schedule_ids: Vec<String>,
    pub(crate) active_event_trigger_ids: Vec<String>,
    pub(crate) intent_task_id: String,
    pub(crate) selected_task_id: Option<String>,
    pub(crate) prior_nonterminal_keys: Vec<String>,
    pub(crate) expected_result: String,
    pub(crate) expected_skip_reason: Option<String>,
    pub(crate) expected_materialize_trigger_id: Option<String>,
    pub(crate) expected_materialize_trigger_kind: Option<String>,
    pub(crate) expected_request_caused_by_id: Option<String>,
    pub(crate) expected_request_caused_by_kind: Option<String>,
    pub(crate) expected_execution_origin: Option<String>,
    pub(crate) expected_supersede_call_keys: Vec<String>,
    pub(crate) superseded_prior_ids: Vec<String>,
    pub(crate) target_nonterminal_count_after: Option<usize>,
    pub(crate) request_count_before: usize,
    pub(crate) request_count_after: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanEventGroupKeyContract {
    pub(crate) node_did: String,
    pub(crate) consumer: serde_json::Value,
    pub(crate) consumer_config_key: String,
    pub(crate) correlation: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanEventGroupCase {
    pub(crate) name: String,
    pub(crate) candidate: LeanEventGroupKeyContract,
    pub(crate) actual_count: usize,
    pub(crate) expected_count: Option<usize>,
    pub(crate) minimum_count: usize,
    pub(crate) timed_out: bool,
    pub(crate) well_formed: bool,
    pub(crate) prior_markers: Vec<LeanEventGroupKeyContract>,
    pub(crate) eligible: bool,
    pub(crate) materialized: bool,
    pub(crate) marker_count_after: usize,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LeanRuntimeReconcileCase {
    pub(crate) requested_agent: Option<usize>,
    pub(crate) pre_default_agent: usize,
    pub(crate) pre_session_agent: Option<usize>,
    pub(crate) pre_runnable: Vec<usize>,
    pub(crate) name: String,
    pub(crate) action: String,
    pub(crate) legal: bool,
    pub(crate) pre_phase: String,
    pub(crate) post_phase: String,
    pub(crate) pre_active_generation: usize,
    pub(crate) post_active_generation: usize,
    pub(crate) pre_router_generation: usize,
    pub(crate) post_router_generation: usize,
    pub(crate) pre_ready_generation_count: usize,
    pub(crate) post_ready_generation_count: usize,
    pub(crate) pre_live_generation_count: usize,
    pub(crate) post_live_generation_count: usize,
    pub(crate) pre_in_flight_count: usize,
    pub(crate) post_in_flight_count: usize,
    pub(crate) tracked_request_id: usize,
    pub(crate) tracked_session_id: usize,
    pub(crate) tracked_request_generation: usize,
    pub(crate) tracked_request_session: usize,
    pub(crate) tracked_request_agent: usize,
    pub(crate) tracked_session_agent: usize,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LeanClientAgentReadinessCase {
    pub(crate) name: String,
    pub(crate) observation_present: bool,
    pub(crate) observation_kind: String,
    pub(crate) process_state: String,
    pub(crate) active_generation: u64,
    pub(crate) router_generation: u64,
    pub(crate) runnable: bool,
    pub(crate) unavailable: bool,
    pub(crate) startup_demoted: bool,
    pub(crate) runtime_unavailable_reason: String,
    pub(crate) expected_state: String,
    pub(crate) expected_reason: Option<String>,
    pub(crate) expected_runtime_admissible: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanApplyDocRef {
    pub(crate) node_did: String,
    pub(crate) collection: String,
    pub(crate) id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanApplyDesiredRow {
    #[serde(rename = "ref")]
    pub(crate) target: LeanApplyDocRef,
    pub(crate) content: String,
    pub(crate) refs: Vec<LeanApplyDocRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanApplyLiveRow {
    #[serde(rename = "ref")]
    pub(crate) target: LeanApplyDocRef,
    pub(crate) value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanApplyReconcileCase {
    pub(crate) name: String,
    pub(crate) accepted: bool,
    pub(crate) candidate: Vec<LeanApplyDesiredRow>,
    pub(crate) pre_desired: Vec<LeanApplyDesiredRow>,
    pub(crate) pre_live: Vec<LeanApplyLiveRow>,
    pub(crate) expected_after_live: Vec<LeanApplyLiveRow>,
    pub(crate) expected_after_desired: Vec<LeanApplyDesiredRow>,
    pub(crate) expected_retry_desired: Vec<LeanApplyDesiredRow>,
    pub(crate) observations_preserved: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LeanReadinessPublicationCase {
    pub(crate) states: Vec<u64>,
    pub(crate) publishes: Vec<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanStartupReadinessCase {
    pub(crate) witness: String,
    pub(crate) lean_theorems: Vec<String>,
    pub(crate) budget: u64,
    pub(crate) outcomes: Vec<String>,
    pub(crate) retired_after: bool,
    pub(crate) post_standing: String,
    pub(crate) blocks_ready: bool,
    pub(crate) requires_restart: bool,
}
