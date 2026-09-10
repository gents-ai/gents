//! Contract-section deserialization targets shared by the trigger, apply
//! publication, runtime-reconcile, client-behavior-readiness, and
//! startup-readiness conformance consumers. Every struct mirrors the JSON
//! emitted by the Lean owners:
//!
//! - triggers: `Proofs/Conformance/Triggers/Contracts.lean`
//! - apply publication: `Proofs/ApplyReconcile/Publication.lean` +
//!   `Proofs/ApplyReconcile/ContractCases.lean`
//! - runtime reconcile / readiness: `Proofs/RuntimeReconcile/*.lean`
//!
//! The trigger consumers (`tests/conformance/triggers.rs`) are owned by the
//! trigger worker; this file only keeps the deserialization surface honest
//! against the emitted JSON.

use super::*;

/// A trigger's key as emitted by the dispatch contract.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanTriggerKeyContract {
    pub(crate) trigger_id: String,
    pub(crate) trigger_kind: String,
}

/// A trigger-dispatch case. `intent_task_id` is the task named by the dispatch
/// intent; `selected_task_id` is the task the dispatcher actually selected.
/// Schedules and event sources select their configured task before common
/// context/inference resolution, so a stale intent task id is overridden and
/// the selected id differs; manual dispatches carry no selected task.
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

/// The shared trigger/callback key, including its owner and config identity.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanEventGroupKeyContract {
    pub(crate) agent_did: String,
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
    pub(crate) requested_behavior: Option<usize>,
    pub(crate) pre_default_behavior: usize,
    pub(crate) pre_session_behavior: Option<usize>,
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
    pub(crate) tracked_request_behavior: usize,
    pub(crate) tracked_session_behavior: usize,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LeanClientBehaviorReadinessCase {
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

/// A document key (`{collection, id}`) as referenced inside publication rows.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanApplyDocRef {
    pub(crate) agent_did: String,
    pub(crate) collection: String,
    pub(crate) id: String,
}

/// A desired-state row of a publication candidate: its key, owning agent DID,
/// content, and the references that must close inside the candidate.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanApplyDesiredRow {
    #[serde(rename = "ref")]
    pub(crate) target: LeanApplyDocRef,
    pub(crate) content: String,
    pub(crate) refs: Vec<LeanApplyDocRef>,
}

/// A runtime observation row: a document key and the live value recorded for
/// it. Observations are never written by a publication.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanApplyLiveRow {
    #[serde(rename = "ref")]
    pub(crate) target: LeanApplyDocRef,
    pub(crate) value: String,
}

/// An atomic-publication case (`Publication.lean`): `publish old candidate`
/// replaces the whole desired snapshot with the candidate when the candidate's
/// reference closure holds, and leaves the prior state untouched otherwise.
/// The retired ranked/per-write fields (steps, write/prune order, selected
/// docs, prefix/retry machinery) are no longer emitted and no longer modeled.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanApplyReconcileCase {
    pub(crate) name: String,
    /// Whether the candidate's references close (`referencesClosed`).
    pub(crate) accepted: bool,
    /// The complete desired snapshot the publication would install.
    pub(crate) candidate: Vec<LeanApplyDesiredRow>,
    /// Desired state before the publication (fixed retained skill row).
    pub(crate) pre_desired: Vec<LeanApplyDesiredRow>,
    /// Runtime observations before the publication (fixed key scaffold plus
    /// the candidate keys).
    pub(crate) pre_live: Vec<LeanApplyLiveRow>,
    /// Live state after publication: observations preserved verbatim.
    pub(crate) expected_after_live: Vec<LeanApplyLiveRow>,
    /// Desired state after one publication.
    pub(crate) expected_after_desired: Vec<LeanApplyDesiredRow>,
    /// Desired state after republishing the same candidate (idempotence).
    pub(crate) expected_retry_desired: Vec<LeanApplyDesiredRow>,
    /// Fixture agreement that observations survive the publication.
    pub(crate) observations_preserved: bool,
}

/// Semantic readiness publication traces, independent of elapsed idle time.
#[derive(Debug, Deserialize)]
pub(crate) struct LeanReadinessPublicationCase {
    pub(crate) states: Vec<u64>,
    pub(crate) publishes: Vec<bool>,
}

/// Startup-readiness vectors for the bounded build-failure barrier (#559).
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
