use super::*;

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
