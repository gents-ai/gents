#![allow(dead_code)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use serde::Deserialize;

#[derive(Debug, Clone, Copy)]
pub(crate) struct LeanVocabulary<'a> {
    pub(crate) lean_file: &'a str,
    pub(crate) model: &'a str,
    pub(crate) namespace: &'a str,
    pub(crate) rust_source: &'a str,
    pub(crate) rust_values: &'a [&'a str],
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct LeanContractVocabulary<'a> {
    pub(crate) domain: &'a str,
    pub(crate) rust_source: &'a str,
    pub(crate) rust_values: &'a [&'a str],
}

#[derive(Debug, Deserialize)]
pub(crate) struct LeanContractSnapshot {
    pub(crate) generated_by: String,
    pub(crate) vocabularies: Vec<LeanVocabularyContract>,
    pub(crate) state_machines: Vec<LeanStateMachineContract>,
    pub(crate) request_transition_cases: Vec<LeanLifecycleTransitionCase>,
    pub(crate) process_transition_cases: Vec<LeanLifecycleTransitionCase>,
    pub(crate) trigger_dispatch_case_count: usize,
    pub(crate) trigger_dispatch_cases: Vec<LeanTriggerDispatchCase>,
    pub(crate) frontend_client_shell_case_count: usize,
    pub(crate) frontend_client_shell_cases: Vec<LeanClientShellCase>,
    pub(crate) desktop_client_shell_case_count: usize,
    pub(crate) desktop_client_shell_cases: Vec<LeanClientShellCase>,
    pub(crate) runtime_reconcile_cases: Vec<LeanRuntimeReconcileCase>,
    pub(crate) apply_reconcile_cases: Vec<LeanApplyReconcileCase>,
    pub(crate) session_recovery_cases: Vec<LeanSessionRecoveryCase>,
    pub(crate) inference_slot_accounting_cases: Vec<LeanInferenceSlotAccountingCase>,
    pub(crate) fleet_slot_accounting_cases: Vec<LeanFleetSlotAccountingCase>,
    pub(crate) persistence_failure_policy_cases: Vec<LeanPersistenceFailurePolicyCase>,
    pub(crate) storage_observation_runtime_cases: Vec<LeanStorageObservationRuntimeCase>,
    pub(crate) backend_health_admission_cases: Vec<LeanBackendHealthAdmissionCase>,
    pub(crate) native_filesystem_boundary_cases: Vec<LeanNativeFilesystemBoundaryCase>,
    pub(crate) tool_preflight_cases: Vec<LeanToolPreflightCase>,
    pub(crate) tool_retry_cases: Vec<LeanToolRetryCase>,
    pub(crate) mcp_health_cases: Vec<LeanMcpHealthCase>,
    pub(crate) boundaries: Vec<LeanBoundary>,
    pub(crate) deviations: Vec<LeanDeviation>,
    pub(crate) command_policy_cases: Vec<LeanCommandPolicyCase>,
    pub(crate) command_sandbox_cases: Vec<LeanCommandSandboxCase>,
    pub(crate) command_env_cases: Vec<LeanCommandEnvCase>,
    pub(crate) live_overlay_cases: Vec<LeanLiveOverlayCase>,
    pub(crate) queue_deadline_conformance_cases: Vec<LeanQueueDeadlineConformanceCase>,
    pub(crate) recovery_sweep_cases: Vec<LeanRecoverySweepCase>,
    pub(crate) transcript_conformance_cases: Vec<LeanTranscriptCase>,
    pub(crate) streaming_response_cases: Vec<LeanResponseTransitionCase>,
    pub(crate) compaction_reducer_cases: Vec<LeanCompactionReducerCase>,
    pub(crate) follow_up_hooks: Vec<String>,
    pub(crate) coverage_ledger: Vec<LeanCoverageEntry>,
    pub(crate) identity_structural_cases: Vec<LeanIdentityStructuralCase>,
    pub(crate) identity_contracts: Vec<LeanIdentityContract>,
    #[serde(default)]
    pub(crate) event_delivery_transition_case_count: usize,
    #[serde(default)]
    pub(crate) event_delivery_transition_cases: Vec<LeanEventDeliveryTransitionCase>,
    #[serde(default)]
    pub(crate) event_delivery_source_instances: Vec<LeanEventDeliverySourceInstance>,
    #[serde(default)]
    pub(crate) event_delivery_convergence_traces: Vec<LeanEventDeliveryConvergenceTrace>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LeanVocabularyContract {
    pub(crate) domain: String,
    pub(crate) values: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LeanStateMachineContract {
    pub(crate) domain: String,
    pub(crate) states: Vec<String>,
    pub(crate) state_count: usize,
    pub(crate) terminal_states: Vec<String>,
    pub(crate) nonterminal_states: Vec<String>,
    pub(crate) actions: Vec<String>,
    pub(crate) legal_transitions: Vec<LeanTransitionPair>,
    pub(crate) illegal_transitions: Vec<LeanTransitionPair>,
    /// Named transition rows emitted by `Conformance.Contracts.NamedTransition`.
    /// Defaults to empty so existing call sites (Bucket 0/Bucket 1) don't need
    /// to deserialize this field on machines that don't emit any.
    #[serde(default)]
    pub(crate) named_transitions: Vec<LeanNamedTransition>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanNamedTransition {
    pub(crate) name: String,
    pub(crate) from: String,
    pub(crate) to: String,
    #[serde(default)]
    pub(crate) requires_native: bool,
    #[serde(default)]
    pub(crate) requires_child: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LeanCoverageEntry {
    pub(crate) category: String,
    pub(crate) domain: String,
    pub(crate) consumer: String,
    pub(crate) accepted_boundary: String,
    pub(crate) accepted_follow_up: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LeanBoundary {
    pub(crate) id: String,
    pub(crate) domain: String,
    pub(crate) subject: String,
    pub(crate) statement: String,
    pub(crate) accepted_failure_mode: Option<String>,
    pub(crate) accepted_follow_up: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LeanDeviation {
    pub(crate) id: String,
    pub(crate) domain: String,
    pub(crate) subject: String,
    pub(crate) statement: String,
    pub(crate) accepted_failure_mode: Option<String>,
    pub(crate) accepted_follow_up: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanTransitionPair {
    pub(crate) from: String,
    pub(crate) to: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanLifecycleTransitionCase {
    pub(crate) name: String,
    pub(crate) domain: String,
    pub(crate) from: String,
    pub(crate) to: String,
    pub(crate) classification: String,
    pub(crate) action: Option<String>,
    pub(crate) boundary: Option<String>,
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
pub(crate) struct LeanEventDeliveryWorld {
    pub(crate) persistent_set: Vec<String>,
    pub(crate) subscription_queue: Vec<String>,
    pub(crate) processed_set: Vec<String>,
    pub(crate) handled: Vec<String>,
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum LeanEventDeliveryAction {
    Persist { doc: String },
    Depersist { doc: String },
    Enqueue { doc: String },
    Drop { doc: String },
    DeliverFromQueue { doc: String },
    RescanTick,
    Handle { doc: String },
}

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct LeanEventDeliveryTransitionCase {
    pub(crate) name: String,
    pub(crate) pre: LeanEventDeliveryWorld,
    pub(crate) action: LeanEventDeliveryAction,
    pub(crate) post: LeanEventDeliveryWorld,
}

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct LeanEventDeliverySourceInstance {
    pub(crate) name: String,
    pub(crate) dedupe_policy: String,
    pub(crate) rescan_bounded_by: u64,
    pub(crate) deviation: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct LeanEventDeliveryConvergenceTrace {
    pub(crate) name: String,
    pub(crate) instance_name: String,
    pub(crate) initial_world: LeanEventDeliveryWorld,
    pub(crate) actions: Vec<LeanEventDeliveryAction>,
    pub(crate) final_world: LeanEventDeliveryWorld,
    pub(crate) status: String,
}

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
    pub(crate) prefix_referrers_closed: bool,
    pub(crate) desired_references_closed_after_prefix: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LeanClientShellCase {
    pub(crate) name: String,
    pub(crate) property: String,
    pub(crate) input: String,
    pub(crate) pre_selection_session: Option<usize>,
    pub(crate) post_selection_session: Option<usize>,
    pub(crate) pre_workflow_kind: String,
    pub(crate) pre_workflow_request: Option<usize>,
    pub(crate) post_workflow_kind: String,
    pub(crate) post_workflow_request: Option<usize>,
    pub(crate) selection_preserved: bool,
    pub(crate) workflow_advanced: bool,
    pub(crate) transport_noop: bool,
    pub(crate) can_submit_before: bool,
    pub(crate) can_submit_after: bool,
    pub(crate) send_decision: String,
    pub(crate) send_blocked_reason: Option<String>,
    pub(crate) frontend_expected_workflow_kind: String,
    pub(crate) frontend_expected_send_status: String,
    pub(crate) frontend_expected_send_blocked_reason: Option<String>,
    pub(crate) frontend_expected_active_request_id: Option<usize>,
    pub(crate) frontend_conversation_present: bool,
    pub(crate) desktop_selected_session_id: Option<usize>,
    pub(crate) desktop_snapshot_present: bool,
    pub(crate) desktop_preferred_request_id: Option<usize>,
    pub(crate) desktop_observed_request_id: Option<usize>,
    pub(crate) desktop_observed_turn_state: Option<String>,
    pub(crate) desktop_expected_latest_request_id: Option<usize>,
    pub(crate) desktop_expected_turn_state: Option<String>,
    pub(crate) desktop_expect_pending_turn: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LeanSessionRecoveryCase {
    pub(crate) name: String,
    pub(crate) action: String,
    pub(crate) legal: bool,
    pub(crate) pre_latest_state: String,
    pub(crate) pre_failed_state: String,
    pub(crate) post_latest_state: String,
    pub(crate) post_failed_state: String,
    pub(crate) post_new_state: String,
    pub(crate) pre_latest_admission: String,
    pub(crate) post_latest_admission: String,
    pub(crate) pre_failed_admission: String,
    pub(crate) post_failed_admission: String,
    pub(crate) post_new_admission: String,
    pub(crate) pre_origin: String,
    pub(crate) post_new_origin: String,
    pub(crate) pre_backend: String,
    pub(crate) post_new_backend: String,
    pub(crate) failed_id: usize,
    pub(crate) new_id: usize,
    pub(crate) pre_latest_id: usize,
    pub(crate) post_latest_id: usize,
    pub(crate) pre_session_id: usize,
    pub(crate) post_session_id: usize,
    pub(crate) pre_behavior_id: usize,
    pub(crate) post_behavior_id: usize,
    pub(crate) pre_request_count: usize,
    pub(crate) post_request_count: usize,
    pub(crate) pre_retry_count: usize,
    pub(crate) post_retry_count: usize,
    pub(crate) max_retries: usize,
    pub(crate) pre_deadline_exceeded: bool,
    pub(crate) post_deadline_exceeded: bool,
    pub(crate) pre_failed_is_latest: bool,
    pub(crate) post_failed_is_latest: bool,
    pub(crate) post_new_is_latest: bool,
    pub(crate) pre_request_ids: Vec<usize>,
    pub(crate) pre_failed_exists: bool,
    pub(crate) pre_latest_exists: bool,
    pub(crate) pre_new_request_exists: bool,
    pub(crate) old_request_retained: bool,
    pub(crate) new_request_inserted: bool,
    pub(crate) origin_preserved: bool,
    pub(crate) backend_preserved: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LeanInferenceSlotAccountingCase {
    pub(crate) name: String,
    pub(crate) property: String,
    pub(crate) backend_id: String,
    pub(crate) pre_state: String,
    pub(crate) post_state: String,
    pub(crate) contribution: usize,
    pub(crate) expected_contribution: usize,
    pub(crate) pre_contribution: usize,
    pub(crate) post_contribution: usize,
    pub(crate) released_slot: bool,
    pub(crate) permit_drop_terminalization: bool,
    pub(crate) row_states: Vec<String>,
    pub(crate) row_backend_ids: Vec<String>,
    pub(crate) reconstructed_running_count: usize,
    pub(crate) max_concurrent: usize,
    pub(crate) bounded_by_max_concurrent: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LeanFleetSlotAccountingCase {
    pub(crate) name: String,
    pub(crate) property: String,
    pub(crate) backend_id: String,
    pub(crate) request_state: String,
    pub(crate) admission_state: String,
    pub(crate) contribution: usize,
    pub(crate) expected_contribution: usize,
    pub(crate) active_count: usize,
    pub(crate) scheduler_running: usize,
    pub(crate) slot_count: usize,
    pub(crate) row_states: Vec<String>,
    pub(crate) row_backend_ids: Vec<String>,
    pub(crate) reconstructed_running_count: usize,
    pub(crate) max_concurrent: usize,
    pub(crate) bounded_by_max_concurrent: bool,
    pub(crate) aggregate_reconstructed_not_persisted: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LeanPersistenceFailurePolicyCase {
    pub(crate) name: String,
    pub(crate) policy: String,
    pub(crate) action: String,
    pub(crate) pre_persistence: String,
    pub(crate) post_persistence: String,
    pub(crate) post_storage_observation: String,
    pub(crate) hook_decision: String,
    pub(crate) records_failure: bool,
    pub(crate) records_success: bool,
    pub(crate) external_durability_claimed: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LeanStorageObservationRuntimeCase {
    pub(crate) name: String,
    pub(crate) policy: String,
    pub(crate) action: String,
    pub(crate) pre_observation: String,
    pub(crate) mutation_result: String,
    pub(crate) post_observation: String,
    pub(crate) post_persistence: String,
    pub(crate) hook_result: String,
    pub(crate) records_failure: bool,
    pub(crate) records_success: bool,
    pub(crate) terminal_write_observed: bool,
    pub(crate) external_visibility_claimed: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LeanBackendHealthAdmissionCase {
    pub(crate) name: String,
    pub(crate) enabled: bool,
    pub(crate) probe_status: String,
    pub(crate) expected_available: bool,
    pub(crate) admission_decision: String,
    pub(crate) observed_document_only: bool,
    pub(crate) external_endpoint_freshness_claimed: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LeanNativeFilesystemBoundaryCase {
    pub(crate) name: String,
    pub(crate) tool_name: String,
    pub(crate) work_class: String,
    pub(crate) boundary: String,
    pub(crate) inner_poll_blocks: bool,
    pub(crate) request_deadline_ms: usize,
    pub(crate) blocker_ms: usize,
    pub(crate) expected_terminal: String,
    pub(crate) expected_failure_class: Option<String>,
    pub(crate) queue_advances_before_blocker_returns: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LeanToolPreflightCase {
    pub(crate) name: String,
    pub(crate) health: String,
    pub(crate) schema_status: String,
    pub(crate) decision: String,
    pub(crate) failure_class: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LeanToolRetryCase {
    pub(crate) name: String,
    pub(crate) operation: String,
    pub(crate) idempotency: String,
    pub(crate) failure_class: String,
    pub(crate) disposition: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LeanMcpHealthCase {
    pub(crate) name: String,
    pub(crate) start_state: String,
    pub(crate) start_count: usize,
    pub(crate) event: String,
    pub(crate) threshold_k: usize,
    pub(crate) next_state: Option<String>,
    pub(crate) next_count: Option<usize>,
    pub(crate) rust_projection: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LeanCommandPolicyCase {
    pub(crate) name: String,
    pub(crate) category: String,
    pub(crate) mode: String,
    pub(crate) allowed_argv_prefixes: Vec<Vec<String>>,
    pub(crate) forbidden_argv_prefixes: Vec<Vec<String>>,
    pub(crate) network_mode: String,
    pub(crate) read_only_allowlist: Vec<String>,
    pub(crate) command: String,
    pub(crate) lookup_command: String,
    pub(crate) args: Vec<String>,
    pub(crate) decision: String,
    pub(crate) denial_reason: Option<String>,
    pub(crate) matched_prefix: Option<Vec<String>>,
    pub(crate) denied_argv: Option<Vec<String>>,
    pub(crate) denied_command: Option<String>,
    pub(crate) denied_argument: Option<String>,
    pub(crate) denied_subcommand: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LeanCommandSandboxCase {
    pub(crate) name: String,
    pub(crate) category: String,
    pub(crate) mode: String,
    pub(crate) workspace_write_sandbox_enforced: bool,
    pub(crate) decision: String,
    pub(crate) sandbox: Option<String>,
    pub(crate) denial_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LeanCommandEnvCase {
    pub(crate) name: String,
    pub(crate) env_key: String,
    pub(crate) input_present: bool,
    pub(crate) input_name: String,
    pub(crate) input_value: String,
    pub(crate) output_name: String,
    pub(crate) expected_value_kind: Option<String>,
    pub(crate) expected_output_value: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanLiveOverlayCase {
    pub(crate) name: String,
    #[serde(rename = "responseStatus")]
    pub(crate) response_status: String,
    pub(crate) materialized: bool,
    #[serde(rename = "precedingToolCalls")]
    pub(crate) preceding_tool_calls: u64,
    #[serde(rename = "turnTerminal")]
    pub(crate) turn_terminal: bool,
    #[serde(rename = "turnLabel")]
    pub(crate) turn_label: String,
    #[serde(rename = "hasContent")]
    pub(crate) has_content: bool,
    #[serde(rename = "hasReasoning")]
    pub(crate) has_reasoning: bool,
    #[serde(rename = "expectOverlay")]
    pub(crate) expect_overlay: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanIdentityPrincipal {
    pub(crate) did: String,
    pub(crate) enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanIdentityBehavior {
    pub(crate) id: String,
    pub(crate) principal: String,
    pub(crate) enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanIdentityDeployment {
    pub(crate) id: String,
    pub(crate) principal: String,
    pub(crate) host_id: String,
    pub(crate) enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanIdentityStructuralCase {
    pub(crate) name: String,
    pub(crate) principals: Vec<LeanIdentityPrincipal>,
    pub(crate) behaviors: Vec<LeanIdentityBehavior>,
    pub(crate) deployments: Vec<LeanIdentityDeployment>,
    pub(crate) well_formed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanIdentityContract {
    pub(crate) name: String,
    pub(crate) statement: String,
    pub(crate) enforced: bool,
    pub(crate) tracked_by: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanQueueDeadlineConformanceCase {
    pub(crate) name: String,
    pub(crate) group: String,
    pub(crate) action: String,
    pub(crate) session_id: usize,
    pub(crate) legal: bool,
    pub(crate) pre_active_request_id: Option<usize>,
    pub(crate) post_active_request_id: Option<usize>,
    pub(crate) pre_pending_request_ids: Vec<usize>,
    pub(crate) post_pending_request_ids: Vec<usize>,
    pub(crate) claimed_request_id: Option<usize>,
    pub(crate) blocked_by_active: bool,
    pub(crate) superseded_request_ids: Vec<usize>,
    pub(crate) queue_key: Option<String>,
    pub(crate) post_coalesced_pending_count: usize,
    pub(crate) automated_drained_request_ids: Vec<usize>,
    pub(crate) preserved_user_pending_request_ids: Vec<usize>,
    pub(crate) post_terminal_request_ids: Vec<usize>,
    pub(crate) pre_request_deadline: Option<usize>,
    pub(crate) synthesized_claim_deadline: Option<usize>,
    pub(crate) post_deadline: Option<usize>,
    pub(crate) explicit_deadline_preserved: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanRecoverySweepCase {
    pub(crate) name: String,
    pub(crate) sweep_id: String,
    pub(crate) collection: String,
    pub(crate) rust_function: String,
    pub(crate) cadence: String,
    pub(crate) implementation_status: String,
    pub(crate) pre_state: String,
    pub(crate) terminal_state: String,
    pub(crate) measure_before: usize,
    pub(crate) measure_after: usize,
    pub(crate) deadline_audit_ref: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LeanTranscriptCase {
    pub(crate) name: String,
    pub(crate) group: String,
    pub(crate) action: String,
    pub(crate) legal: bool,
    pub(crate) pre_message_count: usize,
    pub(crate) post_message_count: usize,
    pub(crate) pre_tool_call_count: usize,
    pub(crate) post_tool_call_count: usize,
    pub(crate) pre_in_flight_count: usize,
    pub(crate) post_in_flight_count: usize,
    pub(crate) assistant_sequence: usize,
    pub(crate) result_sequence: usize,
    pub(crate) logical_result_id: usize,
    pub(crate) payload_hash: usize,
    pub(crate) expected_pair_closed: bool,
    pub(crate) expected_ordered: bool,
    pub(crate) expected_duplicate_reused_sequence: bool,
    pub(crate) expected_strong_drain: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LeanResponseTransitionCase {
    pub(crate) name: String,
    pub(crate) group: String,
    pub(crate) action: String,
    pub(crate) legal: bool,
    pub(crate) pre_status: String,
    pub(crate) post_status: String,
    pub(crate) pre_live_tail: String,
    pub(crate) post_live_tail: String,
    pub(crate) pre_token_count: usize,
    pub(crate) post_token_count: usize,
    pub(crate) error_reason: Option<String>,
    pub(crate) pre_materialized_seq: Option<usize>,
    pub(crate) post_materialized_seq: Option<usize>,
    pub(crate) expected_request_state: Option<String>,
    pub(crate) expected_request_persistence: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LeanCompactionReducerCase {
    pub(crate) name: String,
    pub(crate) group: String,
    pub(crate) reducer: String,
    pub(crate) legal: bool,
    pub(crate) pre_message_count: usize,
    pub(crate) post_message_count: usize,
    pub(crate) preserves_pairs: bool,
    pub(crate) preserves_order: bool,
    pub(crate) gate_open: bool,
    pub(crate) safe_to_reduce: bool,
    pub(crate) reducer_is_identity: bool,
}

#[derive(Debug, PartialEq, Eq)]
enum LeanVocabularyParseError<'a> {
    MissingNamespace,
    MissingToDefraDB,
    EmptyToDefraDB,
    MalformedArm {
        line_number: usize,
        line: &'a str,
        reason: &'static str,
    },
}

static LEAN_CONTRACT_SNAPSHOT: OnceLock<LeanContractSnapshot> = OnceLock::new();
const CONTRACT_JSON_BEGIN: &str = "---BEGIN DEFRA LEAN CONTRACT JSON---";
const CONTRACT_JSON_END: &str = "---END DEFRA LEAN CONTRACT JSON---";

pub(crate) fn lean_contract_snapshot() -> &'static LeanContractSnapshot {
    LEAN_CONTRACT_SNAPSHOT.get_or_init(load_lean_contract_snapshot)
}

pub(crate) fn lean_vocabulary_contract(domain: &str) -> &'static LeanVocabularyContract {
    lean_contract_snapshot()
        .vocabularies
        .iter()
        .find(|contract| contract.domain == domain)
        .unwrap_or_else(|| panic!("Lean vocabulary contract {domain:?} was not emitted"))
}

pub(crate) fn lean_state_machine_contract(domain: &str) -> &'static LeanStateMachineContract {
    lean_contract_snapshot()
        .state_machines
        .iter()
        .find(|contract| contract.domain == domain)
        .unwrap_or_else(|| panic!("Lean state-machine contract {domain:?} was not emitted"))
}

pub(crate) fn lean_request_transition_cases() -> &'static [LeanLifecycleTransitionCase] {
    &lean_contract_snapshot().request_transition_cases
}

pub(crate) fn lean_process_transition_cases() -> &'static [LeanLifecycleTransitionCase] {
    &lean_contract_snapshot().process_transition_cases
}

pub(crate) fn lean_runtime_reconcile_case(name: &str) -> &'static LeanRuntimeReconcileCase {
    lean_contract_snapshot()
        .runtime_reconcile_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean runtime-reconcile case {name:?} was not emitted"))
}

pub(crate) fn lean_apply_reconcile_cases() -> &'static [LeanApplyReconcileCase] {
    &lean_contract_snapshot().apply_reconcile_cases
}

pub(crate) fn lean_apply_reconcile_case(name: &str) -> &'static LeanApplyReconcileCase {
    lean_contract_snapshot()
        .apply_reconcile_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean apply-reconcile case {name:?} was not emitted"))
}

pub(crate) fn lean_session_recovery_case(name: &str) -> &'static LeanSessionRecoveryCase {
    lean_contract_snapshot()
        .session_recovery_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean session-recovery case {name:?} was not emitted"))
}

pub(crate) fn lean_client_shell_case(name: &str) -> &'static LeanClientShellCase {
    lean_contract_snapshot()
        .frontend_client_shell_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean ClientShell case {name:?} was not emitted"))
}

pub(crate) fn lean_desktop_client_shell_cases() -> &'static [LeanClientShellCase] {
    &lean_contract_snapshot().desktop_client_shell_cases
}

pub(crate) fn lean_inference_slot_accounting_cases() -> &'static [LeanInferenceSlotAccountingCase] {
    &lean_contract_snapshot().inference_slot_accounting_cases
}

pub(crate) fn lean_inference_slot_accounting_case(
    name: &str,
) -> &'static LeanInferenceSlotAccountingCase {
    lean_contract_snapshot()
        .inference_slot_accounting_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean inference slot-accounting case {name:?} was not emitted"))
}

pub(crate) fn lean_fleet_slot_accounting_case(name: &str) -> &'static LeanFleetSlotAccountingCase {
    lean_contract_snapshot()
        .fleet_slot_accounting_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean fleet slot-accounting case {name:?} was not emitted"))
}

pub(crate) fn lean_persistence_failure_policy_cases() -> &'static [LeanPersistenceFailurePolicyCase]
{
    &lean_contract_snapshot().persistence_failure_policy_cases
}

pub(crate) fn lean_storage_observation_runtime_cases(
) -> &'static [LeanStorageObservationRuntimeCase] {
    &lean_contract_snapshot().storage_observation_runtime_cases
}

pub(crate) fn lean_backend_health_admission_cases() -> &'static [LeanBackendHealthAdmissionCase] {
    &lean_contract_snapshot().backend_health_admission_cases
}

pub(crate) fn lean_native_filesystem_boundary_cases() -> &'static [LeanNativeFilesystemBoundaryCase]
{
    &lean_contract_snapshot().native_filesystem_boundary_cases
}

pub(crate) fn lean_tool_preflight_cases() -> &'static [LeanToolPreflightCase] {
    &lean_contract_snapshot().tool_preflight_cases
}

pub(crate) fn lean_tool_preflight_case(name: &str) -> &'static LeanToolPreflightCase {
    lean_contract_snapshot()
        .tool_preflight_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean tool preflight case {name:?} was not emitted"))
}

pub(crate) fn lean_tool_retry_cases() -> &'static [LeanToolRetryCase] {
    &lean_contract_snapshot().tool_retry_cases
}

pub(crate) fn lean_queue_deadline_cases() -> &'static [LeanQueueDeadlineConformanceCase] {
    &lean_contract_snapshot().queue_deadline_conformance_cases
}

pub(crate) fn lean_queue_deadline_case(name: &str) -> &'static LeanQueueDeadlineConformanceCase {
    lean_contract_snapshot()
        .queue_deadline_conformance_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean queue/deadline case {name:?} was not emitted"))
}

pub(crate) fn lean_recovery_sweep_cases() -> &'static [LeanRecoverySweepCase] {
    &lean_contract_snapshot().recovery_sweep_cases
}

pub(crate) fn lean_recovery_sweep_case(name: &str) -> &'static LeanRecoverySweepCase {
    lean_contract_snapshot()
        .recovery_sweep_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean recovery sweep case {name:?} was not emitted"))
}

pub(crate) fn lean_transcript_cases() -> &'static [LeanTranscriptCase] {
    &lean_contract_snapshot().transcript_conformance_cases
}

pub(crate) fn lean_transcript_case(name: &str) -> &'static LeanTranscriptCase {
    lean_contract_snapshot()
        .transcript_conformance_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean transcript case {name:?} was not emitted"))
}

pub(crate) fn lean_response_transition_cases() -> &'static [LeanResponseTransitionCase] {
    &lean_contract_snapshot().streaming_response_cases
}

pub(crate) fn lean_response_transition_case(name: &str) -> &'static LeanResponseTransitionCase {
    lean_contract_snapshot()
        .streaming_response_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean response-transition case {name:?} was not emitted"))
}

pub(crate) fn lean_compaction_reducer_cases() -> &'static [LeanCompactionReducerCase] {
    &lean_contract_snapshot().compaction_reducer_cases
}

pub(crate) fn lean_compaction_reducer_case(name: &str) -> &'static LeanCompactionReducerCase {
    lean_contract_snapshot()
        .compaction_reducer_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean compaction-reducer case {name:?} was not emitted"))
}

pub(crate) fn lean_event_delivery_transition_cases() -> &'static [LeanEventDeliveryTransitionCase] {
    &lean_contract_snapshot().event_delivery_transition_cases
}

pub(crate) fn lean_event_delivery_source_instances() -> &'static [LeanEventDeliverySourceInstance] {
    &lean_contract_snapshot().event_delivery_source_instances
}

pub(crate) fn lean_event_delivery_convergence_traces(
) -> &'static [LeanEventDeliveryConvergenceTrace] {
    &lean_contract_snapshot().event_delivery_convergence_traces
}

pub(crate) fn lean_tool_retry_case(name: &str) -> &'static LeanToolRetryCase {
    lean_contract_snapshot()
        .tool_retry_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean tool retry case {name:?} was not emitted"))
}

pub(crate) fn lean_mcp_health_cases() -> &'static [LeanMcpHealthCase] {
    &lean_contract_snapshot().mcp_health_cases
}

pub(crate) fn lean_mcp_health_k1_cases() -> Vec<&'static LeanMcpHealthCase> {
    lean_contract_snapshot()
        .mcp_health_cases
        .iter()
        .filter(|case| case.threshold_k == 1)
        .collect()
}

pub(crate) fn lean_command_policy_cases() -> &'static [LeanCommandPolicyCase] {
    &lean_contract_snapshot().command_policy_cases
}

pub(crate) fn lean_command_policy_case(name: &str) -> &'static LeanCommandPolicyCase {
    lean_contract_snapshot()
        .command_policy_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean command policy case {name:?} was not emitted"))
}

pub(crate) fn lean_command_sandbox_cases() -> &'static [LeanCommandSandboxCase] {
    &lean_contract_snapshot().command_sandbox_cases
}

pub(crate) fn lean_command_sandbox_case(name: &str) -> &'static LeanCommandSandboxCase {
    lean_contract_snapshot()
        .command_sandbox_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean command sandbox case {name:?} was not emitted"))
}

pub(crate) fn lean_command_env_cases() -> &'static [LeanCommandEnvCase] {
    &lean_contract_snapshot().command_env_cases
}

pub(crate) fn lean_command_env_case(name: &str) -> &'static LeanCommandEnvCase {
    lean_contract_snapshot()
        .command_env_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean command env case {name:?} was not emitted"))
}

pub(crate) fn lean_identity_structural_cases() -> &'static [LeanIdentityStructuralCase] {
    &lean_contract_snapshot().identity_structural_cases
}

pub(crate) fn lean_identity_contracts() -> &'static [LeanIdentityContract] {
    &lean_contract_snapshot().identity_contracts
}

pub(crate) fn lean_live_overlay_cases() -> &'static [LeanLiveOverlayCase] {
    &lean_contract_snapshot().live_overlay_cases
}

pub(crate) fn lean_vocabulary_values(domain: &str) -> Vec<&'static str> {
    lean_vocabulary_contract(domain)
        .values
        .iter()
        .map(String::as_str)
        .collect()
}

pub(crate) fn lean_trigger_dispatch_cases() -> &'static [LeanTriggerDispatchCase] {
    &lean_contract_snapshot().trigger_dispatch_cases
}

pub(crate) fn lean_trigger_dispatch_case_count() -> usize {
    lean_contract_snapshot().trigger_dispatch_case_count
}

pub(crate) fn assert_lean_contract_vocabulary_matches(spec: LeanContractVocabulary<'_>) {
    let lean_values = lean_vocabulary_values(spec.domain);
    let missing_from_lean = values_missing_from(spec.rust_values, &lean_values);
    let extra_in_lean = values_missing_from(&lean_values, spec.rust_values);
    let duplicate_rust_values = duplicate_values(spec.rust_values);
    let duplicate_lean_values = duplicate_values(&lean_values);

    assert!(
        spec.rust_values == lean_values.as_slice()
            && missing_from_lean.is_empty()
            && extra_in_lean.is_empty()
            && duplicate_rust_values.is_empty()
            && duplicate_lean_values.is_empty(),
        "Rust/Lean vocabulary contract mismatch\n  Lean contract domain: {}\n  Rust vocabulary source: {}\n  missing Lean values (present in Rust): {:?}\n  extra Lean values (absent from Rust): {:?}\n  duplicate Rust values: {:?}\n  duplicate Lean values: {:?}\n  Rust values: {:?}\n  Lean values: {:?}",
        spec.domain,
        spec.rust_source,
        missing_from_lean,
        extra_in_lean,
        duplicate_rust_values,
        duplicate_lean_values,
        spec.rust_values,
        lean_values
    );
}

pub(crate) fn assert_lean_contract_vocabulary_set_matches(spec: LeanContractVocabulary<'_>) {
    let lean_values = lean_vocabulary_values(spec.domain);
    let missing_from_lean = values_missing_from(spec.rust_values, &lean_values);
    let extra_in_lean = values_missing_from(&lean_values, spec.rust_values);
    let duplicate_rust_values = duplicate_values(spec.rust_values);
    let duplicate_lean_values = duplicate_values(&lean_values);

    assert!(
        missing_from_lean.is_empty()
            && extra_in_lean.is_empty()
            && duplicate_rust_values.is_empty()
            && duplicate_lean_values.is_empty(),
        "Rust/Lean vocabulary contract set mismatch\n  Lean contract domain: {}\n  Rust vocabulary source: {}\n  missing Lean values (present in Rust): {:?}\n  extra Lean values (absent from Rust): {:?}\n  duplicate Rust values: {:?}\n  duplicate Lean values: {:?}\n  Rust values: {:?}\n  Lean values: {:?}",
        spec.domain,
        spec.rust_source,
        missing_from_lean,
        extra_in_lean,
        duplicate_rust_values,
        duplicate_lean_values,
        spec.rust_values,
        lean_values
    );
}

pub(crate) fn assert_lean_transition_is_legal(domain: &str, from: &str, to: &str) {
    let machine = lean_state_machine_contract(domain);
    assert!(
        machine
            .legal_transitions
            .iter()
            .any(|pair| pair.from == from && pair.to == to),
        "Lean state-machine contract {domain:?} does not allow transition {from:?} -> {to:?}\n  legal transitions: {:?}",
        machine.legal_transitions
    );
    assert!(
        !machine
            .illegal_transitions
            .iter()
            .any(|pair| pair.from == from && pair.to == to),
        "Lean state-machine contract {domain:?} marks transition {from:?} -> {to:?} as both legal and illegal"
    );
}

pub(crate) fn assert_lean_transition_is_illegal(domain: &str, from: &str, to: &str) {
    let machine = lean_state_machine_contract(domain);
    assert!(
        machine
            .illegal_transitions
            .iter()
            .any(|pair| pair.from == from && pair.to == to),
        "Lean state-machine contract {domain:?} does not mark transition {from:?} -> {to:?} illegal\n  illegal transitions: {:?}",
        machine.illegal_transitions
    );
    assert!(
        !machine
            .legal_transitions
            .iter()
            .any(|pair| pair.from == from && pair.to == to),
        "Lean state-machine contract {domain:?} marks transition {from:?} -> {to:?} as both legal and illegal"
    );
}

pub(crate) fn assert_state_machine_contract_is_complete(domain: &str) {
    let machine = lean_state_machine_contract(domain);
    let duplicate_states = duplicate_string_values(&machine.states);
    let duplicate_actions = duplicate_string_values(&machine.actions);
    let duplicate_legal_pairs = duplicate_transition_pairs(&machine.legal_transitions);
    let duplicate_illegal_pairs = duplicate_transition_pairs(&machine.illegal_transitions);
    let expected_pairs = machine.state_count * machine.state_count;
    let actual_pairs = machine.legal_transitions.len() + machine.illegal_transitions.len();

    assert!(
        machine.state_count == machine.states.len()
            && duplicate_states.is_empty()
            && duplicate_actions.is_empty()
            && duplicate_legal_pairs.is_empty()
            && duplicate_illegal_pairs.is_empty()
            && actual_pairs == expected_pairs
            && machine
                .legal_transitions
                .iter()
                .all(|pair| !machine.illegal_transitions.contains(pair))
            && machine
                .legal_transitions
                .iter()
                .all(|pair| machine.states.contains(&pair.from) && machine.states.contains(&pair.to))
            && machine
                .illegal_transitions
                .iter()
                .all(|pair| machine.states.contains(&pair.from) && machine.states.contains(&pair.to)),
        "Lean state-machine contract {domain:?} is incomplete or malformed\n  state_count: {}\n  states: {:?}\n  actions: {:?}\n  legal transitions: {:?}\n  illegal transitions: {:?}\n  duplicate states: {:?}\n  duplicate actions: {:?}\n  duplicate legal pairs: {:?}\n  duplicate illegal pairs: {:?}\n  expected pair partition size: {}\n  actual pair partition size: {}",
        machine.state_count,
        machine.states,
        machine.actions,
        machine.legal_transitions,
        machine.illegal_transitions,
        duplicate_states,
        duplicate_actions,
        duplicate_legal_pairs,
        duplicate_illegal_pairs,
        expected_pairs,
        actual_pairs
    );
}

pub(crate) fn assert_lifecycle_transition_cases_partition(
    domain: &str,
    states: &[&str],
    cases: &[LeanLifecycleTransitionCase],
) {
    let expected_pairs = states
        .iter()
        .flat_map(|from| {
            states
                .iter()
                .map(move |to| ((*from).to_string(), (*to).to_string()))
        })
        .collect::<BTreeSet<_>>();
    let mut actual_pairs = BTreeSet::new();
    let mut invalid_cases = Vec::new();

    for case in cases {
        if case.domain != domain {
            invalid_cases.push(format!("{} has wrong domain {:?}", case.name, case.domain));
        }
        if !matches!(
            case.classification.as_str(),
            "legal" | "illegal" | "productUnreachable"
        ) {
            invalid_cases.push(format!(
                "{} has invalid classification {:?}",
                case.name, case.classification
            ));
        }
        if case.classification == "legal" && case.action.is_none() {
            invalid_cases.push(format!("{} legal case missing action", case.name));
        }
        if case.classification != "legal" && case.action.is_some() {
            invalid_cases.push(format!("{} non-legal case has action", case.name));
        }
        if case.classification == "productUnreachable" && case.boundary.is_none() {
            invalid_cases.push(format!(
                "{} product-unreachable case missing boundary",
                case.name
            ));
        }
        if case.classification != "productUnreachable" && case.boundary.is_some() {
            invalid_cases.push(format!("{} reachable case has boundary", case.name));
        }
        if !expected_pairs.contains(&(case.from.clone(), case.to.clone())) {
            invalid_cases.push(format!(
                "{} has pair outside {domain} vocabulary: {:?} -> {:?}",
                case.name, case.from, case.to
            ));
        }
        if !actual_pairs.insert((case.from.clone(), case.to.clone())) {
            invalid_cases.push(format!(
                "{} duplicates pair {:?} -> {:?}",
                case.name, case.from, case.to
            ));
        }
    }

    let missing = expected_pairs
        .difference(&actual_pairs)
        .cloned()
        .collect::<Vec<_>>();
    let extra = actual_pairs
        .difference(&expected_pairs)
        .cloned()
        .collect::<Vec<_>>();

    assert!(
        invalid_cases.is_empty() && missing.is_empty() && extra.is_empty(),
        "Lean lifecycle transition cases for {domain:?} do not form a state^2 partition\n  invalid cases: {:?}\n  missing pairs: {:?}\n  extra pairs: {:?}\n  cases: {:?}",
        invalid_cases,
        missing,
        extra,
        cases
    );
}

pub(crate) fn assert_lean_to_defradb_vocabulary_matches(spec: LeanVocabulary<'_>) {
    let lean_values = lean_to_defradb_values(spec.lean_file, spec.model, spec.namespace);
    let missing_from_lean = values_missing_from(spec.rust_values, &lean_values);
    let extra_in_lean = values_missing_from(&lean_values, spec.rust_values);
    let duplicate_rust_values = duplicate_values(spec.rust_values);
    let duplicate_lean_values = duplicate_values(&lean_values);

    assert!(
        spec.rust_values == lean_values.as_slice()
            && missing_from_lean.is_empty()
            && extra_in_lean.is_empty()
            && duplicate_rust_values.is_empty()
            && duplicate_lean_values.is_empty(),
        "Rust/Lean toDefraDB vocabulary mismatch\n  Lean file: {}\n  namespace: {}\n  Rust vocabulary source: {}\n  missing Lean values (present in Rust): {:?}\n  extra Lean values (absent from Rust): {:?}\n  duplicate Rust values: {:?}\n  duplicate Lean values: {:?}\n  Rust values: {:?}\n  Lean values: {:?}",
        spec.lean_file,
        spec.namespace,
        spec.rust_source,
        missing_from_lean,
        extra_in_lean,
        duplicate_rust_values,
        duplicate_lean_values,
        spec.rust_values,
        lean_values
    );
}

fn load_lean_contract_snapshot() -> LeanContractSnapshot {
    let proofs_dir = proofs_dir();
    run_lake_build(&proofs_dir);
    let output = Command::new("lake")
        .args(["env", "lean", "--run", "Proofs/Conformance/Contracts.lean"])
        .current_dir(&proofs_dir)
        .output()
        .unwrap_or_else(|error| {
            panic!(
                "failed to run Lean conformance contract generator in {}: {error}",
                proofs_dir.display()
            )
        });

    if !output.status.success() {
        panic!(
            "Lean conformance contract generator failed\n  cwd: {}\n  status: {}\n  stdout:\n{}\n  stderr:\n{}",
            proofs_dir.display(),
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let json = extract_contract_json(&stdout);
    serde_json::from_str(json).unwrap_or_else(|error| {
        panic!(
            "failed to parse Lean conformance contract JSON: {error}\n  stdout:\n{}",
            stdout
        )
    })
}

fn run_lake_build(proofs_dir: &Path) {
    let output = Command::new("lake")
        .args(["build", "Proofs.Conformance.Contracts"])
        .current_dir(proofs_dir)
        .output()
        .unwrap_or_else(|error| {
            panic!(
                "failed to build Lean conformance contract target in {}: {error}",
                proofs_dir.display()
            )
        });

    if !output.status.success() {
        panic!(
            "Lean conformance contract build failed\n  cwd: {}\n  status: {}\n  stdout:\n{}\n  stderr:\n{}",
            proofs_dir.display(),
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn proofs_dir() -> PathBuf {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let direct = manifest_dir.join("proofs");
    if direct.exists() {
        return direct;
    }
    for ancestor in manifest_dir.ancestors() {
        let candidate = ancestor.join("crates/defra-agent/proofs");
        if candidate.exists() {
            return candidate;
        }
    }
    manifest_dir
        .parent()
        .map(|parent| parent.join("defra-agent/proofs"))
        .filter(|candidate| candidate.exists())
        .unwrap_or(direct)
}

fn extract_contract_json(stdout: &str) -> &str {
    let begin = unique_marker_position(stdout, CONTRACT_JSON_BEGIN);
    let end = unique_marker_position(stdout, CONTRACT_JSON_END);
    assert!(
        begin < end,
        "Lean contract JSON sentinel order is invalid\n  stdout:\n{}",
        stdout
    );

    let json = stdout[begin + CONTRACT_JSON_BEGIN.len()..end].trim();
    assert!(
        !json.is_empty(),
        "Lean contract JSON sentinel block was empty\n  stdout:\n{}",
        stdout
    );
    json
}

fn unique_marker_position(stdout: &str, marker: &str) -> usize {
    let positions = stdout
        .match_indices(marker)
        .map(|(position, _)| position)
        .collect::<Vec<_>>();
    match positions.as_slice() {
        [position] => *position,
        [] => panic!("Lean contract generator stdout did not contain {marker:?}: {stdout}"),
        _ => panic!(
            "Lean contract generator stdout contained duplicate {marker:?} sentinels: {stdout}"
        ),
    }
}

pub(crate) fn lean_to_defradb_values<'a>(
    lean_file: &str,
    model: &'a str,
    namespace: &str,
) -> Vec<&'a str> {
    parse_lean_to_defradb_values(model, namespace)
        .unwrap_or_else(|error| panic!("{}", error.message(lean_file, namespace)))
}

fn parse_lean_to_defradb_values<'a>(
    model: &'a str,
    namespace: &str,
) -> Result<Vec<&'a str>, LeanVocabularyParseError<'a>> {
    let namespace_start = format!("namespace {namespace}");
    let namespace_end = format!("end {namespace}");
    let mut found_namespace = false;
    let mut found_to_defradb = false;
    let mut in_namespace = false;
    let mut in_to_defradb = false;
    let mut values = Vec::new();

    for (index, line) in model.lines().enumerate() {
        let line_number = index + 1;
        let trimmed = line.trim();

        if !in_namespace {
            if trimmed == namespace_start {
                found_namespace = true;
                in_namespace = true;
            }
            continue;
        }

        if trimmed == namespace_end {
            break;
        }

        if !in_to_defradb {
            if trimmed.starts_with("def toDefraDB") {
                found_to_defradb = true;
                in_to_defradb = true;
            }
            continue;
        }

        if trimmed.is_empty() || trimmed.starts_with("--") {
            continue;
        }

        if trimmed.starts_with('|') {
            values.push(parse_to_defradb_arm(trimmed, line_number)?);
            continue;
        }

        if values.is_empty() {
            return Err(LeanVocabularyParseError::MalformedArm {
                line_number,
                line: trimmed,
                reason: "expected a toDefraDB pattern arm starting with `| .`",
            });
        }

        break;
    }

    if !found_namespace {
        return Err(LeanVocabularyParseError::MissingNamespace);
    }
    if !found_to_defradb {
        return Err(LeanVocabularyParseError::MissingToDefraDB);
    }
    if values.is_empty() {
        return Err(LeanVocabularyParseError::EmptyToDefraDB);
    }

    Ok(values)
}

fn parse_to_defradb_arm<'a>(
    trimmed: &'a str,
    line_number: usize,
) -> Result<&'a str, LeanVocabularyParseError<'a>> {
    let Some(rest) = trimmed.strip_prefix("| .") else {
        return Err(LeanVocabularyParseError::MalformedArm {
            line_number,
            line: trimmed,
            reason: "expected a toDefraDB pattern arm starting with `| .`",
        });
    };
    let Some((_constructor, value)) = rest.split_once("=>") else {
        return Err(LeanVocabularyParseError::MalformedArm {
            line_number,
            line: trimmed,
            reason: "missing `=>`",
        });
    };

    parse_string_literal(value.trim(), line_number, trimmed)
}

fn parse_string_literal<'a>(
    value: &'a str,
    line_number: usize,
    line: &'a str,
) -> Result<&'a str, LeanVocabularyParseError<'a>> {
    let Some(after_opening_quote) = value.strip_prefix('"') else {
        return Err(LeanVocabularyParseError::MalformedArm {
            line_number,
            line,
            reason: "expected a string literal after `=>`",
        });
    };
    let Some(end_index) = after_opening_quote.find('"') else {
        return Err(LeanVocabularyParseError::MalformedArm {
            line_number,
            line,
            reason: "string literal is missing a closing quote",
        });
    };
    let literal = &after_opening_quote[..end_index];
    let trailing = after_opening_quote[end_index + 1..].trim();
    if !trailing.is_empty() && !trailing.starts_with("--") {
        return Err(LeanVocabularyParseError::MalformedArm {
            line_number,
            line,
            reason: "expected only optional comment text after the string literal",
        });
    }

    Ok(literal)
}

fn values_missing_from<'a>(expected: &[&'a str], actual: &[&str]) -> Vec<&'a str> {
    expected
        .iter()
        .copied()
        .filter(|value| !actual.contains(value))
        .collect()
}

fn duplicate_values<'a>(values: &[&'a str]) -> Vec<&'a str> {
    let mut seen = Vec::new();
    let mut duplicates = Vec::new();
    for value in values {
        if seen.contains(value) {
            if !duplicates.contains(value) {
                duplicates.push(*value);
            }
        } else {
            seen.push(*value);
        }
    }
    duplicates
}

fn duplicate_string_values(values: &[String]) -> Vec<String> {
    let refs = values.iter().map(String::as_str).collect::<Vec<_>>();
    duplicate_values(&refs)
        .into_iter()
        .map(str::to_string)
        .collect()
}

fn duplicate_transition_pairs(values: &[LeanTransitionPair]) -> Vec<LeanTransitionPair> {
    let mut seen = Vec::new();
    let mut duplicates = Vec::new();
    for value in values {
        if seen.contains(value) {
            if !duplicates.contains(value) {
                duplicates.push(value.clone());
            }
        } else {
            seen.push(value.clone());
        }
    }
    duplicates
}

impl LeanVocabularyParseError<'_> {
    fn message(&self, lean_file: &str, namespace: &str) -> String {
        match self {
            Self::MissingNamespace => format!(
                "Lean toDefraDB vocabulary parse failed\n  Lean file: {lean_file}\n  namespace: {namespace}\n  reason: namespace block was not found"
            ),
            Self::MissingToDefraDB => format!(
                "Lean toDefraDB vocabulary parse failed\n  Lean file: {lean_file}\n  namespace: {namespace}\n  reason: def toDefraDB was not found in the namespace"
            ),
            Self::EmptyToDefraDB => format!(
                "Lean toDefraDB vocabulary parse failed\n  Lean file: {lean_file}\n  namespace: {namespace}\n  reason: def toDefraDB has no parsed string-valued arms"
            ),
            Self::MalformedArm {
                line_number,
                line,
                reason,
            } => format!(
                "Lean toDefraDB vocabulary parse failed\n  Lean file: {lean_file}\n  namespace: {namespace}\n  line: {line_number}\n  reason: {reason}\n  source: {line}"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NORMAL_LEAN: &str = r#"
inductive Sample where
  | one
  | two

namespace Sample

def toDefraDB : Sample -> String
  | .one => "one"
  | .two => "two"

def fromDefraDB? : String -> Option Sample
  | "one" => some .one
  | "two" => some .two
  | _ => none

end Sample
"#;

    #[test]
    fn parses_normal_to_defradb_values() {
        assert_eq!(
            parse_lean_to_defradb_values(NORMAL_LEAN, "Sample").unwrap(),
            vec!["one", "two"]
        );
    }

    #[test]
    fn reports_missing_namespace() {
        let error = parse_lean_to_defradb_values(NORMAL_LEAN, "Missing").unwrap_err();
        assert_eq!(error, LeanVocabularyParseError::MissingNamespace);
    }

    #[test]
    fn reports_missing_to_defradb() {
        let lean = r#"
namespace Sample

def unrelated : String := "value"

end Sample
"#;

        let error = parse_lean_to_defradb_values(lean, "Sample").unwrap_err();
        assert_eq!(error, LeanVocabularyParseError::MissingToDefraDB);
    }

    #[test]
    fn reports_malformed_arm() {
        let lean = r#"
namespace Sample

def toDefraDB : Sample -> String
  | .one "one"

end Sample
"#;

        let error = parse_lean_to_defradb_values(lean, "Sample").unwrap_err();
        assert!(matches!(
            error,
            LeanVocabularyParseError::MalformedArm {
                reason: "missing `=>`",
                ..
            }
        ));
    }

    #[test]
    fn parses_target_from_multiple_namespaces() {
        let lean = r#"
namespace Other

def toDefraDB : Other -> String
  | .one => "other"

end Other

namespace Sample

def toDefraDB : Sample -> String
  | .one => "sample"

end Sample
"#;

        assert_eq!(
            parse_lean_to_defradb_values(lean, "Sample").unwrap(),
            vec!["sample"]
        );
    }

    #[test]
    fn extracts_contract_json_between_sentinels() {
        let stdout = format!(
            "debug {{noise}}\n{CONTRACT_JSON_BEGIN}\n{{\"ok\":true}}\n{CONTRACT_JSON_END}\nmore {{noise}}\n"
        );

        assert_eq!(extract_contract_json(&stdout), "{\"ok\":true}");
    }

    #[test]
    fn assertion_message_identifies_sources_and_differences() {
        let previous_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let result = std::panic::catch_unwind(|| {
            assert_lean_to_defradb_vocabulary_matches(LeanVocabulary {
                lean_file: "sample.lean",
                model: NORMAL_LEAN,
                namespace: "Sample",
                rust_source: "Sample::ALL",
                rust_values: &["one", "three"],
            });
        });
        std::panic::set_hook(previous_hook);

        let panic = result.expect_err("assertion should fail");
        let message = panic_message(panic.as_ref());
        assert!(message.contains("Lean file: sample.lean"));
        assert!(message.contains("namespace: Sample"));
        assert!(message.contains("Rust vocabulary source: Sample::ALL"));
        assert!(message.contains("missing Lean values (present in Rust): [\"three\"]"));
        assert!(message.contains("extra Lean values (absent from Rust): [\"two\"]"));
    }

    fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
        payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| {
                payload
                    .downcast_ref::<&str>()
                    .map(|message| message.to_string())
            })
            .unwrap_or_else(|| "non-string panic".to_string())
    }
}
