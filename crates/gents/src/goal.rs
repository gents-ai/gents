//! Durable, session-scoped autonomous goals.

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use defra_node::{EmbeddedNode, QueryResponse};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use serde::{Deserialize, Serialize};

use crate::graphql::{
    escape_graphql_string, graphql_mutation_with_transaction_retry, graphql_with_transaction_retry,
    rows,
};

mod claimed_publication;
mod operator_resume;
mod request_head;
pub(crate) use claimed_publication::publish_claimed_continuation;
pub use operator_resume::{resume_goal_request, GoalResumeReceipt};
pub(crate) use request_head::{goal_session_is_idle, latest_goal_request};

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

/// Executable mirror of `GoalAutomation.decideCreate`.
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

/// Executable mirror of `GoalAutomation.submissionStep`.
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

/// Executable mirror of `GoalAutomation.continuationStep`.
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
    agent_did
    objective
    status
    token_budget
    tokens_used
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
    /// Executable mirror of `Goals.step?` in `proofs/Proofs/Goals.lean`.
    pub fn step(self, action: GoalAction) -> Option<Self> {
        match action {
            GoalAction::Pause if self.status == GoalStatus::Active => Some(Self {
                status: GoalStatus::Paused,
                ..self
            }),
            GoalAction::Resume
                if matches!(
                    self.status,
                    GoalStatus::Paused | GoalStatus::Blocked | GoalStatus::UsageLimited
                ) =>
            {
                Some(Self {
                    status: GoalStatus::Active,
                    blocked_audits: 0,
                    ..self
                })
            }
            GoalAction::Complete if self.status != GoalStatus::Complete => Some(Self {
                status: GoalStatus::Complete,
                wrapup_completed: true,
                ..self
            }),
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

/// Executable mirror of `Goals.decide` in `proofs/Proofs/Goals.lean`.
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
                if infrastructure_retries.max(0) < MAX_INFRASTRUCTURE_RETRIES {
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
                    GoalRequestTerminal::Failed
                    | GoalRequestTerminal::Dead
                    | GoalRequestTerminal::Interrupted
                    | GoalRequestTerminal::Superseded => {
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
        | GoalStatus::Complete => GoalDecision::None,
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
    pub agent_did: String,
    #[serde(default)]
    pub objective: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub token_budget: Option<i64>,
    #[serde(default)]
    pub tokens_used: Option<i64>,
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

#[derive(Debug, Clone, Serialize)]
pub struct GoalSnapshot {
    pub goal_id: String,
    pub session_id: String,
    pub agent_did: String,
    pub objective: String,
    pub status: String,
    pub token_budget: Option<i64>,
    pub tokens_used: i64,
    pub active_time_seconds: i64,
    pub consecutive_blocked_audits: i64,
    pub continuation_sequence: i64,
    pub wrapup_requested: bool,
    pub wrapup_completed: bool,
    pub last_blocked_reason: Option<String>,
    pub last_failure: Option<String>,
    pub completion_evidence: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

impl GoalSnapshot {
    pub fn from_document(goal: &GoalDocument, now: DateTime<Utc>) -> Self {
        Self {
            goal_id: goal.goal_id.clone(),
            session_id: goal.session_id.clone(),
            agent_did: goal.agent_did.clone(),
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
}

pub fn deterministic_goal_id(agent_did: &str, session_id: &str) -> String {
    format!("{}:{agent_did}:{session_id}", agent_did.len())
}

pub fn deterministic_goal_creation_key(agent_did: &str, session_id: &str) -> String {
    format!("goal-create:{}:{agent_did}:{session_id}", agent_did.len())
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
    pub agent_did: String,
    #[serde(default)]
    pub behavior_id: String,
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

/// Stable identity for one Task delivery that declares a durable Goal.
///
/// Length-prefixing matches `GoalAutomation.taskFireIdentity` and prevents
/// delimiter aliases without introducing a second persisted identity scheme.
#[doc(hidden)]
pub fn task_goal_fire_identity(
    agent_did: &str,
    task_id: &str,
    fire_key: &str,
) -> TaskGoalFireIdentity {
    let scope = format!(
        "{}:{agent_did}:{}:{task_id}:{}:{fire_key}",
        agent_did.chars().count(),
        task_id.chars().count(),
        fire_key.chars().count()
    );
    TaskGoalFireIdentity {
        session_id: format!("task-goal-session:{scope}"),
        request_id: format!("task-goal-request:{scope}"),
        retry_key: format!("task-goal-retry:{scope}"),
    }
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
    agent_did: &str,
    session_id: &str,
) -> Result<Option<GoalCreationFingerprint>> {
    let key = escape_graphql_string(&deterministic_goal_creation_key(agent_did, session_id));
    let query = format!(
        r#"{{ GoalCreationClaim(filter: {{ creation_key: {{ _eq: "{key}" }} }}, limit: 2) {{
            agent_did session_id objective token_budget
        }} }}"#
    );
    let response = graphql_with_transaction_retry(node, &query, "load goal creation claim").await?;
    #[derive(Deserialize)]
    struct ClaimRow {
        agent_did: String,
        session_id: String,
        objective: String,
        token_budget: Option<i64>,
    }
    let claims: Vec<ClaimRow> = rows(&response, "GoalCreationClaim")?;
    anyhow::ensure!(
        claims.len() <= 1,
        "goal creation key resolved to multiple claims"
    );
    Ok(claims
        .into_iter()
        .next()
        .map(|claim| GoalCreationFingerprint {
            owner: claim.agent_did,
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

/// Reconcile the immutable creation claim and Goal inside an existing
/// transaction.
///
/// Model creation and atomic first-request submission use this seam. Operator
/// `set_goal` remains a mutable upsert and intentionally does not mint an
/// immutable creation claim. Ownership, objective/budget conflict semantics,
/// deterministic identity, and physical-create arbitration stay single-owned
/// for every create-only path.
async fn stage_goal_and_claim(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    agent_did: &str,
    session_id: &str,
    objective: &str,
    token_budget: Option<i64>,
    initial_state: GoalState,
    now: &str,
) -> Result<StagedGoal> {
    let create_request = GoalCreateRequest {
        caller: agent_did.to_string(),
        current_session: session_id.to_string(),
        requested_owner: agent_did.to_string(),
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
    let goal_id = deterministic_goal_id(agent_did, session_id);
    let creation_key = deterministic_goal_creation_key(agent_did, session_id);
    let escaped_did = escape_graphql_string(agent_did);
    let escaped_session = escape_graphql_string(session_id);
    let response = txn
        .execute(&format!(
            r#"{{
                Goal(filter: {{
                    agent_did: {{ _eq: "{escaped_did}" }},
                    session_id: {{ _eq: "{escaped_session}" }}
                }}) {{ {GOAL_FIELDS} }}
                GoalCreationClaim(filter: {{ creation_key: {{ _eq: "{}" }} }}) {{
                    goal_id agent_did session_id objective token_budget
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
                .get("agent_did")
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
                agent_did: "{escaped_did}", session_id: "{escaped_session}",
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
    let active_started_field = optional_string_graphql_field(
        "active_started_at",
        status.accrues_active_time().then_some(now),
    );
    txn.execute(&format!(
        r#"mutation {{ create_Goal(input: {{
            goal_id: "{}", creation_key: "{}",
            session_id: "{escaped_session}", agent_did: "{escaped_did}",
            objective: "{}", status: "{}", {}
            tokens_used: 0, active_time_seconds: 0, {active_started_field}
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
    Ok(StagedGoal {
        disposition: StagedGoalDisposition::Created,
        goal: GoalDocument {
            doc_id: String::new(),
            goal_id,
            session_id: session_id.to_string(),
            agent_did: agent_did.to_string(),
            objective: objective.to_string(),
            status: status.as_str().to_string(),
            token_budget,
            tokens_used: Some(0),
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
            created_at: Some(now.to_string()),
            updated_at: Some(now.to_string()),
        },
    })
}

/// Create the current principal/session goal without granting update semantics.
///
/// Ownership is deliberately not accepted as model input: both values come
/// from the authenticated session hook. Exact retries return `Idempotent` and
/// never reset lifecycle fields; a different immutable fingerprint is a typed
/// conflict. The unique creation key prevents local concurrent retries from
/// producing physical twins, while canonical scope checks retain safe behavior
/// for twins received through P2P replication.
pub async fn create_goal_for_session(
    node: &EmbeddedNode,
    agent_did: &str,
    session_id: &str,
    objective: &str,
    token_budget: Option<i64>,
) -> std::result::Result<CreateGoalForSessionOutcome, CreateGoalForSessionError> {
    let create_request = GoalCreateRequest {
        caller: agent_did.to_string(),
        current_session: session_id.to_string(),
        requested_owner: agent_did.to_string(),
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

    let txn = crate::config_client::ConfigApplyTxn::begin_local(node, None)
        .await
        .map_err(anyhow::Error::from)?;
    let result = stage_goal_and_claim(
        &txn,
        agent_did,
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
    )
    .await;

    match result {
        Ok(outcome) => {
            if let Err(commit_error) = txn.commit().await {
                if let Some(existing) = load_canonical_goal(node, agent_did, session_id)
                    .await
                    .map_err(CreateGoalForSessionError::Storage)?
                {
                    if existing.objective.trim() == objective
                        && existing.token_budget == token_budget
                        && load_goal_creation_claim_fingerprint(node, agent_did, session_id)
                            .await
                            .map_err(CreateGoalForSessionError::Storage)?
                            == Some(requested_fingerprint.clone())
                    {
                        return Ok(CreateGoalForSessionOutcome::Idempotent(existing));
                    }
                    return Err(CreateGoalForSessionError::Conflict);
                }
                return Err(CreateGoalForSessionError::Storage(commit_error));
            }
            let goal = load_canonical_goal(node, agent_did, session_id)
                .await
                .map_err(CreateGoalForSessionError::Storage)?
                .context("committed Goal row not found")
                .map_err(CreateGoalForSessionError::Storage)?;
            Ok(match outcome.disposition {
                StagedGoalDisposition::Created => CreateGoalForSessionOutcome::Created(goal),
                StagedGoalDisposition::Idempotent => CreateGoalForSessionOutcome::Idempotent(goal),
            })
        }
        Err(error) => {
            let _ = txn.discard().await;
            if let Some(existing) = load_canonical_goal(node, agent_did, session_id)
                .await
                .map_err(CreateGoalForSessionError::Storage)?
            {
                if existing.objective.trim() == objective
                    && existing.token_budget == token_budget
                    && load_goal_creation_claim_fingerprint(node, agent_did, session_id)
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
    request_id agent_did requester_did behavior_id session_id
    retry_parent_request retry_parent_request_doc_id retry_root_request retry_key
    content temperature top_p top_k seed max_tokens max_total_tokens metadata
    execution_origin caused_by_trigger_id caused_by_trigger_doc_id
    caused_by_trigger_kind caused_by_correlation caused_by_trigger_context
    caused_by_source_doc_id retry_count max_retries valid_until subagent_depth
    caused_by_parent_request_id caused_by_parent_request_doc_id
    caused_by_parent_tool_call_id caused_by_parent_tool_call_doc_id
    workspace_id workspace_authority workspace_owner_deployment_id workspace_seal_hash
    admission_kind admission_signer_did enrollment_request_id
    enrollment_request_digest enrollment_admin_did
    enrollment_authorization_sequence enrollment_authorization_expires_at
    runtime_issuer_did runtime_source_request_id runtime_source_kind
    runtime_bridge_author_did
"#;

/// Stable logical request identity for goal-backed submission retries.
///
/// `created_at`, the admission signature, backend assignment, and lifecycle
/// fields are deliberately excluded: they are either regenerated on an exact
/// CLI retry or are runtime-mutated after publication. Every other signed
/// semantic field is compared before an existing retry key is accepted.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub(crate) struct GoalBackedRequestFingerprint {
    request_id: String,
    agent_did: String,
    requester_did: String,
    behavior_id: Option<String>,
    session_id: String,
    retry_parent_request: Option<String>,
    retry_parent_request_doc_id: Option<String>,
    retry_root_request: Option<String>,
    retry_key: Option<String>,
    content: String,
    temperature: Option<f64>,
    top_p: Option<f64>,
    top_k: Option<i64>,
    seed: Option<i64>,
    max_tokens: Option<i64>,
    max_total_tokens: Option<i64>,
    metadata: Option<String>,
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
    subagent_depth: i64,
    caused_by_parent_request_id: Option<String>,
    caused_by_parent_request_doc_id: Option<String>,
    caused_by_parent_tool_call_id: Option<String>,
    caused_by_parent_tool_call_doc_id: Option<String>,
    workspace_id: Option<String>,
    workspace_authority: Option<String>,
    workspace_owner_deployment_id: Option<String>,
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
    runtime_bridge_author_did: Option<String>,
}

impl GoalBackedRequestFingerprint {
    pub(crate) fn from_create(
        request: &gents_protocol::request_admission::AgentRequestCreate,
    ) -> Result<Self> {
        // Compile-time fence: adding a new request field requires an explicit
        // decision about whether it belongs in the immutable retry contract.
        let gents_protocol::request_admission::AgentRequestCreate {
            request_id: _,
            agent_did: _,
            requester_did: _,
            behavior_id: _,
            session_id: _,
            retry_parent_request: _,
            retry_parent_request_doc_id: _,
            retry_root_request: _,
            retry_key: _,
            content: _,
            temperature: _,
            top_p: _,
            top_k: _,
            seed: _,
            max_tokens: _,
            max_total_tokens: _,
            metadata: _,
            backend_id: _,
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
            subagent_depth: _,
            caused_by_parent_request_id: _,
            caused_by_parent_request_doc_id: _,
            caused_by_parent_tool_call_id: _,
            caused_by_parent_tool_call_doc_id: _,
            workspace_id: _,
            workspace_authority: _,
            workspace_owner_deployment_id: _,
            workspace_seal_hash: _,
            initial_lifecycle_state: _,
            admission: _,
        } = request;
        Ok(Self {
            request_id: request.request_id.clone(),
            agent_did: request.agent_did.clone(),
            requester_did: request.requester_did.clone(),
            behavior_id: request.behavior_id.clone(),
            session_id: request.session_id.clone(),
            retry_parent_request: request.retry_parent_request.clone(),
            retry_parent_request_doc_id: request.retry_parent_request_doc_id.clone(),
            retry_root_request: request.retry_root_request.clone(),
            retry_key: request.retry_key.clone(),
            content: request.content.clone(),
            temperature: request.temperature,
            top_p: request.top_p,
            top_k: request.top_k,
            seed: request.seed,
            max_tokens: request.max_tokens,
            max_total_tokens: request.max_total_tokens,
            metadata: request.metadata.clone(),
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
            subagent_depth: i64::from(request.subagent_depth),
            caused_by_parent_request_id: request.caused_by_parent_request_id.clone(),
            caused_by_parent_request_doc_id: request.caused_by_parent_request_doc_id.clone(),
            caused_by_parent_tool_call_id: request.caused_by_parent_tool_call_id.clone(),
            caused_by_parent_tool_call_doc_id: request.caused_by_parent_tool_call_doc_id.clone(),
            workspace_id: request.workspace_id.clone(),
            workspace_authority: request.workspace_authority.clone(),
            workspace_owner_deployment_id: request.workspace_owner_deployment_id.clone(),
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
            runtime_bridge_author_did: request.admission.runtime_bridge_author_did.clone(),
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
    agent_did: &str,
    session_id: &str,
    objective: &str,
    token_budget: Option<i64>,
    request: &gents_protocol::request_admission::AgentRequestCreate,
) -> Result<StagedGoalBackedRequest> {
    let objective = objective.trim();
    anyhow::ensure!(!objective.is_empty(), "goal objective must be non-empty");
    anyhow::ensure!(
        token_budget.is_none_or(|budget| budget > 0),
        "goal token budget must be positive"
    );
    anyhow::ensure!(request.agent_did == agent_did, "goal/request DID mismatch");
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
        agent_did,
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
                    AgentRequest(filter: {{ retry_key: {{ _eq: "{}" }} }}) {{
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
    let request_fields = request.graphql_input_fields().map_err(anyhow::Error::msg)?;
    txn.execute(&format!(
        "mutation {{ create_AgentRequest(input: {{ {request_fields} }}) {{ _docID }} }}"
    ))
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

/// Atomically establish the durable goal and its first pending request.
///
/// This is the reusable graph/chat composition boundary. The signed request
/// must already be bound to the same principal/session and carry a stable,
/// unique `retry_key`. No request is externally visible until the goal claim,
/// goal, and request commit together.
pub async fn submit_goal_backed_request(
    access: &crate::ConfigAccess,
    agent_did: &str,
    session_id: &str,
    objective: &str,
    token_budget: Option<i64>,
    request: &gents_protocol::request_admission::AgentRequestCreate,
) -> Result<GoalBackedRequestDisposition> {
    let txn = access.begin_apply_txn().await?;
    let result = stage_goal_backed_request(
        &txn,
        agent_did,
        session_id,
        objective,
        token_budget,
        request,
    )
    .await
    .and_then(authorize_goal_submission_commit);
    match result {
        Ok(disposition) => {
            if let Err(commit_error) = txn.commit().await {
                let recovery = load_goal_backed_request_by_retry_key_from_access(
                    access,
                    agent_did,
                    session_id,
                    objective,
                    token_budget,
                    request,
                )
                .await;
                return resolve_ambiguous_goal_submission_commit(commit_error, recovery);
            }
            Ok(disposition)
        }
        Err(error) => {
            let _ = txn.discard().await;
            Err(error)
        }
    }
}

/// The `Goal`/`GoalCreationClaim` selection shared by
/// `goal_backed_request_recovery_query` (combined with an `AgentRequest`
/// selection in one round trip) and `goal_creation_claim_recovery_query`
/// (issued alone, after a separate `AgentRequest` retry-key match) — same
/// filters and fields, so this selection text exists exactly once.
fn goal_creation_claim_selection(agent_did: &str, session_id: &str) -> String {
    format!(
        r#"Goal(
            filter: {{ agent_did: {{ _eq: "{}" }}, session_id: {{ _eq: "{}" }} }},
            order: [{{ created_at: ASC }}, {{ goal_id: ASC }}]
        ) {{ {GOAL_FIELDS} }}
        GoalCreationClaim(filter: {{ creation_key: {{ _eq: "{}" }} }}, limit: 2) {{
            goal_id agent_did session_id objective token_budget
        }}"#,
        escape_graphql_string(agent_did),
        escape_graphql_string(session_id),
        escape_graphql_string(&deterministic_goal_creation_key(agent_did, session_id)),
    )
}

fn goal_backed_request_recovery_query(
    agent_did: &str,
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
        goal_creation_claim_selection(agent_did, session_id),
    ))
}

/// Decode the `AgentRequest` rows from a retry-key lookup, matching the
/// unique row (if any) against `expected`. Fails closed if the retry key is
/// not unique or if the persisted row's immutable fields differ from
/// `expected`. Returns the row's document id if a matching row exists,
/// `None` if none exists yet.
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

/// Query one `AgentRequest` by `retry_key`, decode it into a
/// `GoalBackedRequestFingerprint`, and compare it against `expected`.
/// Shared by goal creation/continuation recovery, both of which query an
/// `AgentRequest` by its unique `retry_key` and need the same fail-closed
/// uniqueness and immutable-field checks before trusting an existing row.
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

/// Verify that the durable `Goal`/`GoalCreationClaim` rows in `response`
/// match the goal a goal-backed request is meant to belong to. Separate
/// from the `AgentRequest` retry-key match above: an existing request row
/// only proves the request was staged, not that it landed on the goal this
/// caller expects.
fn verify_goal_creation_claim_cross_check(
    response: &serde_json::Value,
    agent_did: &str,
    session_id: &str,
    objective: &str,
    token_budget: Option<i64>,
) -> Result<()> {
    let expected_claim = GoalCreationFingerprint {
        owner: agent_did.to_string(),
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
            .get("agent_did")
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
        goal.agent_did == agent_did
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
    agent_did: &str,
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
        agent_did,
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
    agent_did: &str,
    session_id: &str,
    objective: &str,
    token_budget: Option<i64>,
    request: &gents_protocol::request_admission::AgentRequestCreate,
) -> Result<Option<crate::lifecycle::materialize::EnqueuedAgentRequest>> {
    let query = goal_backed_request_recovery_query(agent_did, session_id, request)?;
    let response = access.execute(&query).await?;
    decode_goal_backed_request_recovery(
        &response,
        agent_did,
        session_id,
        objective,
        token_budget,
        request,
    )
}

fn goal_creation_claim_recovery_query(agent_did: &str, session_id: &str) -> String {
    format!(
        "{{ {} }}",
        goal_creation_claim_selection(agent_did, session_id)
    )
}

/// Recover a goal-backed request by its retry key after an ambiguous local
/// commit, using two round trips instead of `load_goal_backed_request_by_retry_key_from_access`'s
/// one: first the shared `load_agent_request_by_retry_key` AgentRequest
/// match, then — only once that matches — the `Goal`/`GoalCreationClaim`
/// cross-check. This is safe to split because `stage_goal_backed_request`
/// commits the goal, its creation claim, and the request in one durable
/// transaction: if the `AgentRequest` row is visible with this retry key,
/// the `Goal` and `GoalCreationClaim` rows committed alongside it are
/// visible too, so there is no window where the first query could observe
/// a match the second query then fails to find.
async fn load_goal_backed_request_by_retry_key(
    node: &EmbeddedNode,
    agent_did: &str,
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
    let query = goal_creation_claim_recovery_query(agent_did, session_id);
    let response =
        graphql_with_transaction_retry(node, &query, "load goal-backed request creation claim")
            .await?;
    let response = serde_json::json!({
        "data": response.data.unwrap_or(serde_json::Value::Null),
    });
    verify_goal_creation_claim_cross_check(
        &response,
        agent_did,
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

/// Embedded-runtime counterpart of [`submit_goal_backed_request`].
///
/// Trigger materialization uses this to publish a Task-declared Goal and its
/// first request in one transaction, then recover the exact request binding by
/// its unique retry key if commit acknowledgement is ambiguous.
pub async fn submit_goal_backed_request_local(
    node: &EmbeddedNode,
    agent_did: &str,
    session_id: &str,
    objective: &str,
    token_budget: Option<i64>,
    request: &gents_protocol::request_admission::AgentRequestCreate,
) -> Result<crate::lifecycle::EnqueuedAgentRequest> {
    let txn = crate::config_client::ConfigApplyTxn::begin_local(node, None).await?;
    let staged = stage_goal_backed_request(
        &txn,
        agent_did,
        session_id,
        objective,
        token_budget,
        request,
    )
    .await
    .and_then(authorize_goal_submission_commit);
    match staged {
        Ok(_) => {
            if let Err(commit_error) = txn.commit().await {
                if let Some(recovered) = load_goal_backed_request_by_retry_key(
                    node,
                    agent_did,
                    session_id,
                    objective,
                    token_budget,
                    request,
                )
                .await?
                {
                    return Ok(recovered);
                }
                return Err(commit_error);
            }
        }
        Err(error) => {
            let _ = txn.discard().await;
            if let Some(recovered) = load_goal_backed_request_by_retry_key(
                node,
                agent_did,
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
        agent_did,
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
    agent_did: &str,
    session_id: &str,
) -> Result<Vec<GoalDocument>> {
    let agent_did = escape_graphql_string(agent_did);
    let session_id = escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            Goal(
                filter: {{
                    agent_did: {{ _eq: "{agent_did}" }},
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
    agent_did: &str,
    session_id: &str,
) -> Result<Option<GoalDocument>> {
    Ok(load_goals_for_session(node, agent_did, session_id)
        .await?
        .into_iter()
        .next())
}

pub async fn load_goal_by_id(
    node: &EmbeddedNode,
    agent_did: &str,
    goal_id: &str,
) -> Result<Option<GoalDocument>> {
    let agent_did = escape_graphql_string(agent_did);
    let goal_id = escape_graphql_string(goal_id);
    let query = format!(
        r#"{{
            Goal(
                filter: {{ agent_did: {{ _eq: "{agent_did}" }}, goal_id: {{ _eq: "{goal_id}" }} }},
                order: [{{ created_at: ASC }}, {{ goal_id: ASC }}]
            ) {{ {GOAL_FIELDS} }}
        }}"#
    );
    let mut goals = decode_goal_rows(node, &query).await?;
    sort_goals_canonical(&mut goals);
    Ok(goals.into_iter().next())
}

fn sort_goals_canonical(goals: &mut [GoalDocument]) {
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
    node: &EmbeddedNode,
    agent_did: &str,
    session_id: &str,
    objective: Option<&str>,
    status: Option<GoalStatus>,
    token_budget: Option<Option<i64>>,
) -> Result<GoalDocument> {
    let txn = crate::config_client::ConfigApplyTxn::begin_local(node, None).await?;
    finish_set_goal(txn, agent_did, session_id, objective, status, token_budget).await
}

/// Configure a goal through the same transactional policy for local and HTTP access.
pub async fn set_goal_from_access(
    access: &crate::ConfigAccess,
    agent_did: &str,
    session_id: &str,
    objective: Option<&str>,
    status: Option<GoalStatus>,
    token_budget: Option<Option<i64>>,
) -> Result<GoalDocument> {
    let txn = access.begin_apply_txn().await?;
    finish_set_goal(txn, agent_did, session_id, objective, status, token_budget).await
}

async fn finish_set_goal(
    txn: crate::config_client::ConfigApplyTxn<'_>,
    agent_did: &str,
    session_id: &str,
    objective: Option<&str>,
    status: Option<GoalStatus>,
    token_budget: Option<Option<i64>>,
) -> Result<GoalDocument> {
    match set_goal_in_txn(&txn, agent_did, session_id, objective, status, token_budget).await {
        Ok(goal) => {
            txn.commit().await?;
            Ok(goal)
        }
        Err(error) => {
            let _ = txn.discard().await;
            Err(error)
        }
    }
}

async fn load_canonical_goal_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    agent_did: &str,
    session_id: &str,
) -> Result<Option<GoalDocument>> {
    let agent_did = escape_graphql_string(agent_did);
    let session_id = escape_graphql_string(session_id);
    let response = txn
        .execute(&format!(
            r#"{{ Goal(filter: {{
        agent_did: {{ _eq: "{agent_did}" }}, session_id: {{ _eq: "{session_id}" }}
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

async fn set_goal_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    agent_did: &str,
    session_id: &str,
    objective: Option<&str>,
    status: Option<GoalStatus>,
    token_budget: Option<Option<i64>>,
) -> Result<GoalDocument> {
    let existing = load_canonical_goal_in_txn(txn, agent_did, session_id).await?;
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
        let escaped_agent_did = escape_graphql_string(agent_did);
        let objective = escape_graphql_string(&objective);
        let status = post.status.as_str();
        let now = escape_graphql_string(&now_string);
        let mutation = format!(
            r#"mutation {{
                update_Goal(
                    filter: {{ _docID: {{ _eq: "{doc_id}" }}, agent_did: {{ _eq: "{escaped_agent_did}" }} }},
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
        return load_canonical_goal_in_txn(txn, agent_did, session_id)
            .await?
            .context("updated Goal row disappeared");
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
    // Operator-owned goals intentionally remain mutable upserts and therefore
    // do not mint the immutable model/submission creation claim. A later
    // create-only call may adopt a matching operator goal transactionally;
    // attaching the claim here would become stale as soon as `goal set`
    // changes the objective or budget.
    let goal_id = deterministic_goal_id(agent_did, session_id);
    let escaped_goal_id = escape_graphql_string(&goal_id);
    let escaped_agent_did = escape_graphql_string(agent_did);
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_objective = escape_graphql_string(&objective);
    let escaped_now = escape_graphql_string(&now_string);
    let budget_field = optional_int_graphql_field("token_budget", budget);
    let active_started_field = optional_string_graphql_field(
        "active_started_at",
        status.accrues_active_time().then_some(now_string.as_str()),
    );
    let mutation = format!(
        r#"mutation {{
            create_Goal(input: {{
                goal_id: "{escaped_goal_id}",
                session_id: "{escaped_session_id}",
                agent_did: "{escaped_agent_did}",
                objective: "{escaped_objective}",
                status: "{status}",
                {budget_field}
                tokens_used: 0,
                active_time_seconds: 0,
                {active_started_field}
                consecutive_blocked_audits: {blocked_audits},
                continuation_sequence: 0,
                wrapup_requested: {wrapup_requested},
                wrapup_completed: {wrapup_completed},
                infrastructure_retry_count: 0,
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
    load_canonical_goal_in_txn(txn, agent_did, session_id)
        .await?
        .context("created Goal row not found")
}

pub async fn delete_goal(node: &EmbeddedNode, goal: &GoalDocument) -> Result<bool> {
    let doc_id = escape_graphql_string(&goal.doc_id);
    let agent_did = escape_graphql_string(&goal.agent_did);
    let mutation = format!(
        r#"mutation {{
            delete_Goal(filter: {{ _docID: {{ _eq: "{doc_id}" }}, agent_did: {{ _eq: "{agent_did}" }} }}) {{ _docID }}
        }}"#
    );
    let response = execute_goal_mutation_response(node, &mutation, "delete goal").await?;
    Ok(response
        .data
        .as_ref()
        .and_then(|data| data.get("delete_Goal"))
        .is_some_and(mutation_returned_rows))
}

/// Delete every replicated twin for an agent/session goal. Goal identity is
/// intentionally not unique at the schema layer because DefraDB unique indexes
/// do not provide a distributed P2P uniqueness guarantee; clear must therefore
/// sweep the complete ownership scope rather than delete only the canonical row.
pub async fn delete_goals_for_session(
    node: &EmbeddedNode,
    agent_did: &str,
    session_id: &str,
) -> Result<usize> {
    let agent_did = escape_graphql_string(agent_did);
    let session_id = escape_graphql_string(session_id);
    let txn = crate::config_client::ConfigApplyTxn::begin_local(node, None).await?;
    let result = async {
        let response = txn
            .execute(&format!(
                r#"mutation {{
            delete_Goal(filter: {{
                agent_did: {{ _eq: "{agent_did}" }},
                session_id: {{ _eq: "{session_id}" }}
            }}) {{ _docID }}
        }}"#
            ))
            .await?;
        txn.execute(&format!(
            r#"mutation {{
                delete_GoalCreationClaim(filter: {{
                    agent_did: {{ _eq: "{agent_did}" }},
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
    }
    .await;
    match result {
        Ok(count) => {
            txn.commit().await?;
            Ok(count)
        }
        Err(error) => {
            let _ = txn.discard().await;
            Err(error)
        }
    }
}

/// Apply controller-owned fields only while both the observed status and
/// continuation sequence remain current. A resumed goal may have the same
/// status as an older snapshot, but belongs to a newer continuation.
pub async fn update_goal_fields_if_status(
    node: &EmbeddedNode,
    goal: &GoalDocument,
    expected_status: GoalStatus,
    fields: &str,
) -> Result<bool> {
    let doc_id = escape_graphql_string(&goal.doc_id);
    let agent_did = escape_graphql_string(&goal.agent_did);
    let expected_status = escape_graphql_string(expected_status.as_str());
    let expected_sequence = goal.continuation_sequence();
    let mutation = format!(
        r#"mutation {{
            update_Goal(
                filter: {{
                    _docID: {{ _eq: "{doc_id}" }},
                    agent_did: {{ _eq: "{agent_did}" }},
                    status: {{ _eq: "{expected_status}" }},
                    continuation_sequence: {{ _eq: {expected_sequence} }}
                }},
                input: {{ {fields} }}
            ) {{ _docID }}
        }}"#
    );
    let response = execute_goal_mutation_response(
        node,
        &mutation,
        "conditionally update goal controller fields",
    )
    .await?;
    Ok(response
        .data
        .as_ref()
        .and_then(|data| data.get("update_Goal"))
        .is_some_and(mutation_returned_rows))
}

pub async fn claim_continuation(
    node: &EmbeddedNode,
    goal: &GoalDocument,
    parent_request_id: &str,
) -> Result<bool> {
    let doc_id = escape_graphql_string(&goal.doc_id);
    let agent_did = escape_graphql_string(&goal.agent_did);
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
                    agent_did: {{ _eq: "{agent_did}" }},
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
    let response =
        graphql_mutation_with_transaction_retry(node, &mutation, "claim goal continuation").await?;
    Ok(response
        .data
        .as_ref()
        .and_then(|data| data.get("update_Goal"))
        .is_some_and(mutation_returned_rows))
}

/// Atomically charge an infrastructure retry and claim its continuation.
/// There is intentionally no durable phase in which a parent is charged but
/// remains unclaimed, because recovery would otherwise charge it again.
pub async fn claim_retry_continuation(
    node: &EmbeddedNode,
    goal: &GoalDocument,
    parent_request_id: &str,
    retry_count: i64,
    failure: &str,
) -> Result<bool> {
    let doc_id = escape_graphql_string(&goal.doc_id);
    let agent_did = escape_graphql_string(&goal.agent_did);
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
                    agent_did: {{ _eq: "{agent_did}" }},
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
    let response = graphql_mutation_with_transaction_retry(
        node,
        &mutation,
        "atomically charge and claim goal retry",
    )
    .await?;
    Ok(response
        .data
        .as_ref()
        .and_then(|data| data.get("update_Goal"))
        .is_some_and(mutation_returned_rows))
}

/// Sum of tokens charged against the session's budget: the charged total,
/// same as the request ledger (`crate::provider_usage::sum_charged_from_persisted_parts`).
pub async fn session_token_usage(
    node: &EmbeddedNode,
    agent_did: &str,
    session_id: &str,
) -> Result<i64> {
    let agent_did = escape_graphql_string(agent_did);
    let session_id = escape_graphql_string(session_id);
    let request_query = format!(
        r#"{{
            AgentRequest(filter: {{ agent_did: {{ _eq: "{agent_did}" }}, session_id: {{ _eq: "{session_id}" }} }}) {{ request_id }}
        }}"#
    );
    let response =
        graphql_with_transaction_retry(node, &request_query, "query goal session requests").await?;
    let mut request_ids = rows::<serde_json::Value>(&response, "AgentRequest")?
        .into_iter()
        .filter_map(|row| {
            row.get("request_id")
                .and_then(|value| value.as_str())
                .map(ToOwned::to_owned)
        })
        .collect::<Vec<_>>();
    request_ids.sort();
    request_ids.dedup();
    if request_ids.is_empty() {
        return Ok(0);
    }
    let request_ids = request_ids
        .iter()
        .map(|value| format!(r#""{}""#, escape_graphql_string(value)))
        .collect::<Vec<_>>()
        .join(", ");
    let query = format!(
        r#"{{
            InferenceCall(
                filter: {{ agent_did: {{ _eq: "{agent_did}" }}, request_id: {{ _in: [{request_ids}] }} }}
            ) {{ prompt_tokens completion_tokens }}
        }}"#
    );
    #[derive(Deserialize)]
    struct UsageRow {
        #[serde(default)]
        prompt_tokens: Option<i64>,
        #[serde(default)]
        completion_tokens: Option<i64>,
    }
    let response =
        graphql_with_transaction_retry(node, &query, "query goal inference usage").await?;
    let usage_rows: Vec<UsageRow> =
        rows(&response, "InferenceCall").context("decoding goal inference usage")?;
    let charged = crate::provider_usage::sum_charged_from_persisted_parts(
        usage_rows
            .into_iter()
            .map(|row| (row.prompt_tokens, row.completion_tokens)),
    )
    .context("summing goal inference usage")?;
    Ok(i64::try_from(charged).unwrap_or(i64::MAX))
}

pub async fn refresh_goal_usage(node: &EmbeddedNode, goal: &GoalDocument) -> Result<i64> {
    let tokens = session_token_usage(node, &goal.agent_did, &goal.session_id).await?;
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

async fn execute_goal_mutation_response(
    node: &EmbeddedNode,
    mutation: &str,
    label: &str,
) -> Result<QueryResponse> {
    graphql_mutation_with_transaction_retry(node, mutation, label).await
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn goal_status_vocabulary_is_stable() {
        for status in [
            GoalStatus::Active,
            GoalStatus::Paused,
            GoalStatus::Blocked,
            GoalStatus::UsageLimited,
            GoalStatus::BudgetLimited,
            GoalStatus::Complete,
        ] {
            assert_eq!(GoalStatus::parse(status.as_str()), Some(status));
        }
    }

    #[test]
    fn deterministic_id_separates_ambiguous_did_session_pairs() {
        assert_ne!(
            deterministic_goal_id("did:a", "bc"),
            deterministic_goal_id("did:ab", "c")
        );
    }

    #[test]
    fn ambiguous_goal_submission_commit_recovers_only_an_exact_persisted_pair() {
        let recovered = crate::lifecycle::materialize::EnqueuedAgentRequest {
            doc_id: "doc-1".to_string(),
            request_id: "request-1".to_string(),
            session_id: "session-1".to_string(),
        };
        assert_eq!(
            resolve_ambiguous_goal_submission_commit(
                anyhow::anyhow!("commit acknowledgement lost"),
                Ok(Some(recovered)),
            )
            .expect("exact committed pair must recover"),
            GoalBackedRequestDisposition::Idempotent
        );

        let absent = resolve_ambiguous_goal_submission_commit(
            anyhow::anyhow!("original commit error"),
            Ok(None),
        )
        .expect_err("absent committed state must preserve the commit error");
        assert_eq!(absent.to_string(), "original commit error");

        let conflict = resolve_ambiguous_goal_submission_commit(
            anyhow::anyhow!("commit acknowledgement lost"),
            Err(anyhow::anyhow!("immutable request fingerprint conflicts")),
        )
        .expect_err("mismatched committed state must remain a conflict");
        assert!(
            conflict
                .to_string()
                .contains("immutable request fingerprint conflicts"),
            "{conflict:#}"
        );
    }

    #[test]
    fn model_goal_create_is_owned_idempotent_and_budget_bounded() {
        let request = GoalCreateRequest {
            caller: "did:a".into(),
            current_session: "session-a".into(),
            requested_owner: "did:a".into(),
            requested_session: "session-a".into(),
            objective: "ship".into(),
            objective_nonempty: true,
            token_budget: Some(i64::MAX as i128),
            goal_tools: true,
            goal_create: true,
        };
        assert_eq!(
            decide_model_goal_create(&request, None),
            GoalCreateDisposition::Fresh
        );
        let fingerprint = goal_creation_fingerprint(&request);
        assert_eq!(
            decide_model_goal_create(&request, Some(&fingerprint)),
            GoalCreateDisposition::Idempotent
        );
        assert_eq!(
            decide_model_goal_create(
                &GoalCreateRequest {
                    requested_owner: "did:b".into(),
                    ..request.clone()
                },
                None,
            ),
            GoalCreateDisposition::Denied
        );
        assert_eq!(
            decide_model_goal_create(
                &GoalCreateRequest {
                    token_budget: Some(i64::MAX as i128 + 1),
                    ..request
                },
                None,
            ),
            GoalCreateDisposition::Invalid
        );
    }

    #[test]
    fn blocked_audit_requires_three_distinct_requests_for_the_same_condition() {
        let (first, accepted) = next_blocked_audit(0, None, None, "needs approval", "req-1");
        assert_eq!((first, accepted), (1, false));

        let (duplicate, accepted) = next_blocked_audit(
            first,
            Some("needs approval"),
            Some("req-1"),
            "needs approval",
            "req-1",
        );
        assert_eq!((duplicate, accepted), (1, false));

        let (second, accepted) = next_blocked_audit(
            duplicate,
            Some("needs approval"),
            Some("req-1"),
            "needs approval",
            "req-2",
        );
        assert_eq!((second, accepted), (2, false));

        let (third, accepted) = next_blocked_audit(
            second,
            Some("needs approval"),
            Some("req-2"),
            "needs approval",
            "req-3",
        );
        assert_eq!((third, accepted), (3, true));

        let (changed, accepted) = next_blocked_audit(
            third,
            Some("needs approval"),
            Some("req-3"),
            "network unavailable",
            "req-4",
        );
        assert_eq!((changed, accepted), (1, false));
    }
}
