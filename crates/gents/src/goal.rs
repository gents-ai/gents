use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use serde::{Deserialize, Serialize};

use crate::graphql::{escape_graphql_string, graphql_with_transaction_retry, rows};

mod backed_request;
mod claimed_publication;
mod client_control;
mod client_submission;
mod controller;
mod creation;
mod document;
mod edit;
mod first_request_binding;
mod history;
mod load;
mod observation;
mod operator_resume;
mod readiness_gate;
mod request_assignment;
mod request_head;
mod reset_resume;
mod state;
mod task_fire;
mod tool_context;
mod transition;
mod usage;
mod write;
pub(crate) use claimed_publication::{
    publish_claimed_continuation, stop_claimed_continuation_for_unavailable_agent,
};
pub(crate) use client_control::{prepare_goal_control, stage_goal_control};
pub use client_submission::{
    load_applied_opening_goal_id, load_applied_opening_goal_ids,
    load_applied_opening_goal_provenance, prepare_session_goal_control,
    recover_session_goal_control, submit_prepared_session_goal_control,
    submit_session_goal_control, OpeningGoalProvenance,
};
pub(crate) use edit::{edit_active_goal, GoalEdit};
pub use first_request_binding::task_assignment_goal_id;
pub(crate) use first_request_binding::{
    bind_goal_first_request_in_txn, validate_task_goal_identity_in_txn,
};
use first_request_binding::{continuation_assignment, continuation_reference_matches};
pub(crate) use history::{
    load_assignment_versions_for_session, load_goal_versions_for_session, GoalAssignmentVersion,
};
pub use observation::{
    bind_goal_input, load_session_goal_observation, load_session_goal_observation_state,
    load_session_goal_observation_state_in_txn, prepare_goal_input, prepare_goal_input_in_txn,
    SessionGoalObservationState,
};
pub(crate) use observation::{reconcile_goal_observations, SOURCES as GOAL_OBSERVATION_SOURCES};
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
pub(crate) use request_assignment::{
    latest_assignment_goals, load_request_assignment, load_request_assignment_in_txn,
    load_request_assignment_scopes, RequestAssignmentObservation, RequestAssignmentScope,
};
#[cfg(test)]
pub(crate) use request_head::assignment_allows;
pub(crate) use request_head::{
    assignment_request_id_in_txn, authenticated_goal_request_members, goal_session_is_idle,
    latest_goal_request, request_selection_fields, verify_goal_continuation_edge,
};
pub use reset_resume::resume_at_reset;
mod question_wait;
pub(crate) use backed_request::fingerprint::{
    GoalBackedRequestFingerprint, GOAL_BACKED_REQUEST_FINGERPRINT_FIELDS,
};
pub(crate) use backed_request::{
    stage_goal_backed_request_in_txn, stage_goal_backed_request_with_identity_in_txn,
};
pub use backed_request::{
    submit_goal_backed_request, submit_goal_backed_request_local, GoalBackedRequestDisposition,
};
pub(crate) use controller::fence_goal_continuation_claim_in_txn;
pub use controller::{claim_continuation, claim_retry_continuation, update_goal_fields_if_status};
use creation::stage_goal_and_claim;
pub(crate) use creation::{create_goal_for_request, load_goal_creation_claim_fingerprint_in_txn};
pub use creation::{
    create_goal_for_session, replacement_goal_identity, CreateGoalForSessionError,
    CreateGoalForSessionOutcome, GoalCreationIdentity,
};
pub use document::{
    deterministic_goal_creation_key, deterministic_goal_id, snapshot, GoalDocument, GOAL_FIELDS,
};
pub(crate) use load::sort_goals_canonical;
pub use load::{
    load_canonical_goal, load_canonical_goal_in_txn, load_goal_by_id, load_goals_for_session,
};
pub use question_wait::awaiting_goal_question_ids;
pub use state::{
    apply_operator_status_transition, decide_goal_continuation, next_blocked_audit, GoalAction,
    GoalAuditObservation, GoalDecision, GoalRequestTerminal, GoalState, GoalStatus,
};
pub(crate) use task_fire::{apply_claimed_task_goal_in_txn, task_goal_fire_key};
pub use task_fire::{
    decide_task_goal_fire_recovery, task_goal_fire_identity, validate_task_goal_declaration,
    TaskGoalFireIdentity, TaskGoalFireRecoveryDecision, TaskGoalFireRecoveryDisposition,
    TaskGoalRequestBinding,
};
pub(crate) use tool_context::load_goal_tool_context;
use tool_context::same_goal_assignment;
pub use transition::{
    decide_model_goal_create, goal_continuation_materialization_step, goal_creation_fingerprint,
    goal_submission_step, GoalContinuationAction, GoalContinuationPhase, GoalCreateDisposition,
    GoalCreateRequest, GoalCreationFingerprint, GoalSubmissionAction, GoalSubmissionState,
};
#[cfg(test)]
pub(crate) use usage::session_token_usage_breakdown_in_txn;
use usage::session_token_usage_in_txn;
pub use usage::{refresh_goal_usage, session_token_usage, SessionTokenUsage};
pub(crate) use write::pause_goal_for_interrupt_in_txn;
use write::set_goal_in_txn;
pub use write::{
    delete_goal, delete_goals_for_session, set_goal, set_goal_auto_resume_from_access,
    set_goal_from_access,
};

/// Resume only a paused Goal; an interrupted/claimed execution must settle
/// before a fresh continuation can be staged.
pub(crate) async fn stage_session_resume_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    identity: &dyn crate::store::identity::NodeIdentity,
    node_did: &str,
    session_id: &str,
) -> Result<()> {
    let requests = request_head::load_session_requests(txn, node_did, session_id).await?;
    anyhow::ensure!(
        !requests
            .iter()
            .any(|row| row.lifecycle_state.is_some_and(|state| matches!(
                state,
                RequestLifecycleState::Claimed | RequestLifecycleState::Processing
            ))),
        "session work is still settling; retry resume after the interrupted request terminates"
    );
    let Some(goal) = load_canonical_goal_in_txn(txn, node_did, session_id).await? else {
        return Ok(());
    };
    let state = goal.state().context("Goal has an unknown status")?;
    if state.status != GoalStatus::Paused {
        return Ok(());
    }
    let prepared = client_control::prepare_goal_resume(goal, state, &requests)
        .map_err(|error| anyhow::anyhow!("{error}; resume was not applied"))?;
    client_control::stage_goal_control(txn, identity, prepared).await?;
    Ok(())
}

pub const GOAL_TRIGGER_KIND: &str = "goal";
pub const GET_GOAL_TOOL_NAME: &str = "get_goal";
pub const UPDATE_GOAL_TOOL_NAME: &str = "update_goal";
pub const CREATE_GOAL_TOOL_NAME: &str = "create_goal";
pub const BLOCKED_AUDIT_THRESHOLD: i64 = 3;
pub const MAX_INFRASTRUCTURE_RETRIES: i64 = 2;

pub use gents_protocol::goal::GoalSnapshot;

fn mutation_returned_rows(value: &serde_json::Value) -> bool {
    value.as_array().is_some_and(|rows| !rows.is_empty()) || value.get("_docID").is_some()
}

fn mutation_row_count(value: &serde_json::Value) -> usize {
    value
        .as_array()
        .map_or_else(|| usize::from(value.get("_docID").is_some()), Vec::len)
}

fn optional_int_graphql_field(name: &str, value: Option<i64>) -> String {
    value
        .map(|value| format!("{name}: {value},"))
        .unwrap_or_else(|| format!("{name}: null,"))
}

fn optional_string_graphql_field(name: &str, value: Option<&str>) -> String {
    value
        .map(|value| format!(r#"{name}: "{}","#, escape_graphql_string(value)))
        .unwrap_or_else(|| format!("{name}: null,"))
}

#[cfg(test)]
mod tests;
