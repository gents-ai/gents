use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanTriggerKeyContract {
    pub(crate) trigger_id: String,
    pub(crate) trigger_kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanTriggerDispatchCase {
    pub(crate) name: String,
    pub(crate) trigger_id: Option<String>,
    pub(crate) trigger_kind: String,
    pub(crate) concurrency: String,
    pub(crate) active_schedule_ids: Vec<String>,
    pub(crate) active_event_trigger_ids: Vec<String>,
    pub(crate) prior_nonterminal_keys: Vec<LeanTriggerKeyContract>,
    pub(crate) expected_result: String,
    pub(crate) expected_skip_reason: Option<String>,
    pub(crate) expected_materialize_trigger_id: Option<String>,
    pub(crate) expected_materialize_trigger_kind: Option<String>,
    pub(crate) expected_request_caused_by_id: Option<String>,
    pub(crate) expected_request_caused_by_kind: Option<String>,
    pub(crate) expected_execution_origin: Option<String>,
    pub(crate) expected_supersede_call_keys: Vec<LeanTriggerKeyContract>,
    pub(crate) superseded_prior_ids: Vec<String>,
    pub(crate) target_nonterminal_count_after: Option<usize>,
    pub(crate) request_count_before: usize,
    pub(crate) request_count_after: usize,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LeanRuntimeReconcileCase {
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
    pub(crate) tracked_request_behavior: usize,
    pub(crate) tracked_session_behavior: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanApplyDocRef {
    pub(crate) collection: String,
    pub(crate) id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanApplyDesiredDoc {
    pub(crate) collection: String,
    pub(crate) id: String,
    pub(crate) content: String,
    pub(crate) refs: Vec<LeanApplyDocRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanApplyLiveDoc {
    pub(crate) collection: String,
    pub(crate) id: String,
    pub(crate) content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanApplyStep {
    pub(crate) action: String,
    pub(crate) target: LeanApplyDocRef,
    pub(crate) content: String,
    pub(crate) refs: Vec<LeanApplyDocRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanApplyCollectionWrite {
    pub(crate) collection: String,
    pub(crate) graphql_type: String,
    pub(crate) unique_field: String,
    pub(crate) apply_order: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanApplySelectedDoc {
    pub(crate) action: String,
    pub(crate) target: LeanApplyDocRef,
    pub(crate) graphql_type: String,
    pub(crate) unique_field: String,
    pub(crate) unique_value: String,
    pub(crate) content: String,
    pub(crate) refs: Vec<LeanApplyDocRef>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LeanApplyReconcileCase {
    pub(crate) name: String,
    pub(crate) manifest: Vec<LeanApplyDesiredDoc>,
    pub(crate) pre_desired: Vec<LeanApplyDesiredDoc>,
    pub(crate) pre_live: Vec<LeanApplyLiveDoc>,
    pub(crate) expected_create: Vec<LeanApplyDocRef>,
    pub(crate) expected_update: Vec<LeanApplyDocRef>,
    pub(crate) expected_unchanged: Vec<LeanApplyDocRef>,
    pub(crate) expected_live_only: Vec<LeanApplyDocRef>,
    pub(crate) expected_steps: Vec<LeanApplyStep>,
    pub(crate) expected_write_order: Vec<LeanApplyCollectionWrite>,
    pub(crate) expected_selected_create_docs: Vec<LeanApplySelectedDoc>,
    pub(crate) expected_selected_update_docs: Vec<LeanApplySelectedDoc>,
    pub(crate) expected_selected_writes: Vec<LeanApplySelectedDoc>,
    pub(crate) prefix_len: usize,
    pub(crate) expected_prefix_desired: Vec<LeanApplyDesiredDoc>,
    pub(crate) expected_after_desired: Vec<LeanApplyDesiredDoc>,
    pub(crate) expected_retry_desired: Vec<LeanApplyDesiredDoc>,
    pub(crate) expected_retry_step_count: usize,
    pub(crate) expected_rediff_step_count: usize,
    pub(crate) live_preserved: bool,
    pub(crate) manifest_realized_after: bool,
    pub(crate) retry_converges: bool,
    pub(crate) idempotent_after: bool,
    pub(crate) write_order_prefix_safe: bool,
    pub(crate) production_prefixes_referrers_closed: bool,
    pub(crate) prefix_referrers_closed: bool,
    pub(crate) desired_references_closed_after_prefix: bool,
}
