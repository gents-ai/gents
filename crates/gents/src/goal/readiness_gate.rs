use super::*;
use crate::runtime::status::{ReconcilePhase, ReconcileResult};
use gents_protocol::row::{
    is_behavior_unavailable_rejection, project_node_readiness, AgentReadinessUnavailableReason,
    AgentReadinessUnknownReason, AgentRequestRow, NodeReadinessRow, ProjectedAgentReadiness,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalAgentObservation {
    Ready { newer_than_terminal: bool },
    BackendRecovering,
    Unavailable,
    Unassigned,
    Unknown,
}

impl GoalAgentObservation {
    pub fn newer_than_terminal(self) -> bool {
        match self {
            Self::Ready {
                newer_than_terminal,
            } => newer_than_terminal,
            _ => true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalAgentReadiness {
    Ready,
    Waiting,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalFailureCause {
    Attempt,
    AgentUnavailable,
    GoalControlUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalGatedDecision {
    Decided(GoalDecision),
    AwaitReadiness,
    AgentUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GoalContinuationFacts {
    pub status: GoalStatus,
    pub terminal: GoalRequestTerminal,
    pub session_idle: bool,
    pub child_exists: bool,
    pub budget_reached: bool,
    pub has_activity: bool,
    pub request_is_wrapup: bool,
    pub infrastructure_retries: i64,
    pub wrapup_requested: bool,
    pub wrapup_completed: bool,
}

pub fn observe_goal_agent_readiness(
    observation: GoalAgentObservation,
    settled: bool,
) -> GoalAgentReadiness {
    match observation {
        GoalAgentObservation::Ready { .. } => GoalAgentReadiness::Ready,
        GoalAgentObservation::BackendRecovering | GoalAgentObservation::Unknown => {
            GoalAgentReadiness::Waiting
        }
        GoalAgentObservation::Unavailable | GoalAgentObservation::Unassigned => {
            if settled {
                GoalAgentReadiness::Unavailable
            } else {
                GoalAgentReadiness::Waiting
            }
        }
    }
}

fn base_goal_decision(cause: GoalFailureCause, facts: &GoalContinuationFacts) -> GoalDecision {
    match cause {
        GoalFailureCause::Attempt => decide_goal_continuation(
            facts.status,
            facts.terminal,
            facts.session_idle,
            facts.child_exists,
            facts.budget_reached,
            facts.has_activity,
            facts.request_is_wrapup,
            facts.infrastructure_retries,
            facts.wrapup_requested,
            facts.wrapup_completed,
        ),
        GoalFailureCause::GoalControlUnavailable => decide_goal_continuation(
            facts.status,
            GoalRequestTerminal::Failed,
            facts.session_idle,
            facts.child_exists,
            facts.budget_reached,
            false,
            facts.request_is_wrapup,
            facts.infrastructure_retries,
            facts.wrapup_requested,
            facts.wrapup_completed,
        ),
        GoalFailureCause::AgentUnavailable => decide_goal_continuation(
            facts.status,
            GoalRequestTerminal::Completed,
            facts.session_idle,
            facts.child_exists,
            facts.budget_reached,
            true,
            false,
            facts.infrastructure_retries,
            facts.wrapup_requested,
            facts.wrapup_completed,
        ),
    }
}

fn publishes(decision: GoalDecision) -> bool {
    matches!(
        decision,
        GoalDecision::Continue | GoalDecision::Retry | GoalDecision::Wrapup
    )
}

pub fn gate_goal_continuation(
    observation: GoalAgentObservation,
    settled: bool,
    cause: GoalFailureCause,
    facts: &GoalContinuationFacts,
) -> GoalGatedDecision {
    let base = base_goal_decision(cause, facts);
    if !publishes(base) {
        return GoalGatedDecision::Decided(base);
    }
    match observe_goal_agent_readiness(observation, settled) {
        GoalAgentReadiness::Ready
            if cause == GoalFailureCause::AgentUnavailable
                && !observation.newer_than_terminal() =>
        {
            GoalGatedDecision::AwaitReadiness
        }
        GoalAgentReadiness::Ready => GoalGatedDecision::Decided(base),
        GoalAgentReadiness::Waiting => GoalGatedDecision::AwaitReadiness,
        GoalAgentReadiness::Unavailable => GoalGatedDecision::AgentUnavailable,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalClaimedDecision {
    Materialize,
    AwaitReadiness,
    Stop,
    Inactive,
}

pub fn gate_claimed_goal_continuation(
    observation: GoalAgentObservation,
    settled: bool,
    child_exists: bool,
    state: &GoalState,
) -> GoalClaimedDecision {
    if child_exists
        || state.wrapup_completed
        || !(state.status == GoalStatus::Active
            || (state.status == GoalStatus::BudgetLimited && state.wrapup_requested))
    {
        return GoalClaimedDecision::Inactive;
    }
    match observe_goal_agent_readiness(observation, settled) {
        GoalAgentReadiness::Ready => GoalClaimedDecision::Materialize,
        GoalAgentReadiness::Waiting => GoalClaimedDecision::AwaitReadiness,
        GoalAgentReadiness::Unavailable => GoalClaimedDecision::Stop,
    }
}

pub fn next_goal_infrastructure_retries(
    cause: GoalFailureCause,
    facts: &GoalContinuationFacts,
    gated: GoalGatedDecision,
) -> i64 {
    let retries = facts.infrastructure_retries.max(0);
    match gated {
        GoalGatedDecision::Decided(GoalDecision::Retry) => retries.saturating_add(1),
        GoalGatedDecision::Decided(GoalDecision::Continue | GoalDecision::Wrapup)
            if cause == GoalFailureCause::Attempt
                && facts.terminal == GoalRequestTerminal::Completed =>
        {
            0
        }
        _ => retries,
    }
}

pub fn goal_failure_cause(request: &AgentRequestRow) -> GoalFailureCause {
    let unclaimed = request.execution_generation.is_none();
    let failed = request.lifecycle_state == Some(RequestLifecycleState::Failed);
    let readiness_rejection = request
        .failure_reason
        .as_deref()
        .is_some_and(is_behavior_unavailable_rejection);
    if failed && unclaimed && readiness_rejection {
        GoalFailureCause::AgentUnavailable
    } else {
        GoalFailureCause::Attempt
    }
}

pub fn goal_agent_observation(
    projected: &ProjectedAgentReadiness,
    newer_than_terminal: bool,
) -> GoalAgentObservation {
    match projected {
        ProjectedAgentReadiness::Ready => GoalAgentObservation::Ready {
            newer_than_terminal,
        },
        ProjectedAgentReadiness::Unavailable(
            AgentReadinessUnavailableReason::BackendTemporarilyUnavailable,
        ) => GoalAgentObservation::BackendRecovering,
        ProjectedAgentReadiness::Unavailable(_) => GoalAgentObservation::Unavailable,
        ProjectedAgentReadiness::Unknown(AgentReadinessUnknownReason::AgentNotAssigned) => {
            GoalAgentObservation::Unassigned
        }
        ProjectedAgentReadiness::Unknown(_) => GoalAgentObservation::Unknown,
    }
}

pub fn goal_readiness_newer_than_terminal(
    readiness_updated_at: Option<&str>,
    terminalized_at: Option<&str>,
) -> bool {
    let parse =
        |value: Option<&str>| value.and_then(|value| DateTime::parse_from_rfc3339(value).ok());
    match (parse(readiness_updated_at), parse(terminalized_at)) {
        (Some(readiness), Some(terminal)) => readiness > terminal,
        _ => false,
    }
}

pub(crate) fn goal_reconcile_settled(
    reconcile_phase: Option<&str>,
    last_result: Option<&str>,
) -> bool {
    reconcile_phase == Some(ReconcilePhase::Idle.as_str())
        && last_result != Some(ReconcileResult::Error.as_str())
}

pub const GOAL_READINESS_WAIT_PREFIX: &str = "waiting for agent readiness: ";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedGoalAgent {
    pub observation: GoalAgentObservation,
    pub settled: bool,
    pub reason: String,
}

impl ObservedGoalAgent {
    pub fn waiting_reason(&self) -> String {
        format!("{GOAL_READINESS_WAIT_PREFIX}{}", self.reason)
    }
}

pub async fn observe_goal_agent(
    node: &EmbeddedNode,
    node_did: &str,
    agent_id: Option<&str>,
    terminalized_at: Option<&str>,
) -> Result<ObservedGoalAgent> {
    let escaped_did = escape_graphql_string(node_did);
    let query = format!(
        r#"{{
            NodeReadiness(filter: {{ node_did: {{ _eq: "{escaped_did}" }} }}, limit: 1) {{
                node_did snapshot_json updated_at
            }}
            NodeRuntime(filter: {{ node_did: {{ _eq: "{escaped_did}" }} }}, limit: 1) {{
                reconcile_phase last_reconcile_result
            }}
        }}"#
    );
    let response =
        graphql_with_transaction_retry(node, &query, "query goal agent readiness").await?;
    let readiness_row = rows::<NodeReadinessRow>(&response, "NodeReadiness")?
        .into_iter()
        .next();
    let runtime = rows::<serde_json::Value>(&response, "NodeRuntime")?
        .into_iter()
        .next();
    let settled = runtime.as_ref().is_some_and(|row| {
        goal_reconcile_settled(
            row.get("reconcile_phase").and_then(|value| value.as_str()),
            row.get("last_reconcile_result")
                .and_then(|value| value.as_str()),
        )
    });
    let newer_than_terminal = goal_readiness_newer_than_terminal(
        readiness_row.as_ref().map(|row| row.updated_at.as_str()),
        terminalized_at,
    );
    let agent_id = agent_id.map(str::trim).filter(|id| !id.is_empty());
    let projected = match agent_id {
        Some(agent_id) => {
            project_node_readiness(readiness_row.as_ref(), node_did, [agent_id], None)
                .agents
                .remove(agent_id)
                .unwrap_or(ProjectedAgentReadiness::Unknown(
                    AgentReadinessUnknownReason::AgentNotAssigned,
                ))
        }
        None => ProjectedAgentReadiness::Unknown(AgentReadinessUnknownReason::AgentNotAssigned),
    };
    let observation = goal_agent_observation(&projected, newer_than_terminal);
    let label = agent_id.unwrap_or("<none>");
    let reason = match projected {
        ProjectedAgentReadiness::Unavailable(reason) => {
            format!("agent {label} is unavailable: {}", reason.public_message())
        }
        ProjectedAgentReadiness::Unknown(AgentReadinessUnknownReason::AgentNotAssigned) => {
            format!("agent {label} is not assigned to this runtime")
        }
        ProjectedAgentReadiness::Ready => {
            format!("agent {label} readiness has not been republished since its rejection")
        }
        ProjectedAgentReadiness::Unknown(reason) => {
            format!("agent {label} readiness is unknown ({reason:?})")
        }
    };
    Ok(ObservedGoalAgent {
        observation,
        settled,
        reason,
    })
}
