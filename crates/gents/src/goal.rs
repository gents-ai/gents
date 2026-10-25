use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use serde::{Deserialize, Serialize};

use crate::graphql::{escape_graphql_string, graphql_with_transaction_retry, rows};

mod claimed_publication;
mod client_control;
mod client_submission;
mod edit;
mod first_request_binding;
mod history;
mod observation;
mod operator_resume;
mod readiness_gate;
mod request_assignment;
mod request_head;
mod reset_resume;
mod tool_context;
pub(crate) use claimed_publication::{
    publish_claimed_continuation, stop_claimed_continuation_for_unavailable_agent,
};
pub(crate) use client_control::{prepare_goal_control, stage_goal_control};
pub use client_submission::{
    load_applied_opening_goal_id, load_applied_opening_goal_ids,
    load_applied_opening_goal_provenance, submit_session_goal_control, OpeningGoalProvenance,
};
pub(crate) use edit::{edit_active_goal, GoalEdit};
pub use first_request_binding::task_assignment_goal_id;
pub(crate) use first_request_binding::{
    bind_goal_first_request_in_txn, validate_task_goal_identity_in_txn,
};
use first_request_binding::{continuation_assignment, continuation_reference_matches};
pub(crate) use history::{load_goal_versions_for_session, GoalVersion};
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
    load_request_assignments_in_txn, RequestAssignmentObservation, RequestAssignmentScope,
};
#[cfg(test)]
pub(crate) use request_head::assignment_allows;
pub(crate) use request_head::{
    assignment_request_id_in_txn, authenticated_goal_request_members, goal_session_is_idle,
    latest_authenticated_session_request, latest_goal_request, verify_goal_continuation_edge,
};
pub use reset_resume::resume_at_reset;
pub(crate) use tool_context::load_goal_tool_context;
use tool_context::same_goal_assignment;

/// Resume only a paused Goal; an interrupted/claimed execution must settle
/// before a fresh continuation can be staged.
pub(crate) async fn stage_session_resume_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    identity: &dyn crate::identity::NodeIdentity,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalCreationFingerprint {
    pub owner: String,
    pub session: String,
    pub objective: String,
    pub token_budget: Option<i128>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalCreateRequest {
    pub caller: String,
    pub current_session: String,
    pub requested_owner: String,
    pub requested_session: String,
    pub objective: String,
    pub objective_nonempty: bool,
    pub token_budget: Option<i128>,
    pub goal_tools: bool,
    pub goal_create: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalCreateDisposition {
    Denied,
    Invalid,
    Fresh,
    Idempotent,
    Conflict,
}

pub fn goal_creation_fingerprint(request: &GoalCreateRequest) -> GoalCreationFingerprint {
    GoalCreationFingerprint {
        owner: request.caller.clone(),
        session: request.current_session.clone(),
        objective: request.objective.trim().to_string(),
        token_budget: request.token_budget,
    }
}

pub fn decide_model_goal_create(
    request: &GoalCreateRequest,
    existing: Option<&GoalCreationFingerprint>,
) -> GoalCreateDisposition {
    if !request.goal_tools
        || !request.goal_create
        || request.requested_owner != request.caller
        || request.requested_session != request.current_session
    {
        return GoalCreateDisposition::Denied;
    }
    if !request.objective_nonempty
        || request.objective.trim().is_empty()
        || request
            .token_budget
            .is_some_and(|budget| budget <= 0 || budget > i64::MAX as i128)
    {
        return GoalCreateDisposition::Invalid;
    }
    match existing {
        None => GoalCreateDisposition::Fresh,
        Some(existing) if *existing == goal_creation_fingerprint(request) => {
            GoalCreateDisposition::Idempotent
        }
        Some(_) => GoalCreateDisposition::Conflict,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GoalSubmissionState {
    pub durable_goal: bool,
    pub runnable_request: bool,
    pub staged_goal: bool,
    pub staged_request: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalSubmissionAction {
    StageGoal,
    StageRequest,
    Commit,
    Abort,
    Crash,
}

pub fn goal_submission_step(
    state: GoalSubmissionState,
    action: GoalSubmissionAction,
) -> GoalSubmissionState {
    match action {
        GoalSubmissionAction::StageGoal => GoalSubmissionState {
            staged_goal: true,
            ..state
        },
        GoalSubmissionAction::StageRequest if state.staged_goal || state.durable_goal => {
            GoalSubmissionState {
                staged_request: true,
                ..state
            }
        }
        GoalSubmissionAction::Commit if state.staged_goal && state.staged_request => {
            GoalSubmissionState {
                durable_goal: true,
                runnable_request: true,
                staged_goal: false,
                staged_request: false,
            }
        }
        GoalSubmissionAction::Abort | GoalSubmissionAction::Crash => GoalSubmissionState {
            staged_goal: false,
            staged_request: false,
            ..state
        },
        _ => state,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalContinuationPhase {
    Unclaimed,
    Claimed,
    ChildPresent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalContinuationAction {
    Claim(bool),
    Materialize,
    Reconcile,
    Crash,
}

pub fn goal_continuation_materialization_step(
    phase: GoalContinuationPhase,
    action: GoalContinuationAction,
) -> GoalContinuationPhase {
    match (phase, action) {
        (GoalContinuationPhase::Unclaimed, GoalContinuationAction::Claim(true)) => {
            GoalContinuationPhase::Claimed
        }
        (GoalContinuationPhase::Claimed, GoalContinuationAction::Materialize)
        | (GoalContinuationPhase::Claimed, GoalContinuationAction::Reconcile) => {
            GoalContinuationPhase::ChildPresent
        }
        (phase, _) => phase,
    }
}

pub const GOAL_FIELDS: &str = r#"
    _docID
    goal_id
    creation_key
    session_id
    node_did
    objective
    status
    token_budget
    tokens_used
    token_usage_baseline
    assignment_root_request_doc_id
    active_time_seconds
    active_started_at
    consecutive_blocked_audits
    last_blocked_request_id
    last_blocked_reason
    last_continued_from_request_id
    continuation_sequence
    wrapup_requested
    wrapup_completed
    infrastructure_retry_count
    last_failure
    completion_evidence
    auto_resume_at_reset
    created_at
    updated_at
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    Active,
    Paused,
    Blocked,
    UsageLimited,
    BudgetLimited,
    Complete,
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalRequestTerminal {
    Completed,
    Failed,
    Dead,
    Interrupted,
    Superseded,
}

impl GoalRequestTerminal {
    pub fn parse(value: &str) -> Option<Self> {
        Self::from_state(RequestLifecycleState::parse(value).ok()?)
    }

    pub fn from_state(state: RequestLifecycleState) -> Option<Self> {
        match state {
            RequestLifecycleState::Completed => Some(Self::Completed),
            RequestLifecycleState::Failed => Some(Self::Failed),
            RequestLifecycleState::Dead => Some(Self::Dead),
            RequestLifecycleState::Interrupted => Some(Self::Interrupted),
            RequestLifecycleState::Superseded => Some(Self::Superseded),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalDecision {
    None,
    Continue,
    Retry,
    Pause,
    Wrapup,
    AbandonWrapup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalAuditObservation {
    SameRequest,
    SameCondition,
    NewCondition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalAction {
    Pause,
    Resume,
    Stop,
    Complete,
    BlockedAudit(GoalAuditObservation),
    OperatorBlock,
    UsageLimit,
    BudgetExhausted,
    WrapupFinished,
    WrapupAbandoned,
    CleanTurn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GoalState {
    pub status: GoalStatus,
    pub blocked_audits: i64,
    pub wrapup_requested: bool,
    pub wrapup_completed: bool,
}

impl GoalState {
    pub(crate) fn has_continuation_obligation(self) -> bool {
        self.status == GoalStatus::Active
            || (self.status == GoalStatus::BudgetLimited
                && self.wrapup_requested
                && !self.wrapup_completed)
    }

    pub fn step(self, action: GoalAction) -> Option<Self> {
        match action {
            GoalAction::Pause
                if matches!(self.status, GoalStatus::Active | GoalStatus::BudgetLimited) =>
            {
                Some(Self {
                    status: GoalStatus::Paused,
                    ..self
                })
            }
            GoalAction::Resume
                if matches!(
                    self.status,
                    GoalStatus::Paused | GoalStatus::Blocked | GoalStatus::UsageLimited
                ) =>
            {
                Some(Self {
                    status: if self.wrapup_requested && !self.wrapup_completed {
                        GoalStatus::BudgetLimited
                    } else {
                        GoalStatus::Active
                    },
                    blocked_audits: 0,
                    ..self
                })
            }
            GoalAction::Complete
                if !matches!(self.status, GoalStatus::Complete | GoalStatus::Stopped) =>
            {
                Some(Self {
                    status: GoalStatus::Complete,
                    wrapup_completed: true,
                    ..self
                })
            }
            GoalAction::Stop
                if !matches!(self.status, GoalStatus::Complete | GoalStatus::Stopped) =>
            {
                Some(Self {
                    status: GoalStatus::Stopped,
                    ..self
                })
            }
            GoalAction::BlockedAudit(observation) if self.status == GoalStatus::Active => {
                let blocked_audits = match observation {
                    GoalAuditObservation::SameRequest => self.blocked_audits.max(0),
                    GoalAuditObservation::SameCondition => {
                        self.blocked_audits.max(0).saturating_add(1)
                    }
                    GoalAuditObservation::NewCondition => 1,
                };
                Some(Self {
                    status: if blocked_audits >= BLOCKED_AUDIT_THRESHOLD {
                        GoalStatus::Blocked
                    } else {
                        GoalStatus::Active
                    },
                    blocked_audits,
                    ..self
                })
            }
            GoalAction::OperatorBlock if self.status == GoalStatus::Active => Some(Self {
                status: GoalStatus::Blocked,
                ..self
            }),
            GoalAction::UsageLimit if self.status == GoalStatus::Active => Some(Self {
                status: GoalStatus::UsageLimited,
                ..self
            }),
            GoalAction::BudgetExhausted
                if self.status == GoalStatus::Active && !self.wrapup_requested =>
            {
                Some(Self {
                    status: GoalStatus::BudgetLimited,
                    wrapup_requested: true,
                    ..self
                })
            }
            GoalAction::WrapupFinished | GoalAction::WrapupAbandoned
                if self.status == GoalStatus::BudgetLimited
                    && self.wrapup_requested
                    && !self.wrapup_completed =>
            {
                Some(Self {
                    wrapup_completed: true,
                    ..self
                })
            }
            GoalAction::CleanTurn if self.status == GoalStatus::Active => Some(Self {
                blocked_audits: 0,
                ..self
            }),
            _ => None,
        }
    }
}

pub fn next_blocked_audit(
    previous_count: i64,
    previous_reason: Option<&str>,
    previous_request_id: Option<&str>,
    reason: &str,
    request_id: &str,
) -> (i64, bool) {
    let observation = if previous_request_id == Some(request_id) {
        GoalAuditObservation::SameRequest
    } else if previous_reason == Some(reason) {
        GoalAuditObservation::SameCondition
    } else {
        GoalAuditObservation::NewCondition
    };
    let state = GoalState {
        status: GoalStatus::Active,
        blocked_audits: previous_count,
        wrapup_requested: false,
        wrapup_completed: false,
    }
    .step(GoalAction::BlockedAudit(observation))
    .expect("blocked audit is legal from active");
    (state.blocked_audits, state.status == GoalStatus::Blocked)
}

#[allow(clippy::too_many_arguments)]
pub fn decide_goal_continuation(
    status: GoalStatus,
    terminal: GoalRequestTerminal,
    session_idle: bool,
    child_exists: bool,
    budget_reached: bool,
    has_activity: bool,
    request_is_wrapup: bool,
    infrastructure_retries: i64,
    wrapup_requested: bool,
    wrapup_completed: bool,
) -> GoalDecision {
    if !session_idle || child_exists {
        return GoalDecision::None;
    }
    match status {
        GoalStatus::Active => match terminal {
            GoalRequestTerminal::Interrupted | GoalRequestTerminal::Superseded => {
                GoalDecision::Pause
            }
            GoalRequestTerminal::Failed | GoalRequestTerminal::Dead => {
                if budget_reached {
                    GoalDecision::Wrapup
                } else if infrastructure_retries.max(0) < MAX_INFRASTRUCTURE_RETRIES {
                    GoalDecision::Retry
                } else {
                    GoalDecision::Pause
                }
            }
            GoalRequestTerminal::Completed => {
                if !has_activity {
                    GoalDecision::Pause
                } else if budget_reached {
                    GoalDecision::Wrapup
                } else {
                    GoalDecision::Continue
                }
            }
        },
        GoalStatus::BudgetLimited => {
            if !wrapup_requested || wrapup_completed {
                GoalDecision::None
            } else if !request_is_wrapup {
                GoalDecision::Wrapup
            } else {
                match terminal {
                    GoalRequestTerminal::Completed => GoalDecision::None,
                    GoalRequestTerminal::Interrupted | GoalRequestTerminal::Superseded => {
                        GoalDecision::Pause
                    }
                    GoalRequestTerminal::Failed | GoalRequestTerminal::Dead => {
                        if infrastructure_retries.max(0) < MAX_INFRASTRUCTURE_RETRIES {
                            GoalDecision::Retry
                        } else {
                            GoalDecision::AbandonWrapup
                        }
                    }
                }
            }
        }
        GoalStatus::Paused
        | GoalStatus::Blocked
        | GoalStatus::UsageLimited
        | GoalStatus::Complete
        | GoalStatus::Stopped => GoalDecision::None,
    }
}

impl GoalStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Blocked => "blocked",
            Self::UsageLimited => "usage_limited",
            Self::BudgetLimited => "budget_limited",
            Self::Complete => "complete",
            Self::Stopped => "stopped",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "active" => Some(Self::Active),
            "paused" => Some(Self::Paused),
            "blocked" => Some(Self::Blocked),
            "usage_limited" | "usageLimited" => Some(Self::UsageLimited),
            "budget_limited" | "budgetLimited" => Some(Self::BudgetLimited),
            "complete" => Some(Self::Complete),
            "stopped" => Some(Self::Stopped),
            _ => None,
        }
    }

    pub fn accrues_active_time(self) -> bool {
        self == Self::Active
    }
}

fn operator_action_for_status(current: GoalStatus, target: GoalStatus) -> Option<GoalAction> {
    if current == target {
        return None;
    }
    Some(match target {
        GoalStatus::Active => GoalAction::Resume,
        GoalStatus::Paused => GoalAction::Pause,
        GoalStatus::Blocked => GoalAction::OperatorBlock,
        GoalStatus::UsageLimited => GoalAction::UsageLimit,
        GoalStatus::BudgetLimited => GoalAction::BudgetExhausted,
        GoalStatus::Complete => GoalAction::Complete,
        GoalStatus::Stopped => GoalAction::Stop,
    })
}

pub fn apply_operator_status_transition(state: GoalState, target: GoalStatus) -> Result<GoalState> {
    let Some(action) = operator_action_for_status(state.status, target) else {
        return Ok(state);
    };
    state.step(action).with_context(|| {
        format!(
            "illegal durable Goal transition {} -> {}",
            state.status.as_str(),
            target.as_str()
        )
    })
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GoalDocument {
    #[serde(rename = "_docID")]
    pub doc_id: String,
    pub goal_id: String,
    pub session_id: String,
    pub node_did: String,
    #[serde(default)]
    pub objective: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub token_budget: Option<i64>,
    #[serde(default)]
    pub assignment_root_request_doc_id: Option<String>,
    #[serde(default)]
    pub tokens_used: Option<i64>,
    #[serde(default)]
    pub token_usage_baseline: Option<i64>,
    #[serde(default)]
    pub active_time_seconds: Option<i64>,
    #[serde(default)]
    pub active_started_at: Option<String>,
    #[serde(default)]
    pub consecutive_blocked_audits: Option<i64>,
    #[serde(default)]
    pub last_blocked_request_id: Option<String>,
    #[serde(default)]
    pub last_blocked_reason: Option<String>,
    #[serde(default)]
    pub last_continued_from_request_id: Option<String>,
    #[serde(default)]
    pub continuation_sequence: Option<i64>,
    #[serde(default)]
    pub wrapup_requested: Option<bool>,
    #[serde(default)]
    pub wrapup_completed: Option<bool>,
    #[serde(default)]
    pub infrastructure_retry_count: Option<i64>,
    #[serde(default)]
    pub last_failure: Option<String>,
    #[serde(default)]
    pub completion_evidence: Option<String>,
    #[serde(default)]
    pub auto_resume_at_reset: Option<bool>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

impl GoalDocument {
    pub fn parsed_status(&self) -> Option<GoalStatus> {
        GoalStatus::parse(&self.status)
    }

    pub fn state(&self) -> Option<GoalState> {
        Some(GoalState {
            status: self.parsed_status()?,
            blocked_audits: self.consecutive_blocked_audits.unwrap_or_default().max(0),
            wrapup_requested: self.wrapup_requested.unwrap_or(false),
            wrapup_completed: self.wrapup_completed.unwrap_or(false),
        })
    }

    pub fn current_active_time_seconds(&self, now: DateTime<Utc>) -> i64 {
        let persisted = self.active_time_seconds.unwrap_or_default().max(0);
        if self.parsed_status() != Some(GoalStatus::Active) {
            return persisted;
        }
        let elapsed = self
            .active_started_at
            .as_deref()
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            .map(|started| (now - started.with_timezone(&Utc)).num_seconds().max(0))
            .unwrap_or_default();
        persisted.saturating_add(elapsed)
    }

    pub fn continuation_sequence(&self) -> i64 {
        self.continuation_sequence.unwrap_or_default().max(0)
    }
}

pub use gents_protocol::goal::GoalSnapshot;

pub fn snapshot(goal: &GoalDocument, now: DateTime<Utc>) -> GoalSnapshot {
    GoalSnapshot {
        goal_id: goal.goal_id.clone(),
        session_id: goal.session_id.clone(),
        node_did: goal.node_did.clone(),
        objective: goal.objective.clone(),
        status: goal.status.clone(),
        token_budget: goal.token_budget,
        tokens_used: goal.tokens_used.unwrap_or_default().max(0),
        active_time_seconds: goal.current_active_time_seconds(now),
        consecutive_blocked_audits: goal.consecutive_blocked_audits.unwrap_or_default().max(0),
        continuation_sequence: goal.continuation_sequence(),
        wrapup_requested: goal.wrapup_requested.unwrap_or(false),
        wrapup_completed: goal.wrapup_completed.unwrap_or(false),
        last_blocked_reason: goal.last_blocked_reason.clone(),
        last_failure: goal.last_failure.clone(),
        completion_evidence: goal.completion_evidence.clone(),
        created_at: goal.created_at.clone(),
        updated_at: goal.updated_at.clone(),
    }
}

pub fn deterministic_goal_id(node_did: &str, session_id: &str) -> String {
    format!("{}:{node_did}:{session_id}", node_did.len())
}

pub fn deterministic_goal_creation_key(node_did: &str, session_id: &str) -> String {
    format!("goal-create:{}:{node_did}:{session_id}", node_did.len())
}

#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskGoalFireIdentity {
    pub session_id: String,
    pub request_id: String,
    pub retry_key: String,
}

#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TaskGoalRequestBinding {
    #[serde(default)]
    pub node_did: String,
    #[serde(default)]
    pub agent_id: String,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub request_id: String,
    #[serde(default)]
    pub retry_key: String,
}

#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskGoalFireRecoveryDisposition {
    Absent,
    Recovered,
    Conflict,
}

#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskGoalFireRecoveryDecision {
    pub disposition: TaskGoalFireRecoveryDisposition,
    pub recovered_request_id: Option<String>,
    pub checkpointable: bool,
}

#[doc(hidden)]
pub fn decide_task_goal_fire_recovery(
    expected: &TaskGoalRequestBinding,
    observed: Option<&TaskGoalRequestBinding>,
) -> TaskGoalFireRecoveryDecision {
    match observed {
        None => TaskGoalFireRecoveryDecision {
            disposition: TaskGoalFireRecoveryDisposition::Absent,
            recovered_request_id: None,
            checkpointable: false,
        },
        Some(observed) if observed == expected => TaskGoalFireRecoveryDecision {
            disposition: TaskGoalFireRecoveryDisposition::Recovered,
            recovered_request_id: Some(observed.request_id.clone()),
            checkpointable: true,
        },
        Some(_) => TaskGoalFireRecoveryDecision {
            disposition: TaskGoalFireRecoveryDisposition::Conflict,
            recovered_request_id: None,
            checkpointable: false,
        },
    }
}

/// Length-prefixing matches `GoalAutomation.taskFireIdentity`.
#[doc(hidden)]
pub fn task_goal_fire_identity(
    node_did: &str,
    task_id: &str,
    fire_key: &str,
) -> TaskGoalFireIdentity {
    let scope = format!(
        "{}:{node_did}:{}:{task_id}:{}:{fire_key}",
        node_did.chars().count(),
        task_id.chars().count(),
        fire_key.chars().count()
    );
    TaskGoalFireIdentity {
        session_id: format!("task-goal-session:{scope}"),
        request_id: format!("task-goal-request:{scope}"),
        retry_key: format!("task-goal-retry:{scope}"),
    }
}

pub(crate) fn task_goal_fire_key<'a>(request_id: &'a str, expected_owner: &str) -> Option<&'a str> {
    fn component(input: &str) -> Option<(&str, &str)> {
        let (length, rest) = input.split_once(':')?;
        let length = length.parse::<usize>().ok()?;
        let end = rest
            .char_indices()
            .map(|(index, _)| index)
            .chain(std::iter::once(rest.len()))
            .nth(length)?;
        Some((&rest[..end], &rest[end..]))
    }
    let scope = request_id.strip_prefix("task-goal-request:")?;
    let (owner, tail) = component(scope)?;
    let (task, tail) = component(tail.strip_prefix(':')?)?;
    let (fire, tail) = component(tail.strip_prefix(':')?)?;
    if owner != expected_owner
        || !tail.is_empty()
        || task_goal_fire_identity(owner, task, fire).request_id != request_id
    {
        return None;
    }
    Some(fire)
}

#[doc(hidden)]
pub fn validate_task_goal_declaration(
    objective: Option<&str>,
    token_budget: Option<i64>,
) -> Result<()> {
    match objective {
        None => anyhow::ensure!(
            token_budget.is_none(),
            "goal token budget requires a goal objective template"
        ),
        Some(objective) => {
            anyhow::ensure!(
                !objective.trim().is_empty(),
                "goal objective must be non-empty"
            );
            anyhow::ensure!(
                token_budget.is_none_or(|budget| budget > 0),
                "goal token budget must be positive"
            );
        }
    }
    Ok(())
}

pub(crate) async fn load_goal_creation_claim_fingerprint(
    node: &EmbeddedNode,
    node_did: &str,
    session_id: &str,
) -> Result<Option<GoalCreationFingerprint>> {
    let query = goal_creation_claim_query(node_did, session_id);
    let response = graphql_with_transaction_retry(node, &query, "load goal creation claim").await?;
    goal_creation_claim_fingerprint(&response)
}

pub(crate) async fn load_goal_creation_claim_fingerprint_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
) -> Result<Option<GoalCreationFingerprint>> {
    let response = txn
        .execute_local_response(&goal_creation_claim_query(node_did, session_id))
        .await?;
    goal_creation_claim_fingerprint(&response)
}

fn goal_creation_claim_query(node_did: &str, session_id: &str) -> String {
    let key = escape_graphql_string(&deterministic_goal_creation_key(node_did, session_id));
    format!(
        r#"{{ GoalCreationClaim(filter: {{ creation_key: {{ _eq: "{key}" }} }}, limit: 2) {{
            node_did session_id objective token_budget
        }} }}"#
    )
}

fn goal_creation_claim_fingerprint(
    response: &defra_node::QueryResponse,
) -> Result<Option<GoalCreationFingerprint>> {
    #[derive(Deserialize)]
    struct ClaimRow {
        node_did: String,
        session_id: String,
        objective: String,
        token_budget: Option<i64>,
    }
    let claims: Vec<ClaimRow> = rows(response, "GoalCreationClaim")?;
    anyhow::ensure!(
        claims.len() <= 1,
        "goal creation key resolved to multiple claims"
    );
    Ok(claims
        .into_iter()
        .next()
        .map(|claim| GoalCreationFingerprint {
            owner: claim.node_did,
            session: claim.session_id,
            objective: claim.objective,
            token_budget: claim.token_budget.map(i128::from),
        }))
}

#[derive(Debug, Clone)]
pub enum CreateGoalForSessionOutcome {
    Created(GoalDocument),
    Idempotent(GoalDocument),
}

impl CreateGoalForSessionOutcome {
    pub fn goal(&self) -> &GoalDocument {
        match self {
            Self::Created(goal) | Self::Idempotent(goal) => goal,
        }
    }

    pub fn disposition(&self) -> &'static str {
        match self {
            Self::Created(_) => "created",
            Self::Idempotent(_) => "idempotent",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CreateGoalForSessionError {
    #[error("goal objective must be non-empty")]
    InvalidObjective,
    #[error("goal token budget must be positive")]
    InvalidBudget,
    #[error("the current session already has a goal with a different objective or budget")]
    Conflict,
    #[error(transparent)]
    Storage(#[from] anyhow::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StagedGoalDisposition {
    Created,
    Idempotent,
}

#[derive(Debug, Clone)]
struct StagedGoal {
    disposition: StagedGoalDisposition,
    goal: GoalDocument,
}

async fn fence_opened_graph_goal_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    previous: Option<GoalState>,
    goal: &GoalDocument,
) -> Result<()> {
    let current = goal
        .state()
        .context("graph Goal opening has unknown status")?;
    if !current.has_continuation_obligation()
        || previous.is_some_and(GoalState::has_continuation_obligation)
    {
        return Ok(());
    }
    let did = escape_graphql_string(&goal.node_did);
    let session = escape_graphql_string(&goal.session_id);
    let fields = crate::request_admission::SIGNED_REQUEST_FIELDS;
    let response = txn
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{
        node_did: {{ _eq: "{did}" }}, session_id: {{ _eq: "{session}" }}
    }}, order: [{{ created_at: DESC }}, {{ request_id: DESC }}]) {{ {fields} }} }}"#
        ))
        .await?;
    let requests: Vec<gents_protocol::row::AgentRequestRow> = serde_json::from_value(
        response
            .pointer("/data/AgentRequest")
            .cloned()
            .context("Goal opening request query omitted rows")?,
    )?;
    let Some(head) = latest_goal_request(goal, &requests) else {
        return Ok(());
    };
    if head.caused_by_trigger_kind.as_deref() == Some(GOAL_TRIGGER_KIND)
        && head.caused_by_trigger_id.as_deref() != Some(goal.goal_id.as_str())
    {
        return Ok(());
    }
    let head_doc = head
        .doc_id
        .as_deref()
        .context("Goal opening head has no physical document ID")?;
    if let Some(binding) =
        crate::graph_pipeline::graph_binding_for_request_in_txn(txn, head_doc).await?
    {
        crate::graph_pipeline::fence_graph_publication_in_txn(
            txn,
            &binding.run_id,
            &binding.revision_digest,
        )
        .await?;
    }
    Ok(())
}

#[doc(hidden)]
pub struct GoalCreationIdentity {
    pub goal_id: String,
    pub creation_key: String,
}

/// IDs may contain separators; each component is framed by its UTF-8 byte
/// length so the client and transaction owner name the same fresh assignment.
#[doc(hidden)]
pub fn replacement_goal_identity(
    node_did: &str,
    session_id: &str,
    command_id: &str,
) -> GoalCreationIdentity {
    let scope = format!(
        "{}:{node_did}:{}:{session_id}:{}:{command_id}",
        node_did.len(),
        session_id.len(),
        command_id.len()
    );
    GoalCreationIdentity {
        goal_id: format!("goal-assignment:{scope}"),
        creation_key: format!("goal-assignment-create:{scope}"),
    }
}

#[allow(clippy::too_many_arguments)]
async fn stage_goal_and_claim(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
    objective: &str,
    token_budget: Option<i64>,
    initial_state: GoalState,
    now: &str,
    identity: Option<&GoalCreationIdentity>,
) -> Result<StagedGoal> {
    let create_request = GoalCreateRequest {
        caller: node_did.to_string(),
        current_session: session_id.to_string(),
        requested_owner: node_did.to_string(),
        requested_session: session_id.to_string(),
        objective: objective.to_string(),
        objective_nonempty: !objective.is_empty(),
        token_budget: token_budget.map(i128::from),
        goal_tools: true,
        goal_create: true,
    };
    anyhow::ensure!(
        decide_model_goal_create(&create_request, None) == GoalCreateDisposition::Fresh,
        "invalid goal objective or token budget"
    );
    let fingerprint = goal_creation_fingerprint(&create_request);
    let objective = fingerprint.objective.as_str();
    let goal_id = identity
        .map(|identity| identity.goal_id.clone())
        .unwrap_or_else(|| deterministic_goal_id(node_did, session_id));
    let creation_key = identity
        .map(|identity| identity.creation_key.clone())
        .unwrap_or_else(|| deterministic_goal_creation_key(node_did, session_id));
    let escaped_did = escape_graphql_string(node_did);
    let escaped_session = escape_graphql_string(session_id);
    let response = txn
        .execute(&format!(
            r#"{{
                Goal(filter: {{
                    node_did: {{ _eq: "{escaped_did}" }},
                    session_id: {{ _eq: "{escaped_session}" }}
                }}) {{ {GOAL_FIELDS} }}
                GoalCreationClaim(filter: {{ creation_key: {{ _eq: "{}" }} }}) {{
                    goal_id node_did session_id objective token_budget
                }}
            }}"#,
            escape_graphql_string(&creation_key),
        ))
        .await?;
    let mut goals: Vec<GoalDocument> = serde_json::from_value(
        response
            .pointer("/data/Goal")
            .cloned()
            .unwrap_or_else(|| serde_json::Value::Array(Vec::new())),
    )
    .context("decoding transactional Goal rows")?;
    sort_goals_canonical(&mut goals);
    let claims = response
        .pointer("/data/GoalCreationClaim")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    anyhow::ensure!(
        claims.len() <= 1,
        "goal creation key resolved to multiple claims"
    );
    if let Some(claim) = claims.first() {
        let claim_fingerprint = GoalCreationFingerprint {
            owner: claim
                .get("node_did")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
            session: claim
                .get("session_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
            objective: claim
                .get("objective")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
            token_budget: claim
                .get("token_budget")
                .and_then(serde_json::Value::as_i64)
                .map(i128::from),
        };
        if claim.get("goal_id").and_then(serde_json::Value::as_str) != Some(goal_id.as_str())
            || decide_model_goal_create(&create_request, Some(&claim_fingerprint))
                != GoalCreateDisposition::Idempotent
        {
            return Err(anyhow::Error::new(CreateGoalForSessionError::Conflict));
        }
    } else {
        let budget_field = optional_int_graphql_field("token_budget", token_budget);
        txn.execute(&format!(
            r#"mutation {{ create_GoalCreationClaim(input: {{
                creation_key: "{}", goal_id: "{}",
                node_did: "{escaped_did}", session_id: "{escaped_session}",
                objective: "{}", {budget_field} created_at: "{}"
            }}) {{ _docID }} }}"#,
            escape_graphql_string(&creation_key),
            escape_graphql_string(&goal_id),
            escape_graphql_string(objective),
            escape_graphql_string(now),
        ))
        .await?;
    }

    if let Some(goal) = goals.first().cloned() {
        if !goals.iter().all(|candidate| {
            candidate.objective.trim() == objective && candidate.token_budget == token_budget
        }) {
            return Err(anyhow::Error::new(CreateGoalForSessionError::Conflict));
        }
        return Ok(StagedGoal {
            disposition: StagedGoalDisposition::Idempotent,
            goal,
        });
    }

    let status = initial_state.status;
    let baseline = session_token_usage_in_txn(txn, node_did, session_id).await?;
    let active_started_field = optional_string_graphql_field(
        "active_started_at",
        status.accrues_active_time().then_some(now),
    );
    let created = txn.execute(&format!(
        r#"mutation {{ create_Goal(input: {{
            goal_id: "{}", creation_key: "{}",
            session_id: "{escaped_session}", node_did: "{escaped_did}",
            objective: "{}", status: "{}", {}
            tokens_used: 0, token_usage_baseline: {baseline}, active_time_seconds: 0, {active_started_field}
            consecutive_blocked_audits: {}, continuation_sequence: 0,
            wrapup_requested: {}, wrapup_completed: {},
            infrastructure_retry_count: 0, created_at: "{}", updated_at: "{}"
        }}) {{ _docID }} }}"#,
        escape_graphql_string(&goal_id),
        escape_graphql_string(&creation_key),
        escape_graphql_string(objective),
        status.as_str(),
        optional_int_graphql_field("token_budget", token_budget),
        initial_state.blocked_audits,
        initial_state.wrapup_requested,
        initial_state.wrapup_completed,
        escape_graphql_string(now),
        escape_graphql_string(now),
    ))
    .await?;
    let doc_id = crate::graphql::created_doc_id(&created, "Goal")?;
    let staged = StagedGoal {
        disposition: StagedGoalDisposition::Created,
        goal: GoalDocument {
            doc_id,
            goal_id,
            session_id: session_id.to_string(),
            node_did: node_did.to_string(),
            objective: objective.to_string(),
            status: status.as_str().to_string(),
            token_budget,
            tokens_used: Some(0),
            token_usage_baseline: Some(baseline),
            assignment_root_request_doc_id: None,
            active_time_seconds: Some(0),
            active_started_at: status.accrues_active_time().then(|| now.to_string()),
            consecutive_blocked_audits: Some(initial_state.blocked_audits),
            last_blocked_request_id: None,
            last_blocked_reason: None,
            last_continued_from_request_id: None,
            continuation_sequence: Some(0),
            wrapup_requested: Some(initial_state.wrapup_requested),
            wrapup_completed: Some(initial_state.wrapup_completed),
            infrastructure_retry_count: Some(0),
            last_failure: None,
            completion_evidence: None,
            auto_resume_at_reset: None,
            created_at: Some(now.to_string()),
            updated_at: Some(now.to_string()),
        },
    };
    fence_opened_graph_goal_in_txn(txn, None, &staged.goal).await?;
    Ok(staged)
}

/// Ownership is deliberately not accepted as model input: both values come
/// from the authenticated session hook.
pub async fn create_goal_for_session(
    node: &std::sync::Arc<EmbeddedNode>,
    node_did: &str,
    session_id: &str,
    objective: &str,
    token_budget: Option<i64>,
) -> std::result::Result<CreateGoalForSessionOutcome, CreateGoalForSessionError> {
    create_goal_for_session_inner(node, node_did, session_id, objective, token_budget, None).await
}

pub(crate) async fn create_goal_for_request(
    node: &std::sync::Arc<EmbeddedNode>,
    caller: &gents_protocol::row::AgentRequestRow,
    objective: &str,
    token_budget: Option<i64>,
) -> std::result::Result<CreateGoalForSessionOutcome, CreateGoalForSessionError> {
    let owner = caller.node_did.as_deref().ok_or_else(|| {
        CreateGoalForSessionError::Storage(anyhow::anyhow!("Goal tool caller has no owner"))
    })?;
    let session = caller.session_id.as_deref().ok_or_else(|| {
        CreateGoalForSessionError::Storage(anyhow::anyhow!("Goal tool caller has no session"))
    })?;
    create_goal_for_session_inner(node, owner, session, objective, token_budget, Some(caller)).await
}

async fn create_goal_for_session_inner(
    node: &std::sync::Arc<EmbeddedNode>,
    node_did: &str,
    session_id: &str,
    objective: &str,
    token_budget: Option<i64>,
    caller: Option<&gents_protocol::row::AgentRequestRow>,
) -> std::result::Result<CreateGoalForSessionOutcome, CreateGoalForSessionError> {
    let create_request = GoalCreateRequest {
        caller: node_did.to_string(),
        current_session: session_id.to_string(),
        requested_owner: node_did.to_string(),
        requested_session: session_id.to_string(),
        objective: objective.to_string(),
        objective_nonempty: !objective.is_empty(),
        token_budget: token_budget.map(i128::from),
        goal_tools: true,
        goal_create: true,
    };
    match decide_model_goal_create(&create_request, None) {
        GoalCreateDisposition::Invalid if objective.trim().is_empty() => {
            return Err(CreateGoalForSessionError::InvalidObjective);
        }
        GoalCreateDisposition::Invalid => return Err(CreateGoalForSessionError::InvalidBudget),
        GoalCreateDisposition::Fresh => {}
        disposition => unreachable!("owned authorized create preflight returned {disposition:?}"),
    }
    let requested_fingerprint = goal_creation_fingerprint(&create_request);
    let objective = requested_fingerprint.objective.as_str();

    let result = crate::config_client::ConfigAccess::transact_local_write(
        node,
        None,
        "goal.create_for_session",
        move |txn| {
            Box::pin(async move {
                let context = if let Some(caller) = caller {
                    let context = tool_context::load_goal_tool_context_in_txn(
                        txn, node_did, session_id,
                        &caller.request_id,
                        caller.doc_id.as_deref().context("Goal tool caller has no physical identity")?,
                    ).await?;
                    anyhow::ensure!(context.can_create(), "Goal creation requires an unassigned caller with no current Goal, or the exact current assignment");
                    Some(context)
                } else { None };
                let identity = if let Some(tool_context::GoalToolContext { assignment: RequestAssignmentObservation::Assigned(goal), .. }) = &context {
                    let response = txn.execute(&format!(r#"{{ Goal(filter: {{ _docID: {{_eq: "{}"}} }}) {{creation_key}} }}"#,
                        escape_graphql_string(&goal.doc_id))).await?;
                    let creation_key = response.pointer("/data/Goal/0/creation_key").and_then(serde_json::Value::as_str)
                        .filter(|value| !value.is_empty()).context("assigned Goal has no creation identity")?;
                    Some(GoalCreationIdentity { goal_id: goal.goal_id.clone(), creation_key: creation_key.to_string() })
                } else { None };
                let mut staged = stage_goal_and_claim(
                    txn,
                    node_did,
                    session_id,
                    objective,
                    token_budget,
                    GoalState {
                        status: GoalStatus::Active,
                        blocked_audits: 0,
                        wrapup_requested: false,
                        wrapup_completed: false,
                    },
                    &Utc::now().to_rfc3339(),
                    identity.as_ref(),
                ).await?;
                if let Some(context) = context {
                    if matches!(staged.disposition, StagedGoalDisposition::Created) {
                        let goal = load_canonical_goal_in_txn(txn, node_did, session_id).await?
                            .context("created Goal row disappeared before caller binding")?;
                        bind_goal_first_request_in_txn(txn, &goal,
                            context.request.doc_id.as_deref().context("Goal tool caller has no physical identity")?,
                            context.request.input.as_ref().and_then(|input| input.goal_assignment.as_ref()),
                        ).await?;
                        staged.goal = load_canonical_goal_in_txn(txn, node_did, session_id).await?
                            .context("bound Goal row disappeared")?;
                    }
                }
                Ok(staged)
            })
        },
    )
    .await;

    match result {
        Ok(outcome) => {
            let goal = if caller.is_some() {
                outcome.goal
            } else {
                load_canonical_goal(node, node_did, session_id)
                    .await
                    .map_err(CreateGoalForSessionError::Storage)?
                    .context("committed Goal row not found")
                    .map_err(CreateGoalForSessionError::Storage)?
            };
            Ok(match outcome.disposition {
                StagedGoalDisposition::Created => CreateGoalForSessionOutcome::Created(goal),
                StagedGoalDisposition::Idempotent => CreateGoalForSessionOutcome::Idempotent(goal),
            })
        }
        Err(error) if caller.is_some() => match error.downcast::<CreateGoalForSessionError>() {
            Ok(error) => Err(error),
            Err(error) => Err(CreateGoalForSessionError::Storage(error)),
        },
        Err(error) => {
            if let Some(existing) = load_canonical_goal(node, node_did, session_id)
                .await
                .map_err(CreateGoalForSessionError::Storage)?
            {
                if existing.objective.trim() == objective
                    && existing.token_budget == token_budget
                    && load_goal_creation_claim_fingerprint(node, node_did, session_id)
                        .await
                        .map_err(CreateGoalForSessionError::Storage)?
                        == Some(requested_fingerprint.clone())
                {
                    return Ok(CreateGoalForSessionOutcome::Idempotent(existing));
                }
                return Err(CreateGoalForSessionError::Conflict);
            }
            match error.downcast::<CreateGoalForSessionError>() {
                Ok(error) => Err(error),
                Err(error) => Err(CreateGoalForSessionError::Storage(error)),
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalBackedRequestDisposition {
    Created,
    Idempotent,
}

pub(crate) const GOAL_BACKED_REQUEST_FINGERPRINT_FIELDS: &str = r#"
    request_id purpose node_did requester_did agent_id session_id
    retry_parent_request retry_parent_request_doc_id retry_root_request retry_key
    content input
    execution_origin caused_by_trigger_id caused_by_trigger_doc_id
    caused_by_trigger_kind caused_by_correlation caused_by_trigger_context
    caused_by_source_doc_id retry_count max_retries valid_until request_hop
    caused_by_parent_request_id caused_by_parent_request_doc_id
    caused_by_parent_tool_call_id caused_by_parent_tool_call_doc_id
    workspace_id workspace_owner_node_did workspace_authority workspace_seal_hash
    admission_kind admission_signer_did enrollment_request_id
    enrollment_request_digest enrollment_admin_did
    enrollment_authorization_sequence enrollment_authorization_expires_at
    runtime_issuer_did runtime_source_request_id runtime_source_kind
"#;

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub(crate) struct GoalBackedRequestFingerprint {
    request_id: String,
    purpose: gents_protocol::request_admission::RequestPurpose,
    node_did: String,
    requester_did: String,
    agent_id: String,
    session_id: String,
    retry_parent_request: Option<String>,
    retry_parent_request_doc_id: Option<String>,
    retry_root_request: Option<String>,
    retry_key: Option<String>,
    content: String,
    #[serde(
        default,
        deserialize_with = "crate::document_config::deserialize_default_on_null"
    )]
    input: gents_protocol::request_input::RequestInput,
    execution_origin: String,
    caused_by_trigger_id: Option<String>,
    caused_by_trigger_doc_id: Option<String>,
    caused_by_trigger_kind: Option<String>,
    caused_by_correlation: Option<String>,
    caused_by_trigger_context: Option<String>,
    caused_by_source_doc_id: Option<String>,
    retry_count: i64,
    max_retries: i64,
    valid_until: Option<String>,
    request_hop: i64,
    caused_by_parent_request_id: Option<String>,
    caused_by_parent_request_doc_id: Option<String>,
    caused_by_parent_tool_call_id: Option<String>,
    caused_by_parent_tool_call_doc_id: Option<String>,
    workspace_id: Option<String>,
    workspace_owner_node_did: Option<String>,
    workspace_authority: Option<String>,
    workspace_seal_hash: Option<String>,
    admission_kind: String,
    admission_signer_did: String,
    enrollment_request_id: Option<String>,
    enrollment_request_digest: Option<String>,
    enrollment_admin_did: Option<String>,
    enrollment_authorization_sequence: Option<i64>,
    enrollment_authorization_expires_at: Option<String>,
    runtime_issuer_did: Option<String>,
    runtime_source_request_id: Option<String>,
    runtime_source_kind: Option<String>,
}

impl GoalBackedRequestFingerprint {
    pub(crate) fn from_create(
        request: &gents_protocol::request_admission::AgentRequestCreate,
    ) -> Result<Self> {
        // Compile-time fence: adding a new request field requires an explicit
        // decision about whether it belongs in the immutable retry contract.
        let gents_protocol::request_admission::AgentRequestCreate {
            request_id: _,
            purpose: _,
            node_did: _,
            requester_did: _,
            agent_id: _,
            session_id: _,
            retry_parent_request: _,
            retry_parent_request_doc_id: _,
            retry_root_request: _,
            retry_key: _,
            content: _,
            input: _,
            execution_origin: _,
            caused_by_trigger_id: _,
            caused_by_trigger_doc_id: _,
            caused_by_trigger_kind: _,
            caused_by_correlation: _,
            caused_by_trigger_context: _,
            caused_by_source_doc_id: _,
            created_at: _,
            retry_count: _,
            max_retries: _,
            valid_until: _,
            request_hop: _,
            caused_by_parent_request_id: _,
            caused_by_parent_request_doc_id: _,
            caused_by_parent_tool_call_id: _,
            caused_by_parent_tool_call_doc_id: _,
            workspace_id: _,
            workspace_owner_node_did: _,
            workspace_authority: _,
            workspace_seal_hash: _,
            initial_lifecycle_state: _,
            admission: _,
        } = request;
        Ok(Self {
            request_id: request.request_id.clone(),
            purpose: request.purpose,
            node_did: request.node_did.clone(),
            requester_did: request.requester_did.clone(),
            agent_id: request.agent_id.clone(),
            session_id: request.session_id.clone(),
            retry_parent_request: request.retry_parent_request.clone(),
            retry_parent_request_doc_id: request.retry_parent_request_doc_id.clone(),
            retry_root_request: request.retry_root_request.clone(),
            retry_key: request.retry_key.clone(),
            content: request.content.clone(),
            input: request.input.clone(),
            execution_origin: request.execution_origin.clone(),
            caused_by_trigger_id: request.caused_by_trigger_id.clone(),
            caused_by_trigger_doc_id: request.caused_by_trigger_doc_id.clone(),
            caused_by_trigger_kind: request.caused_by_trigger_kind.clone(),
            caused_by_correlation: request.caused_by_correlation.clone(),
            caused_by_trigger_context: request.caused_by_trigger_context.clone(),
            caused_by_source_doc_id: request.caused_by_source_doc_id.clone(),
            retry_count: request.retry_count,
            max_retries: request.max_retries,
            valid_until: request.valid_until.clone(),
            request_hop: i64::from(request.request_hop),
            caused_by_parent_request_id: request.caused_by_parent_request_id.clone(),
            caused_by_parent_request_doc_id: request.caused_by_parent_request_doc_id.clone(),
            caused_by_parent_tool_call_id: request.caused_by_parent_tool_call_id.clone(),
            caused_by_parent_tool_call_doc_id: request.caused_by_parent_tool_call_doc_id.clone(),
            workspace_id: request.workspace_id.clone(),
            workspace_owner_node_did: request.workspace_owner_node_did.clone(),
            workspace_authority: request.workspace_authority.clone(),
            workspace_seal_hash: request.workspace_seal_hash.clone(),
            admission_kind: request.admission.kind.as_str().to_string(),
            admission_signer_did: request.admission.signer_did.clone(),
            enrollment_request_id: request.admission.enrollment_request_id.clone(),
            enrollment_request_digest: request.admission.enrollment_request_digest.clone(),
            enrollment_admin_did: request.admission.enrollment_admin_did.clone(),
            enrollment_authorization_sequence: request
                .admission
                .enrollment_authorization_sequence
                .map(i64::try_from)
                .transpose()
                .context("enrollment authorization sequence exceeds storage range")?,
            enrollment_authorization_expires_at: request
                .admission
                .enrollment_authorization_expires_at
                .clone(),
            runtime_issuer_did: request.admission.runtime_issuer_did.clone(),
            runtime_source_request_id: request.admission.runtime_source_request_id.clone(),
            runtime_source_kind: request
                .admission
                .runtime_source_kind
                .map(|kind| kind.as_str().to_string()),
        })
    }
}

struct StagedGoalBackedRequest {
    disposition: GoalBackedRequestDisposition,
    state: GoalSubmissionState,
}

fn authorize_goal_submission_commit(
    staged: StagedGoalBackedRequest,
) -> Result<GoalBackedRequestDisposition> {
    let committed = goal_submission_step(staged.state, GoalSubmissionAction::Commit);
    anyhow::ensure!(
        committed.durable_goal
            && committed.runnable_request
            && !committed.staged_goal
            && !committed.staged_request,
        "goal-backed request transaction is not ready to commit"
    );
    Ok(staged.disposition)
}

fn resolve_ambiguous_goal_submission_commit(
    commit_error: anyhow::Error,
    recovery: Result<Option<crate::lifecycle::materialize::EnqueuedAgentRequest>>,
) -> Result<GoalBackedRequestDisposition> {
    match recovery {
        Ok(Some(_)) => Ok(GoalBackedRequestDisposition::Idempotent),
        Ok(None) => Err(commit_error),
        Err(recovery_error) => Err(anyhow::anyhow!(
            "goal-backed request recovery failed after ambiguous commit: {recovery_error:#}; original commit error: {commit_error:#}"
        )),
    }
}

async fn stage_goal_backed_request(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
    objective: &str,
    token_budget: Option<i64>,
    request: &gents_protocol::request_admission::AgentRequestCreate,
    identity: Option<&GoalCreationIdentity>,
) -> Result<StagedGoalBackedRequest> {
    let objective = objective.trim();
    anyhow::ensure!(!objective.is_empty(), "goal objective must be non-empty");
    anyhow::ensure!(
        token_budget.is_none_or(|budget| budget > 0),
        "goal token budget must be positive"
    );
    anyhow::ensure!(request.node_did == node_did, "goal/request DID mismatch");
    anyhow::ensure!(
        request.session_id == session_id,
        "goal/request session mismatch"
    );
    let retry_key = request
        .retry_key
        .as_deref()
        .context("goal-backed request requires a stable retry_key")?;

    let staged_goal = stage_goal_and_claim(
        txn,
        node_did,
        session_id,
        objective,
        token_budget,
        GoalState {
            status: GoalStatus::Active,
            blocked_audits: 0,
            wrapup_requested: false,
            wrapup_completed: false,
        },
        &Utc::now().to_rfc3339(),
        identity,
    )
    .await?;
    let mut submission_state = goal_submission_step(
        GoalSubmissionState {
            durable_goal: false,
            runnable_request: false,
            staged_goal: false,
            staged_request: false,
        },
        GoalSubmissionAction::StageGoal,
    );
    let existing = txn
        .execute(&format!(
            r#"{{
                    AgentRequest(filter: {{ retry_key: {{ _eq: "{}" }} }}) {{ _docID
                        {GOAL_BACKED_REQUEST_FINGERPRINT_FIELDS}
                    }}
                }}"#,
            escape_graphql_string(retry_key),
        ))
        .await?;
    let requests = existing
        .pointer("/data/AgentRequest")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    anyhow::ensure!(requests.len() <= 1, "goal request retry key is not unique");
    if let Some(existing_request) = requests.first() {
        let persisted: GoalBackedRequestFingerprint =
            serde_json::from_value(existing_request.clone())
                .context("decoding goal-backed request fingerprint")?;
        anyhow::ensure!(
            persisted == GoalBackedRequestFingerprint::from_create(request)?,
            "goal request retry key conflicts with different immutable request fields"
        );
        bind_goal_first_request_in_txn(
            txn,
            &staged_goal.goal,
            existing_request["_docID"]
                .as_str()
                .context("persisted Goal request lacks physical identity")?,
            request.input.goal_assignment.as_ref(),
        )
        .await?;
        submission_state =
            goal_submission_step(submission_state, GoalSubmissionAction::StageRequest);
        anyhow::ensure!(
            submission_state.staged_request,
            "validated goal-backed request could not be staged"
        );
        return Ok(StagedGoalBackedRequest {
            disposition: GoalBackedRequestDisposition::Idempotent,
            state: submission_state,
        });
    }
    anyhow::ensure!(
        staged_goal.goal.parsed_status() == Some(GoalStatus::Active),
        "the session already has a non-active goal"
    );
    crate::graph_pipeline::fence_graph_root_request_in_txn(txn, request).await?;
    let request_fields = request.graphql_input_fields().map_err(anyhow::Error::msg)?;
    let created = txn
        .execute(&format!(
            "mutation {{ create_AgentRequest(input: {{ {request_fields} }}) {{ _docID }} }}"
        ))
        .await?;
    let request_doc_id = crate::graphql::created_doc_id(&created, "AgentRequest")?;
    bind_goal_first_request_in_txn(
        txn,
        &staged_goal.goal,
        &request_doc_id,
        request.input.goal_assignment.as_ref(),
    )
    .await?;
    submission_state = goal_submission_step(submission_state, GoalSubmissionAction::StageRequest);
    anyhow::ensure!(
        submission_state.staged_request,
        "created goal-backed request could not be staged"
    );
    Ok(StagedGoalBackedRequest {
        disposition: GoalBackedRequestDisposition::Created,
        state: submission_state,
    })
}

pub async fn submit_goal_backed_request(
    access: &crate::ConfigAccess,
    node_did: &str,
    session_id: &str,
    objective: &str,
    token_budget: Option<i64>,
    request: &gents_protocol::request_admission::AgentRequestCreate,
) -> Result<GoalBackedRequestDisposition> {
    let result = access
        .transact_write("goal.submit_backed_request", move |txn| {
            Box::pin(async move {
                stage_goal_backed_request(
                    txn,
                    node_did,
                    session_id,
                    objective,
                    token_budget,
                    request,
                    None,
                )
                .await
                .and_then(authorize_goal_submission_commit)
            })
        })
        .await;
    match result {
        Ok(disposition) => Ok(disposition),
        Err(error) => {
            let recovery = load_goal_backed_request_by_retry_key_from_access(
                access,
                node_did,
                session_id,
                objective,
                token_budget,
                request,
            )
            .await;
            resolve_ambiguous_goal_submission_commit(error, recovery)
        }
    }
}

fn goal_creation_claim_selection(node_did: &str, session_id: &str) -> String {
    format!(
        r#"Goal(
            filter: {{ node_did: {{ _eq: "{}" }}, session_id: {{ _eq: "{}" }} }},
            order: [{{ created_at: ASC }}, {{ goal_id: ASC }}]
        ) {{ {GOAL_FIELDS} }}
        GoalCreationClaim(filter: {{ creation_key: {{ _eq: "{}" }} }}, limit: 2) {{
            goal_id node_did session_id objective token_budget
        }}"#,
        escape_graphql_string(node_did),
        escape_graphql_string(session_id),
        escape_graphql_string(&deterministic_goal_creation_key(node_did, session_id)),
    )
}

fn goal_backed_request_recovery_query(
    node_did: &str,
    session_id: &str,
    request: &gents_protocol::request_admission::AgentRequestCreate,
) -> Result<String> {
    let retry_key = request
        .retry_key
        .as_deref()
        .context("goal-backed request requires a stable retry_key")?;
    Ok(format!(
        r#"{{
            AgentRequest(filter: {{ retry_key: {{ _eq: "{}" }} }}, limit: 2) {{
                _docID {GOAL_BACKED_REQUEST_FINGERPRINT_FIELDS}
            }}
            {}
        }}"#,
        escape_graphql_string(retry_key),
        goal_creation_claim_selection(node_did, session_id),
    ))
}

fn decode_agent_request_by_retry_key(
    rows: &[serde_json::Value],
    expected: &GoalBackedRequestFingerprint,
) -> Result<Option<String>> {
    anyhow::ensure!(rows.len() <= 1, "AgentRequest retry key is not unique");
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let persisted: GoalBackedRequestFingerprint =
        serde_json::from_value(row.clone()).context("decoding AgentRequest fingerprint")?;
    anyhow::ensure!(
        persisted == *expected,
        "AgentRequest retry key conflicts with a different immutable request"
    );
    row.get("_docID")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .context("AgentRequest matched by retry key has no document ID")
        .map(Some)
}

pub(crate) async fn load_agent_request_by_retry_key(
    node: &EmbeddedNode,
    retry_key: &str,
    expected: &GoalBackedRequestFingerprint,
) -> Result<Option<String>> {
    let query = format!(
        r#"{{ AgentRequest(filter: {{ retry_key: {{ _eq: "{}" }} }}, limit: 2) {{
            _docID {GOAL_BACKED_REQUEST_FINGERPRINT_FIELDS}
        }} }}"#,
        escape_graphql_string(retry_key),
    );
    let response =
        graphql_with_transaction_retry(node, &query, "load AgentRequest by retry key").await?;
    decode_agent_request_by_retry_key(&rows(&response, "AgentRequest")?, expected)
}

fn verify_goal_creation_claim_cross_check(
    response: &serde_json::Value,
    node_did: &str,
    session_id: &str,
    objective: &str,
    token_budget: Option<i64>,
) -> Result<()> {
    let expected_claim = GoalCreationFingerprint {
        owner: node_did.to_string(),
        session: session_id.to_string(),
        objective: objective.trim().to_string(),
        token_budget: token_budget.map(i128::from),
    };
    let mut goals: Vec<GoalDocument> = serde_json::from_value(
        response
            .pointer("/data/Goal")
            .cloned()
            .unwrap_or_else(|| serde_json::Value::Array(Vec::new())),
    )
    .context("decoding committed goal-backed request Goal rows")?;
    sort_goals_canonical(&mut goals);
    let goal = goals
        .first()
        .context("goal-backed request exists without its durable goal")?;
    let claims = response
        .pointer("/data/GoalCreationClaim")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    anyhow::ensure!(
        claims.len() <= 1,
        "goal-backed request creation key is not unique"
    );
    let claim = claims
        .first()
        .context("goal-backed request exists without its durable goal claim")?;
    let claim_goal_id = claim
        .get("goal_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let claim = GoalCreationFingerprint {
        owner: claim
            .get("node_did")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        session: claim
            .get("session_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        objective: claim
            .get("objective")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        token_budget: claim
            .get("token_budget")
            .and_then(serde_json::Value::as_i64)
            .map(i128::from),
    };
    anyhow::ensure!(
        goal.node_did == node_did
            && goal.session_id == session_id
            && goal.objective.trim() == expected_claim.objective
            && goal.token_budget == token_budget
            && claim_goal_id == goal.goal_id
            && claim == expected_claim,
        "goal-backed request conflicts with its durable goal claim"
    );
    Ok(())
}

fn decode_goal_backed_request_recovery(
    response: &serde_json::Value,
    node_did: &str,
    session_id: &str,
    objective: &str,
    token_budget: Option<i64>,
    request: &gents_protocol::request_admission::AgentRequestCreate,
) -> Result<Option<crate::lifecycle::materialize::EnqueuedAgentRequest>> {
    let rows = response
        .pointer("/data/AgentRequest")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let expected = GoalBackedRequestFingerprint::from_create(request)?;
    let Some(doc_id) = decode_agent_request_by_retry_key(&rows, &expected)? else {
        return Ok(None);
    };
    verify_goal_creation_claim_cross_check(
        response,
        node_did,
        session_id,
        objective,
        token_budget,
    )?;
    Ok(Some(crate::lifecycle::materialize::EnqueuedAgentRequest {
        doc_id,
        request_id: request.request_id.clone(),
        session_id: request.session_id.clone(),
    }))
}

async fn load_goal_backed_request_by_retry_key_from_access(
    access: &crate::ConfigAccess,
    node_did: &str,
    session_id: &str,
    objective: &str,
    token_budget: Option<i64>,
    request: &gents_protocol::request_admission::AgentRequestCreate,
) -> Result<Option<crate::lifecycle::materialize::EnqueuedAgentRequest>> {
    let query = goal_backed_request_recovery_query(node_did, session_id, request)?;
    let response = access.execute(&query).await?;
    decode_goal_backed_request_recovery(
        &response,
        node_did,
        session_id,
        objective,
        token_budget,
        request,
    )
}

fn goal_creation_claim_recovery_query(node_did: &str, session_id: &str) -> String {
    format!(
        "{{ {} }}",
        goal_creation_claim_selection(node_did, session_id)
    )
}

async fn load_goal_backed_request_by_retry_key(
    node: &EmbeddedNode,
    node_did: &str,
    session_id: &str,
    objective: &str,
    token_budget: Option<i64>,
    request: &gents_protocol::request_admission::AgentRequestCreate,
) -> Result<Option<crate::lifecycle::materialize::EnqueuedAgentRequest>> {
    let retry_key = request
        .retry_key
        .as_deref()
        .context("goal-backed request requires a stable retry_key")?;
    let expected = GoalBackedRequestFingerprint::from_create(request)?;
    let Some(doc_id) = load_agent_request_by_retry_key(node, retry_key, &expected).await? else {
        return Ok(None);
    };
    let query = goal_creation_claim_recovery_query(node_did, session_id);
    let response =
        graphql_with_transaction_retry(node, &query, "load goal-backed request creation claim")
            .await?;
    let response = serde_json::json!({
        "data": response.data.unwrap_or(serde_json::Value::Null),
    });
    verify_goal_creation_claim_cross_check(
        &response,
        node_did,
        session_id,
        objective,
        token_budget,
    )?;
    Ok(Some(crate::lifecycle::materialize::EnqueuedAgentRequest {
        doc_id,
        request_id: request.request_id.clone(),
        session_id: request.session_id.clone(),
    }))
}

pub(crate) async fn stage_goal_backed_request_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
    objective: &str,
    token_budget: Option<i64>,
    request: &gents_protocol::request_admission::AgentRequestCreate,
) -> Result<crate::lifecycle::EnqueuedAgentRequest> {
    stage_goal_backed_request_with_identity_in_txn(
        txn,
        node_did,
        session_id,
        objective,
        token_budget,
        request,
        None,
    )
    .await
}

pub(crate) async fn stage_goal_backed_request_with_identity_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
    objective: &str,
    token_budget: Option<i64>,
    request: &gents_protocol::request_admission::AgentRequestCreate,
    identity: Option<&GoalCreationIdentity>,
) -> Result<crate::lifecycle::EnqueuedAgentRequest> {
    stage_goal_backed_request(
        txn,
        node_did,
        session_id,
        objective,
        token_budget,
        request,
        identity,
    )
    .await
    .and_then(authorize_goal_submission_commit)?;
    let retry_key = request
        .retry_key
        .as_deref()
        .context("goal-backed request requires a stable retry_key")?;
    let staged = txn
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ retry_key: {{ _eq: "{}" }} }}, limit: 2) {{ _docID request_id session_id }} }}"#,
            escape_graphql_string(retry_key),
        ))
        .await?;
    let rows = staged
        .pointer("/data/AgentRequest")
        .and_then(serde_json::Value::as_array)
        .context("staged goal request lookup omitted rows")?;
    anyhow::ensure!(
        rows.len() == 1,
        "staged goal request is missing or ambiguous"
    );
    Ok(crate::lifecycle::EnqueuedAgentRequest {
        doc_id: rows[0]["_docID"]
            .as_str()
            .context("staged goal request lacks _docID")?
            .to_owned(),
        request_id: request.request_id.clone(),
        session_id: request.session_id.clone(),
    })
}

pub async fn submit_goal_backed_request_local(
    node: &std::sync::Arc<EmbeddedNode>,
    actor: ::identity::Did,
    node_did: &str,
    session_id: &str,
    objective: &str,
    token_budget: Option<i64>,
    request: &gents_protocol::request_admission::AgentRequestCreate,
) -> Result<crate::lifecycle::EnqueuedAgentRequest> {
    let staged = crate::config_client::ConfigAccess::transact_local_write(
        node,
        Some(actor),
        "goal.submit_backed_request_local",
        move |txn| {
            Box::pin(async move {
                stage_goal_backed_request(
                    txn,
                    node_did,
                    session_id,
                    objective,
                    token_budget,
                    request,
                    None,
                )
                .await
                .and_then(authorize_goal_submission_commit)
            })
        },
    )
    .await;
    match staged {
        Ok(_) => {}
        Err(error) => {
            if let Some(recovered) = load_goal_backed_request_by_retry_key(
                node,
                node_did,
                session_id,
                objective,
                token_budget,
                request,
            )
            .await?
            {
                return Ok(recovered);
            }
            return Err(error);
        }
    }
    load_goal_backed_request_by_retry_key(
        node,
        node_did,
        session_id,
        objective,
        token_budget,
        request,
    )
    .await?
    .context("committed goal-backed request was not found by retry key")
}

pub async fn load_goals_for_session(
    node: &EmbeddedNode,
    node_did: &str,
    session_id: &str,
) -> Result<Vec<GoalDocument>> {
    let node_did = escape_graphql_string(node_did);
    let session_id = escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            Goal(
                filter: {{
                    node_did: {{ _eq: "{node_did}" }},
                    session_id: {{ _eq: "{session_id}" }}
                }},
                order: [{{ created_at: ASC }}, {{ goal_id: ASC }}]
            ) {{ {GOAL_FIELDS} }}
        }}"#
    );
    let mut goals = decode_goal_rows(node, &query).await?;
    sort_goals_canonical(&mut goals);
    Ok(goals)
}

pub async fn load_canonical_goal(
    node: &EmbeddedNode,
    node_did: &str,
    session_id: &str,
) -> Result<Option<GoalDocument>> {
    Ok(load_goals_for_session(node, node_did, session_id)
        .await?
        .into_iter()
        .next())
}

pub async fn load_goal_by_id(
    node: &EmbeddedNode,
    node_did: &str,
    goal_id: &str,
) -> Result<Option<GoalDocument>> {
    let node_did = escape_graphql_string(node_did);
    let goal_id = escape_graphql_string(goal_id);
    let query = format!(
        r#"{{
            Goal(
                filter: {{ node_did: {{ _eq: "{node_did}" }}, goal_id: {{ _eq: "{goal_id}" }} }},
                showDeleted: true,
                order: [{{ created_at: ASC }}, {{ goal_id: ASC }}]
            ) {{ {GOAL_FIELDS} }}
        }}"#
    );
    let mut goals = decode_goal_rows(node, &query).await?;
    sort_goals_canonical(&mut goals);
    Ok(goals.into_iter().next())
}

pub(crate) fn sort_goals_canonical(goals: &mut [GoalDocument]) {
    goals.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left.goal_id.cmp(&right.goal_id))
            .then_with(|| left.doc_id.cmp(&right.doc_id))
    });
}

async fn decode_goal_rows(node: &EmbeddedNode, query: &str) -> Result<Vec<GoalDocument>> {
    let response = graphql_with_transaction_retry(node, query, "Goal query").await?;
    rows(&response, "Goal").context("decoding Goal rows")
}

pub async fn set_goal(
    node: &std::sync::Arc<EmbeddedNode>,
    node_did: &str,
    session_id: &str,
    objective: Option<&str>,
    status: Option<GoalStatus>,
    token_budget: Option<Option<i64>>,
) -> Result<GoalDocument> {
    crate::config_client::ConfigAccess::transact_local_write(node, None, "goal.set", move |txn| {
        Box::pin(async move {
            set_goal_in_txn(
                txn,
                node_did,
                session_id,
                objective,
                status,
                token_budget,
                None,
            )
            .await
        })
    })
    .await
}

pub async fn set_goal_from_access(
    access: &crate::ConfigAccess,
    node_did: &str,
    session_id: &str,
    objective: Option<&str>,
    status: Option<GoalStatus>,
    token_budget: Option<Option<i64>>,
    auto_resume: Option<bool>,
) -> Result<GoalDocument> {
    access
        .transact_write("goal.set_from_access", move |txn| {
            Box::pin(async move {
                set_goal_in_txn(
                    txn,
                    node_did,
                    session_id,
                    objective,
                    status,
                    token_budget,
                    auto_resume,
                )
                .await
            })
        })
        .await
}

/// An operator reset preference belongs to the observed Goal, including when
/// another assignment replaces the session's Goal before the write arrives.
pub async fn set_goal_auto_resume_from_access(
    access: &crate::ConfigAccess,
    node_did: &str,
    session_id: &str,
    expected_goal_id: &str,
    enabled: bool,
) -> Result<()> {
    access
        .transact_write("goal.set_auto_resume", |txn| {
            Box::pin(async move {
                let goal = load_canonical_goal_in_txn(txn, node_did, session_id)
                    .await?
                    .context("Goal no longer exists")?;
                anyhow::ensure!(
                    goal.goal_id == expected_goal_id,
                    "Goal changed before its reset preference was saved"
                );
                set_goal_in_txn(txn, node_did, session_id, None, None, None, Some(enabled)).await?;
                Ok(())
            })
        })
        .await
}

pub async fn load_canonical_goal_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
) -> Result<Option<GoalDocument>> {
    let node_did = escape_graphql_string(node_did);
    let session_id = escape_graphql_string(session_id);
    let response = txn
        .execute(&format!(
            r#"{{ Goal(filter: {{
        node_did: {{ _eq: "{node_did}" }}, session_id: {{ _eq: "{session_id}" }}
    }}) {{ {GOAL_FIELDS} }} }}"#
        ))
        .await?;
    let mut goals: Vec<GoalDocument> = serde_json::from_value(
        response
            .pointer("/data/Goal")
            .cloned()
            .context("transactional Goal query omitted rows")?,
    )
    .context("decoding transactional Goal rows")?;
    sort_goals_canonical(&mut goals);
    Ok(goals.into_iter().next())
}

pub(crate) async fn pause_goal_for_interrupt_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
) -> Result<()> {
    let Some(goal) = load_canonical_goal_in_txn(txn, node_did, session_id).await? else {
        return Ok(());
    };
    let state = goal.state().context("Goal has an unknown status")?;
    if state.step(GoalAction::Pause).is_some() {
        set_goal_in_txn(
            txn,
            node_did,
            session_id,
            None,
            Some(GoalStatus::Paused),
            None,
            None,
        )
        .await?;
    }
    Ok(())
}

pub(crate) async fn apply_claimed_task_goal_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    request: &crate::watcher::AgentRequest,
    now: &str,
) -> Result<()> {
    let owner = escape_graphql_string(&request.node_did);
    let request_id = escape_graphql_string(&request.request_id);
    let response = txn.execute_local_response(&format!(
        r#"{{ TriggerFire(filter: {{owner_did: {{_eq: "{owner}"}}, request_id: {{_eq: "{request_id}"}}}}) {{
            _docID session_id goal_id goal_objective goal_token_budget goal_assignment_applied
        }} }}"#,
    )).await?;
    #[derive(Deserialize)]
    struct Assignment {
        #[serde(rename = "_docID")]
        doc_id: String,
        session_id: String,
        goal_id: Option<String>,
        goal_objective: Option<String>,
        goal_token_budget: Option<i64>,
        #[serde(default)]
        goal_assignment_applied: bool,
    }
    let assignments: Vec<Assignment> = rows(&response, "TriggerFire")?;
    anyhow::ensure!(
        assignments.len() <= 1,
        "request has multiple Task fire receipts"
    );
    let Some(assignment) = assignments.into_iter().next() else {
        return Ok(());
    };
    if assignment.goal_id.is_none() || assignment.goal_assignment_applied {
        return Ok(());
    }
    anyhow::ensure!(
        assignment.session_id == request.session_id,
        "Task receipt session differs from claimed request"
    );
    validate_task_goal_identity_in_txn(
        txn,
        &request.node_did,
        &request.session_id,
        assignment
            .goal_id
            .as_deref()
            .context("Task assignment lost Goal identity")?,
        request.input.goal_assignment.as_ref(),
    )
    .await?;
    let objective = assignment
        .goal_objective
        .as_deref()
        .map(str::trim)
        .filter(|objective| !objective.is_empty())
        .context("Goal-backed Task has no objective")?;
    anyhow::ensure!(
        assignment.goal_token_budget.is_none_or(|budget| budget > 0),
        "Task Goal budget must be positive"
    );
    let previous = load_canonical_goal_in_txn(txn, &request.node_did, &request.session_id).await?;
    if let Some(goal) = &previous {
        anyhow::ensure!(
            goal.parsed_status().is_some(),
            "existing Task Goal has an unknown status"
        );
        crate::trigger_engine::durable::publish_goal_outcomes(
            txn,
            &goal.node_did,
            &goal.goal_id,
            goal.last_blocked_reason.as_deref().unwrap_or(&goal.status),
            now,
        )
        .await?;
    }
    let was_active = previous
        .as_ref()
        .is_some_and(|goal| goal.parsed_status() == Some(GoalStatus::Active));
    let next_sequence = previous
        .as_ref()
        .map(|goal| {
            goal.continuation_sequence()
                .checked_add(1)
                .context("Task Goal continuation sequence exhausted")
        })
        .transpose()?
        .unwrap_or(0);
    if previous.is_none() || was_active {
        set_goal_in_txn(
            txn,
            &request.node_did,
            &request.session_id,
            Some(objective),
            Some(GoalStatus::Active),
            Some(assignment.goal_token_budget),
            None,
        )
        .await?;
    }
    let goal = load_canonical_goal_in_txn(txn, &request.node_did, &request.session_id)
        .await?
        .context("Task Goal assignment has no canonical row")?;
    let goal_doc_id = escape_graphql_string(&goal.doc_id);
    let expected_sequence = goal.continuation_sequence();
    let objective = escape_graphql_string(objective);
    let budget = optional_int_graphql_field("token_budget", assignment.goal_token_budget);
    let timestamp = escape_graphql_string(now);
    let assignment_root = escape_graphql_string(&request.doc_id);
    let reset = if was_active {
        String::new()
    } else {
        let baseline =
            session_token_usage_in_txn(txn, &request.node_did, &request.session_id).await?;
        format!(
            r#"tokens_used: 0, token_usage_baseline: {baseline},
            active_time_seconds: 0, active_started_at: "{timestamp}",
            consecutive_blocked_audits: 0, last_blocked_request_id: null,
            last_blocked_reason: null, last_continued_from_request_id: null,
            wrapup_requested: false, wrapup_completed: false,
            infrastructure_retry_count: 0, last_failure: null, completion_evidence: null,"#
        )
    };
    let response = txn
        .execute_local_response(&format!(
            r#"mutation {{
        update_Goal(filter: {{_docID: {{_eq: "{goal_doc_id}"}}, node_did: {{_eq: "{owner}"}},
            continuation_sequence: {{_eq: {expected_sequence}}}}}, input: {{
            objective: "{objective}", status: "active", {budget}
            assignment_root_request_doc_id: "{assignment_root}",
            continuation_sequence: {next_sequence}, {reset} updated_at: "{timestamp}"
        }}) {{_docID}}
    }}"#
        ))
        .await?;
    anyhow::ensure!(
        response
            .data
            .as_ref()
            .and_then(|data| data.get("update_Goal"))
            .is_some_and(mutation_returned_rows),
        "Task Goal assignment lost its continuation fence"
    );
    let updated = load_canonical_goal_in_txn(txn, &request.node_did, &request.session_id)
        .await?
        .context("assigned Task Goal disappeared")?;
    request_head::retire_stale_assignment_continuations_in_txn(txn, &updated, None, now).await?;
    fence_opened_graph_goal_in_txn(
        txn,
        previous.as_ref().and_then(GoalDocument::state),
        &updated,
    )
    .await?;
    let receipt_doc_id = escape_graphql_string(&assignment.doc_id);
    txn.execute(&format!(
        r#"mutation {{ update_TriggerFire(filter: {{_docID: {{_eq: "{receipt_doc_id}"}}}},
        input: {{goal_assignment_applied: true}}) {{_docID}} }}"#
    ))
    .await?;
    crate::trigger_engine::durable::publish_goal_outcomes(
        txn,
        &updated.node_did,
        &updated.goal_id,
        "Task Goal assignment replaced",
        now,
    )
    .await?;
    Ok(())
}

async fn set_goal_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
    objective: Option<&str>,
    status: Option<GoalStatus>,
    token_budget: Option<Option<i64>>,
    auto_resume: Option<bool>,
) -> Result<GoalDocument> {
    anyhow::ensure!(
        objective.is_none_or(|value| !value.trim().is_empty()),
        "goal objective must be non-empty"
    );
    let existing = load_canonical_goal_in_txn(txn, node_did, session_id).await?;
    let objective = objective
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| existing.as_ref().map(|goal| goal.objective.clone()))
        .context("a goal objective is required")?;
    let status = status
        .or_else(|| existing.as_ref().and_then(GoalDocument::parsed_status))
        .unwrap_or(GoalStatus::Active);
    let budget =
        token_budget.unwrap_or_else(|| existing.as_ref().and_then(|goal| goal.token_budget));
    if budget.is_some_and(|value| value <= 0) {
        bail!("goal token budget must be positive");
    }
    let auto_resume_field = auto_resume
        .map(|on| format!("auto_resume_at_reset: {on},"))
        .unwrap_or_default();

    let now = Utc::now();
    let now_string = now.to_rfc3339();
    if let Some(existing) = existing {
        let pre = existing
            .state()
            .context("existing Goal has an unknown status")?;
        anyhow::ensure!(
            pre.status == GoalStatus::Active || status != GoalStatus::Active,
            "reactivating an existing goal requires goal resume-request --from REQUEST_ID"
        );
        let post = apply_operator_status_transition(pre, status)?;
        let active_time = existing.current_active_time_seconds(now);
        let active_started_at = if post.status.accrues_active_time() {
            now_string.clone()
        } else {
            String::new()
        };
        let budget_field = optional_int_graphql_field("token_budget", budget);
        let active_started_field = optional_string_graphql_field(
            "active_started_at",
            (!active_started_at.is_empty()).then_some(active_started_at.as_str()),
        );
        let doc_id = escape_graphql_string(&existing.doc_id);
        let escaped_node_did = escape_graphql_string(node_did);
        let objective = escape_graphql_string(&objective);
        let status = post.status.as_str();
        let now = escape_graphql_string(&now_string);
        let mutation = format!(
            r#"mutation {{
                update_Goal(
                    filter: {{ _docID: {{ _eq: "{doc_id}" }}, node_did: {{ _eq: "{escaped_node_did}" }} }},
                    input: {{
                        objective: "{objective}",
                        status: "{status}",
                        {budget_field}
                        tokens_used: {tokens_used},
                        active_time_seconds: {active_time},
                        {active_started_field}
                        consecutive_blocked_audits: {blocked_audits},
                        wrapup_requested: {wrapup_requested},
                        wrapup_completed: {wrapup_completed},
                        {auto_resume_field}
                        updated_at: "{now}"
                    }}
                ) {{ _docID }}
            }}"#,
            tokens_used = existing.tokens_used.unwrap_or_default().max(0),
            blocked_audits = post.blocked_audits,
            wrapup_requested = post.wrapup_requested,
            wrapup_completed = post.wrapup_completed,
        );
        txn.execute(&mutation).await?;
        let updated = load_canonical_goal_in_txn(txn, node_did, session_id)
            .await?
            .context("updated Goal row disappeared")?;
        fence_opened_graph_goal_in_txn(txn, Some(pre), &updated).await?;
        crate::trigger_engine::durable::publish_goal_outcomes(
            txn,
            node_did,
            &updated.goal_id,
            updated
                .last_blocked_reason
                .as_deref()
                .unwrap_or(&updated.status),
            &now_string,
        )
        .await?;
        return Ok(updated);
    }

    let initial_state = GoalState {
        status,
        blocked_audits: if status == GoalStatus::Blocked {
            BLOCKED_AUDIT_THRESHOLD
        } else {
            0
        },
        wrapup_requested: status == GoalStatus::BudgetLimited,
        wrapup_completed: status == GoalStatus::Complete,
    };
    let goal_id = deterministic_goal_id(node_did, session_id);
    let escaped_goal_id = escape_graphql_string(&goal_id);
    let escaped_node_did = escape_graphql_string(node_did);
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_objective = escape_graphql_string(&objective);
    let escaped_now = escape_graphql_string(&now_string);
    let budget_field = optional_int_graphql_field("token_budget", budget);
    let baseline = session_token_usage_in_txn(txn, node_did, session_id).await?;
    let active_started_field = optional_string_graphql_field(
        "active_started_at",
        status.accrues_active_time().then_some(now_string.as_str()),
    );
    let mutation = format!(
        r#"mutation {{
            create_Goal(input: {{
                goal_id: "{escaped_goal_id}",
                session_id: "{escaped_session_id}",
                node_did: "{escaped_node_did}",
                objective: "{escaped_objective}",
                status: "{status}",
                {budget_field}
                tokens_used: 0,
                token_usage_baseline: {baseline},
                active_time_seconds: 0,
                {active_started_field}
                consecutive_blocked_audits: {blocked_audits},
                continuation_sequence: 0,
                wrapup_requested: {wrapup_requested},
                wrapup_completed: {wrapup_completed},
                infrastructure_retry_count: 0,
                {auto_resume_field}
                created_at: "{escaped_now}",
                updated_at: "{escaped_now}"
            }}) {{ _docID }}
        }}"#,
        status = status.as_str(),
        blocked_audits = initial_state.blocked_audits,
        wrapup_requested = initial_state.wrapup_requested,
        wrapup_completed = initial_state.wrapup_completed,
    );
    txn.execute(&mutation).await?;
    let created = load_canonical_goal_in_txn(txn, node_did, session_id)
        .await?
        .context("created Goal row not found")?;
    fence_opened_graph_goal_in_txn(txn, None, &created).await?;
    Ok(created)
}

pub async fn delete_goal(node: &std::sync::Arc<EmbeddedNode>, goal: &GoalDocument) -> Result<bool> {
    let doc_id = escape_graphql_string(&goal.doc_id);
    let node_did = escape_graphql_string(&goal.node_did);
    let mutation = format!(
        r#"mutation {{
            delete_Goal(filter: {{ _docID: {{ _eq: "{doc_id}" }}, node_did: {{ _eq: "{node_did}" }} }}) {{ _docID }}
        }}"#
    );
    let response =
        crate::config_client::ConfigAccess::write_local_response(node, "goal.delete", &mutation)
            .await?;
    Ok(response
        .data
        .as_ref()
        .and_then(|data| data.get("delete_Goal"))
        .is_some_and(mutation_returned_rows))
}

pub async fn delete_goals_for_session(
    node: &std::sync::Arc<EmbeddedNode>,
    node_did: &str,
    session_id: &str,
) -> Result<usize> {
    let node_did = escape_graphql_string(node_did);
    let session_id = escape_graphql_string(session_id);
    let node_did = &node_did;
    let session_id = &session_id;
    crate::config_client::ConfigAccess::transact_local_write(
        node,
        None,
        "goal.delete_for_session",
        move |txn| {
            Box::pin(async move {
                let response = txn
                    .execute(&format!(
                        r#"mutation {{
            delete_Goal(filter: {{
                node_did: {{ _eq: "{node_did}" }},
                session_id: {{ _eq: "{session_id}" }}
            }}) {{ _docID }}
        }}"#
                    ))
                    .await?;
                txn.execute(&format!(
                    r#"mutation {{
                delete_GoalCreationClaim(filter: {{
                    node_did: {{ _eq: "{node_did}" }},
                    session_id: {{ _eq: "{session_id}" }}
                }}) {{ _docID }}
            }}"#
                ))
                .await?;
                Ok::<_, anyhow::Error>(
                    response
                        .pointer("/data/delete_Goal")
                        .map(mutation_row_count)
                        .unwrap_or_default(),
                )
            })
        },
    )
    .await
}

pub async fn update_goal_fields_if_status(
    node: &std::sync::Arc<EmbeddedNode>,
    goal: &GoalDocument,
    expected_status: GoalStatus,
    fields: &str,
) -> Result<bool> {
    let doc_id = escape_graphql_string(&goal.doc_id);
    let node_did = escape_graphql_string(&goal.node_did);
    let expected_status = escape_graphql_string(expected_status.as_str());
    let expected_sequence = goal.continuation_sequence();
    let mutation = format!(
        r#"mutation {{
            update_Goal(
                filter: {{
                    _docID: {{ _eq: "{doc_id}" }},
                    node_did: {{ _eq: "{node_did}" }},
                    status: {{ _eq: "{expected_status}" }},
                    continuation_sequence: {{ _eq: {expected_sequence} }}
                }},
                input: {{ {fields} }}
            ) {{ _docID }}
        }}"#
    );
    crate::config_client::ConfigAccess::transact_local_write(
        node,
        None,
        "goal.update_controller_fields",
        |txn| {
            Box::pin(async {
                let current =
                    load_canonical_goal_in_txn(txn, &goal.node_did, &goal.session_id).await?;
                if !current
                    .as_ref()
                    .is_some_and(|current| same_goal_assignment(current, goal))
                {
                    return Ok(false);
                }
                let response = txn.execute(&mutation).await?;
                let won = response
                    .pointer("/data/update_Goal")
                    .is_some_and(mutation_returned_rows);
                if won {
                    let updated = load_canonical_goal_in_txn(txn, &goal.node_did, &goal.session_id)
                        .await?
                        .context("updated Goal row disappeared")?;
                    crate::trigger_engine::durable::publish_goal_outcomes(
                        txn,
                        &updated.node_did,
                        &updated.goal_id,
                        updated
                            .last_blocked_reason
                            .as_deref()
                            .unwrap_or(&updated.status),
                        &Utc::now().to_rfc3339(),
                    )
                    .await?;
                }
                Ok(won)
            })
        },
    )
    .await
}

pub async fn claim_continuation(
    node: &std::sync::Arc<EmbeddedNode>,
    goal: &GoalDocument,
    parent_request_id: &str,
) -> Result<bool> {
    let doc_id = escape_graphql_string(&goal.doc_id);
    let node_did = escape_graphql_string(&goal.node_did);
    let parent_request_id = escape_graphql_string(parent_request_id);
    let expected_sequence = goal.continuation_sequence();
    let next_sequence = expected_sequence
        .checked_add(1)
        .context("goal continuation sequence exhausted")?;
    let expected_status = escape_graphql_string(&goal.status);
    let now = escape_graphql_string(&Utc::now().to_rfc3339());
    let mutation = format!(
        r#"mutation {{
            update_Goal(
                filter: {{
                    _docID: {{ _eq: "{doc_id}" }},
                    node_did: {{ _eq: "{node_did}" }},
                    status: {{ _eq: "{expected_status}" }},
                    continuation_sequence: {{ _eq: {expected_sequence} }}
                }},
                input: {{
                    last_continued_from_request_id: "{parent_request_id}",
                    continuation_sequence: {next_sequence},
                    updated_at: "{now}"
                }}
            ) {{ _docID }}
        }}"#
    );
    let response = crate::config_client::ConfigAccess::write_local_response(
        node,
        "goal.claim_continuation",
        &mutation,
    )
    .await?;
    Ok(response
        .data
        .as_ref()
        .and_then(|data| data.get("update_Goal"))
        .is_some_and(mutation_returned_rows))
}

pub async fn claim_retry_continuation(
    node: &std::sync::Arc<EmbeddedNode>,
    goal: &GoalDocument,
    parent_request_id: &str,
    retry_count: i64,
    failure: &str,
) -> Result<bool> {
    let doc_id = escape_graphql_string(&goal.doc_id);
    let node_did = escape_graphql_string(&goal.node_did);
    let parent_request_id = escape_graphql_string(parent_request_id);
    let failure = escape_graphql_string(failure);
    let expected_sequence = goal.continuation_sequence();
    let next_sequence = expected_sequence
        .checked_add(1)
        .context("goal continuation sequence exhausted")?;
    let expected_status = escape_graphql_string(&goal.status);
    let now = escape_graphql_string(&Utc::now().to_rfc3339());
    let mutation = format!(
        r#"mutation {{
            update_Goal(
                filter: {{
                    _docID: {{ _eq: "{doc_id}" }},
                    node_did: {{ _eq: "{node_did}" }},
                    status: {{ _eq: "{expected_status}" }},
                    continuation_sequence: {{ _eq: {expected_sequence} }}
                }},
                input: {{
                    last_continued_from_request_id: "{parent_request_id}",
                    continuation_sequence: {next_sequence},
                    infrastructure_retry_count: {retry_count},
                    last_failure: "{failure}",
                    updated_at: "{now}"
                }}
            ) {{ _docID }}
        }}"#
    );
    let response = crate::config_client::ConfigAccess::write_local_response(
        node,
        "goal.claim_retry_continuation",
        &mutation,
    )
    .await?;
    Ok(response
        .data
        .as_ref()
        .and_then(|data| data.get("update_Goal"))
        .is_some_and(mutation_returned_rows))
}

/// Tokens charged against a session's budget. `input` and `output` are the
/// durable `InferenceCall` prompt and completion columns; `total` is their
/// charged sum (`crate::provider_usage::sum_charged_from_persisted_parts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionTokenUsage {
    pub input: u64,
    pub output: u64,
    pub total: u64,
}

pub async fn session_token_usage(
    node: &std::sync::Arc<EmbeddedNode>,
    node_did: &str,
    session_id: &str,
) -> Result<i64> {
    crate::config_client::ConfigAccess::transact_local_readonly(
        node,
        None,
        "goal.session_usage",
        |txn| Box::pin(async move { session_token_usage_in_txn(txn, node_did, session_id).await }),
    )
    .await
}

async fn session_token_usage_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
) -> Result<i64> {
    let usage = session_token_usage_breakdown_in_txn(txn, node_did, session_id).await?;
    Ok(i64::try_from(usage.total).unwrap_or(i64::MAX))
}

pub(crate) async fn session_token_usage_breakdown_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
) -> Result<SessionTokenUsage> {
    crate::toolset::load_session_goal_usage(txn, node_did, session_id)
        .await
        .context("summing goal inference usage")
}

pub async fn refresh_goal_usage(
    node: &std::sync::Arc<EmbeddedNode>,
    goal: &GoalDocument,
) -> Result<i64> {
    let tokens = session_token_usage(node, &goal.node_did, &goal.session_id)
        .await?
        .saturating_sub(goal.token_usage_baseline.unwrap_or_default().max(0))
        .max(0);
    let now = Utc::now();
    let now_string = now.to_rfc3339();
    let active_time = goal.current_active_time_seconds(now);
    let updated_at = escape_graphql_string(&now_string);
    let active_started_field = optional_string_graphql_field(
        "active_started_at",
        goal.parsed_status()
            .is_some_and(GoalStatus::accrues_active_time)
            .then_some(now_string.as_str()),
    );
    let status = goal
        .parsed_status()
        .context("Goal usage snapshot has an unknown status")?;
    update_goal_fields_if_status(
        node,
        goal,
        status,
        &format!(
            "tokens_used: {tokens}, active_time_seconds: {active_time}, {active_started_field} updated_at: \"{updated_at}\""
        ),
    )
    .await?;
    Ok(tokens)
}

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

pub(crate) async fn fence_goal_continuation_claim_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    request: &crate::watcher::AgentRequest,
    now: &str,
) -> Result<bool> {
    if request.caused_by_trigger_kind.as_deref() != Some(GOAL_TRIGGER_KIND) {
        return Ok(true);
    }
    let Some(goal) =
        load_canonical_goal_in_txn(txn, &request.node_did, &request.session_id).await?
    else {
        return Ok(true);
    };
    request_head::retire_stale_assignment_continuations_in_txn(
        txn,
        &goal,
        Some(&request.doc_id),
        now,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn session_token_usage_breakdown_sums_to_the_total() {
        let node = std::sync::Arc::new(crate::test_support::node_builder().build().await.unwrap());
        crate::schema::ensure_runtime_schemas(&node).await.unwrap();
        let owner = "did:key:z6MkTestOwner";
        let request = format!(
            r#"mutation {{ create_AgentRequest(input: {{
                request_id: "usage-request", purpose: "normal", node_did: "{owner}",
                session_id: "usage-session", agent_id: "general", content: "test",
                lifecycle_state: "completed", backend_id: "",
                created_at: "2026-07-15T00:00:00Z", retry_count: 0, max_retries: 0
            }}) {{ _docID }} }}"#
        );
        let response = node.execute(&request).await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        let request_doc_id =
            crate::graphql::single_mutation_document(&response, "create_AgentRequest")
                .unwrap()
                .and_then(|document| document.get("_docID"))
                .and_then(serde_json::Value::as_str)
                .unwrap()
                .to_owned();
        for (seq, prompt, completion) in [(1, 1000, 50), (2, 200, 7)] {
            let call = format!(
                r#"mutation {{ add_InferenceCall(input: {{
                    call_id: "usage-call-{seq}", runtime_instance_id: "goal-test",
                    request_id: "usage-request", request_doc_id: "{request_doc_id}", call_seq: {seq}, backend_id: "backend-test",
                    agent_id: "test", node_did: "{owner}", call_kind: "inference", attempt: 1,
                    call_state: "completed", queued_at: "2026-07-15T00:00:00Z",
                    started_at: "2026-07-15T00:00:00Z", ended_at: "2026-07-15T00:00:01Z",
                    priority: 0, queue_depth_at_enqueue: 0, controller_generation: 1,
                    backend_config_fingerprint: "goal-test",
                    prompt_tokens: {prompt}, completion_tokens: {completion}
                }}) {{ _docID }} }}"#
            );
            let response = node.execute(&call).await;
            assert!(!response.has_errors(), "{:?}", response.errors);
        }
        let usage = crate::config_client::ConfigAccess::transact_local_readonly(
            &node,
            None,
            "goal.session_usage_breakdown_test",
            |txn| {
                Box::pin(async move {
                    session_token_usage_breakdown_in_txn(txn, owner, "usage-session").await
                })
            },
        )
        .await
        .unwrap();
        assert_eq!(
            usage,
            SessionTokenUsage {
                input: 1200,
                output: 57,
                total: 1257
            }
        );
        assert_eq!(usage.input + usage.output, usage.total);
        assert_eq!(
            session_token_usage(&node, owner, "usage-session")
                .await
                .unwrap(),
            1257
        );
    }

    #[tokio::test]
    async fn auto_resume_field_round_trips_through_the_live_schema() {
        let node = std::sync::Arc::new(crate::test_support::node_builder().build().await.unwrap());
        crate::schema::ensure_runtime_schemas(&node).await.unwrap();
        let owner = "did:key:z6MkTestOwner";
        let goal = set_goal(&node, owner, "session", Some("ship"), None, None)
            .await
            .unwrap();
        assert_eq!(goal.auto_resume_at_reset, None);
        let response = node
            .execute(&format!(
                r#"mutation {{ update_Goal(filter: {{_docID: {{_eq: "{}"}}}},
                input: {{auto_resume_at_reset: true}}) {{_docID}} }}"#,
                goal.doc_id
            ))
            .await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        let stored = load_canonical_goal(&node, owner, "session")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.auto_resume_at_reset, Some(true));
    }

    async fn auto_resume_access() -> crate::ConfigAccess {
        let node = std::sync::Arc::new(crate::test_support::node_builder().build().await.unwrap());
        crate::schema::ensure_runtime_schemas(&node).await.unwrap();
        crate::ConfigAccess::local(node)
    }

    #[tokio::test]
    async fn auto_resume_operator_setter_stores_it() {
        let access = auto_resume_access().await;
        let owner = "did:key:z6MkTestOwner";
        let created =
            set_goal_from_access(&access, owner, "s", Some("ship"), None, None, Some(true))
                .await
                .unwrap();
        assert_eq!(created.auto_resume_at_reset, Some(true));
        let off = set_goal_from_access(&access, owner, "s", None, None, None, Some(false))
            .await
            .unwrap();
        assert_eq!(off.auto_resume_at_reset, Some(false));
    }

    #[tokio::test]
    async fn auto_resume_none_and_set_goal_keep_the_stored_value() {
        let access = auto_resume_access().await;
        let node = access.local_node().expect("local test access");
        let owner = "did:key:z6MkTestOwner";
        set_goal_from_access(&access, owner, "s", Some("ship"), None, None, Some(true))
            .await
            .unwrap();
        let kept = set_goal_from_access(
            &access,
            owner,
            "s",
            None,
            Some(GoalStatus::Paused),
            Some(Some(10)),
            None,
        )
        .await
        .unwrap();
        assert_eq!(kept.auto_resume_at_reset, Some(true));
        let unchanged = set_goal(node, owner, "s", Some("ship more"), None, None)
            .await
            .unwrap();
        assert_eq!(unchanged.auto_resume_at_reset, Some(true));
    }
}
