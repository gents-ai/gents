use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanTriggerDispatchCase {
    pub name: String,
    pub trigger_id: Option<String>,
    pub trigger_kind: String,
    pub concurrency: String,
    pub active_schedule_ids: Vec<String>,
    pub active_event_trigger_ids: Vec<String>,
    pub intent_task_id: String,
    pub selected_task_id: Option<String>,
    pub prior_nonterminal_keys: Vec<String>,
    pub expected_result: String,
    pub expected_skip_reason: Option<String>,
    pub expected_materialize_trigger_id: Option<String>,
    pub expected_materialize_trigger_kind: Option<String>,
    pub expected_request_caused_by_id: Option<String>,
    pub expected_request_caused_by_kind: Option<String>,
    pub expected_execution_origin: Option<String>,
    pub expected_supersede_call_keys: Vec<String>,
    pub superseded_prior_ids: Vec<String>,
    pub target_nonterminal_count_after: Option<usize>,
    pub request_count_before: usize,
    pub request_count_after: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanEventGroupKeyContract {
    pub node_did: String,
    pub consumer: serde_json::Value,
    pub consumer_config_key: String,
    pub correlation: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanEventGroupCase {
    pub name: String,
    pub candidate: LeanEventGroupKeyContract,
    pub actual_count: usize,
    pub expected_count: Option<usize>,
    pub minimum_count: usize,
    pub timed_out: bool,
    pub well_formed: bool,
    pub prior_markers: Vec<LeanEventGroupKeyContract>,
    pub eligible: bool,
    pub materialized: bool,
    pub marker_count_after: usize,
}

#[derive(Debug, Deserialize)]
pub struct LeanRuntimeReconcileCase {
    pub requested_agent: Option<usize>,
    pub pre_default_agent: usize,
    pub pre_session_agent: Option<usize>,
    pub pre_runnable: Vec<usize>,
    pub name: String,
    pub action: String,
    pub legal: bool,
    pub pre_phase: String,
    pub post_phase: String,
    pub pre_active_generation: usize,
    pub post_active_generation: usize,
    pub pre_router_generation: usize,
    pub post_router_generation: usize,
    pub pre_ready_generation_count: usize,
    pub post_ready_generation_count: usize,
    pub pre_live_generation_count: usize,
    pub post_live_generation_count: usize,
    pub pre_in_flight_count: usize,
    pub post_in_flight_count: usize,
    pub tracked_request_id: usize,
    pub tracked_session_id: usize,
    pub tracked_request_generation: usize,
    pub tracked_request_session: usize,
    pub tracked_request_agent: usize,
    pub tracked_session_agent: usize,
}

#[derive(Debug, Deserialize)]
pub struct LeanClientAgentReadinessCase {
    pub name: String,
    pub observation_present: bool,
    pub observation_kind: String,
    pub process_state: String,
    pub active_generation: u64,
    pub router_generation: u64,
    pub runnable: bool,
    pub unavailable: bool,
    pub startup_demoted: bool,
    pub runtime_unavailable_reason: String,
    pub expected_state: String,
    pub expected_reason: Option<String>,
    pub expected_runtime_admissible: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanApplyDocRef {
    pub node_did: String,
    pub collection: String,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanApplyDesiredRow {
    #[serde(rename = "ref")]
    pub target: LeanApplyDocRef,
    pub content: String,
    pub refs: Vec<LeanApplyDocRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanApplyLiveRow {
    #[serde(rename = "ref")]
    pub target: LeanApplyDocRef,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanApplyReconcileCase {
    pub name: String,
    pub accepted: bool,
    pub candidate: Vec<LeanApplyDesiredRow>,
    pub pre_desired: Vec<LeanApplyDesiredRow>,
    pub pre_live: Vec<LeanApplyLiveRow>,
    pub expected_after_live: Vec<LeanApplyLiveRow>,
    pub expected_after_desired: Vec<LeanApplyDesiredRow>,
    pub expected_retry_desired: Vec<LeanApplyDesiredRow>,
    pub observations_preserved: bool,
}

#[derive(Debug, Deserialize)]
pub struct LeanReadinessPublicationCase {
    pub states: Vec<u64>,
    pub publishes: Vec<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanStartupReadinessCase {
    pub witness: String,
    pub lean_theorems: Vec<String>,
    pub budget: u64,
    pub outcomes: Vec<String>,
    pub retired_after: bool,
    pub post_standing: String,
    pub blocks_ready: bool,
    pub requires_restart: bool,
}
