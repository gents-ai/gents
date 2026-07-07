use super::*;

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
pub(crate) struct LeanManagedExecLivenessCase {
    pub(crate) name: String,
    pub(crate) trigger: String,
    pub(crate) pre_exec_state: String,
    pub(crate) pre_tool_state: String,
    pub(crate) expected_exec_state: String,
    pub(crate) expected_tool_state: String,
    pub(crate) max_steps: usize,
    pub(crate) kill_signal_required: bool,
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

/// Generated witness for `Proofs.BackendHealth.step` (#640): the scheduled
/// inference-backend prober's per-runtime hysteresis machine. Unlike
/// `LeanMcpHealthCase` the machine is total (no removal), so `next_state` /
/// `next_count` are non-optional, and each row carries the `blocks_routing`
/// projection of the next state (the routing veto the admission merge
/// consumes).
#[derive(Debug, Deserialize)]
pub(crate) struct LeanBackendHealthCase {
    pub(crate) name: String,
    pub(crate) start_state: String,
    pub(crate) start_count: usize,
    pub(crate) event: String,
    pub(crate) threshold_k: usize,
    pub(crate) next_state: String,
    pub(crate) next_count: usize,
    pub(crate) blocks_routing: bool,
}
