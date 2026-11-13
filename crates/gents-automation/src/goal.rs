use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use defra_node::Node as EmbeddedNode;
use gents_protocol::goal_status::GoalStatus;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use serde::{Deserialize, Serialize};

use gents_store::graphql::{escape_graphql_string, graphql_with_transaction_retry, rows};

mod backed_request;
pub mod blocked_turn;
mod claimed_publication;
mod client_control;
mod client_submission;
pub mod continuation;
mod controller;
mod creation;
mod document;
mod first_request_binding;
mod history;
pub mod lifecycle_hooks;
mod load;
mod observation;
pub mod operator_resume;
#[cfg(any(test, feature = "test-fixtures"))]
pub use operator_resume::support as operator_resume_test_support;
pub mod readiness_gate;
mod request_assignment;
mod request_head;
mod reset_resume;
pub mod session_goal_control;
pub mod session_message;
mod state;
mod task_fire;
mod tool_context;
mod transition;
mod usage;
mod write;
pub use claimed_publication::{
    publish_claimed_continuation_with_identity,
    stop_claimed_continuation_for_unavailable_agent_from_access,
};
pub(crate) use client_control::{
    attach_client_goal_graphs, prepare_goal_control, stage_goal_control,
};
pub use client_submission::{
    load_applied_opening_goal_id, load_applied_opening_goal_ids,
    load_applied_opening_goal_provenance, prepare_session_goal_control,
    recover_session_goal_control, submit_prepared_session_goal_control, OpeningGoalProvenance,
};
pub use first_request_binding::bind_goal_first_request_in_txn;
pub use first_request_binding::task_assignment_goal_id;
pub(crate) use first_request_binding::validate_task_goal_identity_in_txn;
use first_request_binding::{continuation_assignment, continuation_reference_matches};
pub use history::load_goal_versions_for_session;
pub(crate) use history::{load_assignment_versions_for_session, GoalAssignmentVersion};
pub use observation::{
    bind_goal_input, load_client_session_goal_observation_state, load_session_goal_observation,
    observe_session_goal, prepare_client_goal_input, prepare_goal_input_in_txn,
    project_session_goal, GoalInputRefused, SessionGoalNextAction, SessionGoalObservationState,
    SessionGoalProjectionRead, SessionGoalUnknownCause,
};
pub use observation::{
    reconcile_goal_observations_with_identities, SOURCES as GOAL_OBSERVATION_SOURCES,
};
pub use operator_resume::{
    resume_goal_on_account, resume_goal_request, GoalResumeOnReceipt, GoalResumeReceipt,
};
pub use readiness_gate::{
    gate_claimed_goal_continuation, gate_goal_continuation, goal_agent_observation,
    goal_failure_cause, goal_readiness_newer_than_terminal, next_goal_infrastructure_retries,
    observe_goal_agent, observe_goal_agent_readiness, GoalAgentObservation, GoalAgentReadiness,
    GoalClaimedDecision, GoalContinuationFacts, GoalFailureCause, GoalGatedDecision,
    ObservedGoalAgent, GOAL_READINESS_WAIT_PREFIX,
};
pub use readiness_gate::{goal_reconcile_settled, GoalReconcileSettled};
pub(crate) use request_assignment::latest_assignment_goals;
pub use request_assignment::RequestAssignmentObservation;
pub use request_assignment::{load_request_assignment, load_request_assignment_scopes};
pub use request_head::authenticated_goal_request_members;
pub(crate) use request_head::{
    assignment_request_id_in_txn, goal_session_is_idle, latest_goal_request, load_session_requests,
    request_selection_fields, verify_goal_continuation_edge,
};
pub use reset_resume::{resume_at_reset, ResetResume};
mod question_wait;
pub(crate) use backed_request::fingerprint::GoalBackedRequestFingerprint;
pub(crate) use backed_request::submit_goal_backed_request;
pub use backed_request::submit_goal_backed_request_with_identity;
pub use backed_request::GoalBackedRequestDisposition;
pub(crate) use backed_request::{
    stage_goal_backed_request_in_txn, stage_goal_backed_request_with_identity_in_txn,
};
pub use controller::{
    claim_continuation_from_access, claim_retry_continuation_from_access,
    update_goal_fields_if_status_from_access,
};
pub use creation::create_goal_for_request_from_access;
pub(crate) use creation::load_goal_creation_claim_for_key_in_txn;
use creation::stage_goal_and_claim;
pub use creation::{
    create_goal_for_session_from_access, replacement_goal_identity, CreateGoalForSessionError,
    CreateGoalForSessionOutcome, GoalCreationIdentity,
};
pub use document::{snapshot, GoalDocument};
use gents_protocol::goal::{deterministic_goal_creation_key, deterministic_goal_id, GOAL_FIELDS};
pub(crate) use load::sort_goals_canonical;
pub use load::{load_canonical_goal, load_canonical_goal_in_txn, load_goals_for_session};
pub use question_wait::awaiting_goal_question_ids;
pub use state::{
    apply_operator_status_transition, decide_goal_continuation, next_blocked_audit, GoalAction,
    GoalAuditObservation, GoalDecision, GoalRequestTerminal, GoalState,
};
pub(crate) use task_fire::task_goal_fire_key;
pub use task_fire::{
    decide_task_goal_fire_recovery, task_goal_fire_identity, TaskGoalFireIdentity,
    TaskGoalFireRecoveryDecision, TaskGoalFireRecoveryDisposition, TaskGoalRequestBinding,
};
pub use tool_context::load_goal_tool_context;
pub(crate) use tool_context::load_goal_tool_context_with_usage;
pub(crate) use tool_context::same_goal_assignment;
pub use transition::{
    decide_model_goal_create, goal_continuation_materialization_step, goal_creation_fingerprint,
    goal_submission_step, GoalContinuationAction, GoalContinuationPhase, GoalCreateDisposition,
    GoalCreateRequest, GoalCreationFingerprint, GoalSubmissionAction, GoalSubmissionState,
};
#[cfg(test)]
pub(crate) use usage::session_token_usage_breakdown_in_txn;
use usage::session_token_usage_in_txn;
pub use usage::{refresh_goal_usage_from_access, session_token_usage};
pub(crate) use usage::{refresh_goal_usage_observed, GoalUsageObservation};
#[cfg(any(test, feature = "test-fixtures"))]
pub use write::seed_goal_from_access;
pub use write::{delete_goal_from_access, delete_goals_for_session_from_access};

pub const BLOCKED_AUDIT_THRESHOLD: i64 = 3;
pub const MAX_INFRASTRUCTURE_RETRIES: i64 = 2;

pub use gents_protocol::goal::GoalSnapshot;

fn mutation_returned_rows(value: &serde_json::Value) -> bool {
    value.as_array().is_some_and(|rows| !rows.is_empty()) || value.get("_docID").is_some()
}

fn optional_string_graphql_field(name: &str, value: Option<&str>) -> String {
    value
        .map(|value| format!(r#"{name}: "{}","#, escape_graphql_string(value)))
        .unwrap_or_else(|| format!("{name}: null,"))
}

pub mod goal_tools;
#[cfg(test)]
mod pushdown_runtime_tests;
#[cfg(test)]
mod pushdown_tests;
#[cfg(test)]
mod tests;
