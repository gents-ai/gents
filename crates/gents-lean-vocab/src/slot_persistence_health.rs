use super::*;

#[derive(Debug, Deserialize)]
pub struct LeanInferenceSlotAccountingCase {
    pub name: String,
    pub property: String,
    pub backend_id: String,
    pub pre_state: String,
    pub post_state: String,
    pub contribution: usize,
    pub expected_contribution: usize,
    pub pre_contribution: usize,
    pub post_contribution: usize,
    pub released_slot: bool,
    pub permit_drop_terminalization: bool,
    pub row_states: Vec<String>,
    pub row_backend_ids: Vec<String>,
    pub reconstructed_running_count: usize,
    pub max_concurrent: usize,
    pub bounded_by_max_concurrent: bool,
}

#[derive(Debug, Deserialize)]
pub struct LeanFleetSlotAccountingCase {
    pub name: String,
    pub property: String,
    pub backend_id: String,
    pub request_state: String,
    pub admission_state: String,
    pub contribution: usize,
    pub expected_contribution: usize,
    pub active_count: usize,
    pub scheduler_running: usize,
    pub slot_count: usize,
    pub row_states: Vec<String>,
    pub row_backend_ids: Vec<String>,
    pub reconstructed_running_count: usize,
    pub max_concurrent: usize,
    pub bounded_by_max_concurrent: bool,
    pub aggregate_reconstructed_not_persisted: bool,
}

#[derive(Debug, Deserialize)]
pub struct LeanPersistenceFailurePolicyCase {
    pub name: String,
    pub policy: String,
    pub action: String,
    pub pre_persistence: String,
    pub post_persistence: String,
    pub post_storage_observation: String,
    pub hook_decision: String,
    pub records_failure: bool,
    pub records_success: bool,
    pub external_durability_claimed: bool,
}

#[derive(Debug, Deserialize)]
pub struct LeanStorageObservationRuntimeCase {
    pub name: String,
    pub policy: String,
    pub action: String,
    pub pre_observation: String,
    pub mutation_result: String,
    pub post_observation: String,
    pub post_persistence: String,
    pub hook_result: String,
    pub records_failure: bool,
    pub records_success: bool,
    pub terminal_write_observed: bool,
    pub external_visibility_claimed: bool,
}

#[derive(Debug, Deserialize)]
pub struct LeanBackendHealthAdmissionCase {
    pub name: String,
    pub enabled: bool,
    pub probe_status: String,
    pub expected_available: bool,
    pub admission_decision: String,
    pub observed_document_only: bool,
    pub external_endpoint_freshness_claimed: bool,
}

#[derive(Debug, Deserialize)]
pub struct LeanNativeFilesystemBoundaryCase {
    pub name: String,
    pub tool_name: String,
    pub work_class: String,
    pub boundary: String,
    pub inner_poll_blocks: bool,
    pub request_deadline_ms: usize,
    pub blocker_ms: usize,
    pub expected_terminal: String,
    pub expected_failure_class: Option<String>,
    pub queue_advances_before_blocker_returns: bool,
}

#[derive(Debug, Deserialize)]
pub struct LeanManagedExecToolBoundaryCase {
    pub name: String,
    pub tool_name: String,
    pub work_class: String,
    pub boundary: String,
    pub kill_scope: String,
    pub timeout_requires_kill: bool,
    pub cancel_requires_kill: bool,
    pub descendants_in_termination_scope: bool,
    pub capture_drain_bounded: bool,
}

#[derive(Debug, Deserialize)]
pub struct LeanPairingReconcileShutdownBoundaryCase {
    pub name: String,
    pub supervisor: String,
    pub work_class: String,
    pub boundary: String,
    pub per_admin_call_timeout_ms: usize,
    pub cancellation_observed_inside_sweep: bool,
    pub current_admin_future_dropped: bool,
    pub remaining_peers_skipped: bool,
    pub shutdown_join_bounded: bool,
}

#[derive(Debug, Deserialize)]
pub struct LeanPairingReconcileSweepRetryBoundaryCase {
    pub name: String,
    pub supervisor: String,
    pub work_class: String,
    pub boundary: String,
    pub failure_scope: String,
    pub failure_terminal: bool,
    pub retry_trigger: String,
    pub cancellation_prioritized: bool,
    pub convergence_retried: bool,
}

#[derive(Debug, Deserialize)]
pub struct LeanPairingReconcileSweepSchedulingCase {
    pub name: String,
    pub supervisor: String,
    pub work_class: String,
    pub boundary: String,
    pub max_concurrent_peer_preparations: usize,
    pub peer_preparation_bounded: bool,
    pub topology_mutation_serialized: bool,
    pub stale_peer_blocks_ready_peer: bool,
    pub every_peer_result_accounted: bool,
}

#[derive(Debug, Deserialize)]
pub struct LeanManagedExecLivenessCase {
    pub name: String,
    pub trigger: String,
    pub pre_exec_state: String,
    pub pre_tool_state: String,
    pub expected_exec_state: String,
    pub expected_tool_state: String,
    pub max_steps: usize,
    pub kill_signal_required: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanProcessStopCase {
    pub name: String,
    pub live_evidence: String,
    pub worker_released: bool,
    pub settle_completed: bool,
    pub settle_cancelled: bool,
    pub settle_timed_out: bool,
    pub settle_failed: bool,
    pub settle_panicked: bool,
    pub retire_before_commit: bool,
    pub retire_after_commit: bool,
    pub preserve_timeout: bool,
    pub preserve_failure: bool,
    pub pending_failure_beats_cancel: bool,
    pub pending_failure_resolves: bool,
    pub retained_completion_notified: bool,
    pub retired_completion_notified: bool,
    pub before: String,
    pub after: String,
    pub may_terminate: bool,
    pub retain_record: bool,
    pub outcome: String,
    pub cancel_reply: String,
    pub durable_state: String,
}

#[derive(Debug, Deserialize)]
pub struct LeanToolPreflightCase {
    pub name: String,
    pub health: String,
    pub schema_status: String,
    pub decision: String,
    pub failure_class: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct LeanToolRetryCase {
    pub name: String,
    pub operation: String,
    pub idempotency: String,
    pub failure_class: String,
    pub disposition: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCompletionRetryCase {
    pub name: String,
    pub domain: String,
    pub action: String,
    pub failure_origin: Option<String>,
    pub classified_failure: Option<String>,
    pub legal: bool,
    pub pre_phase: String,
    pub pre_now: i64,
    pub pre_deadline: Option<i64>,
    pub pre_scheduled_wake: Option<i64>,
    pub expected_phase: Option<String>,
    pub expected_transport_used: Option<usize>,
    pub expected_resample_used: Option<usize>,
    pub expected_attempt: Option<usize>,
    pub expected_account_used: Option<usize>,
    pub expected_repair_used: Option<bool>,
    pub expected_usage_charged: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct LeanMcpHealthCase {
    pub name: String,
    pub start_state: String,
    pub start_count: usize,
    pub event: String,
    pub threshold_k: usize,
    pub next_state: Option<String>,
    pub next_count: Option<usize>,
    pub rust_projection: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct LeanBackendHealthCase {
    pub name: String,
    pub start_state: String,
    pub start_count: usize,
    pub event: String,
    pub threshold_k: usize,
    pub next_state: String,
    pub next_count: usize,
    pub blocks_routing: bool,
    pub readiness_reason: Option<String>,
}
