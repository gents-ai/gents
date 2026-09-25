//! Executable mirror of `proofs/Proofs/GoalAutomation/ReadinessGate.lean` and
//! its native adapters over the canonical behavior-readiness row.
use super::*;
use gents_protocol::row::{
    is_behavior_unavailable_rejection, project_behavior_readiness, AgentBehaviorReadinessRow,
    AgentRequestRow, BehaviorReadinessUnavailableReason, BehaviorReadinessUnknownReason,
    ProjectedBehaviorReadiness,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalBehaviorObservation {
    Ready,
    BackendRecovering,
    Unavailable,
    Unassigned,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalBehaviorReadiness {
    Ready,
    Waiting,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalFailureCause {
    Attempt,
    BehaviorUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalGatedDecision {
    Decided(GoalDecision),
    AwaitReadiness,
    BehaviorUnavailable,
}

/// Inputs of the existing `decide_goal_continuation` owner.
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

/// Mirror of `ReadinessGate.observe`.
pub fn observe_goal_behavior_readiness(
    observation: GoalBehaviorObservation,
    settled: bool,
) -> GoalBehaviorReadiness {
    match observation {
        GoalBehaviorObservation::Ready => GoalBehaviorReadiness::Ready,
        GoalBehaviorObservation::BackendRecovering | GoalBehaviorObservation::Unknown => {
            GoalBehaviorReadiness::Waiting
        }
        GoalBehaviorObservation::Unavailable | GoalBehaviorObservation::Unassigned => {
            if settled {
                GoalBehaviorReadiness::Unavailable
            } else {
                GoalBehaviorReadiness::Waiting
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
        GoalFailureCause::BehaviorUnavailable => decide_goal_continuation(
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

/// Mirror of `ReadinessGate.gate`.
pub fn gate_goal_continuation(
    readiness: GoalBehaviorReadiness,
    cause: GoalFailureCause,
    facts: &GoalContinuationFacts,
) -> GoalGatedDecision {
    let base = base_goal_decision(cause, facts);
    if !publishes(base) {
        return GoalGatedDecision::Decided(base);
    }
    match readiness {
        GoalBehaviorReadiness::Ready => GoalGatedDecision::Decided(base),
        GoalBehaviorReadiness::Waiting => GoalGatedDecision::AwaitReadiness,
        GoalBehaviorReadiness::Unavailable => GoalGatedDecision::BehaviorUnavailable,
    }
}

/// Mirror of `ReadinessGate.nextRetries`: the persisted
/// `infrastructure_retry_count` after GoalSource applies `gated`.
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

/// Routing rejects an unclaimed request with the readiness public message
/// when its behavior cannot accept work. A claim always writes
/// `execution_generation`, so its absence proves nothing executed.
pub fn goal_failure_cause(request: &AgentRequestRow) -> GoalFailureCause {
    let unclaimed = request.execution_generation.is_none();
    let failed = request.lifecycle_state == Some(RequestLifecycleState::Failed);
    let readiness_rejection = request
        .failure_reason
        .as_deref()
        .is_some_and(is_behavior_unavailable_rejection);
    if failed && unclaimed && readiness_rejection {
        GoalFailureCause::BehaviorUnavailable
    } else {
        GoalFailureCause::Attempt
    }
}

pub fn goal_behavior_observation(
    projected: &ProjectedBehaviorReadiness,
) -> GoalBehaviorObservation {
    match projected {
        ProjectedBehaviorReadiness::Ready => GoalBehaviorObservation::Ready,
        ProjectedBehaviorReadiness::Unavailable(
            BehaviorReadinessUnavailableReason::BackendTemporarilyUnavailable,
        ) => GoalBehaviorObservation::BackendRecovering,
        ProjectedBehaviorReadiness::Unavailable(_) => GoalBehaviorObservation::Unavailable,
        ProjectedBehaviorReadiness::Unknown(
            BehaviorReadinessUnknownReason::BehaviorNotAssigned,
        ) => GoalBehaviorObservation::Unassigned,
        ProjectedBehaviorReadiness::Unknown(_) => GoalBehaviorObservation::Unknown,
    }
}

/// `AgentRuntime` reports settled reconciliation as an idle phase whose last
/// reconcile did not fail.
pub fn goal_reconcile_settled(reconcile_phase: Option<&str>, last_result: Option<&str>) -> bool {
    reconcile_phase == Some("idle") && last_result != Some("error")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedGoalBehavior {
    pub observation: GoalBehaviorObservation,
    pub readiness: GoalBehaviorReadiness,
    /// Presentation-safe reason recorded when the Goal stops on it.
    pub reason: String,
}

pub async fn observe_goal_behavior(
    node: &EmbeddedNode,
    agent_did: &str,
    behavior_id: Option<&str>,
) -> Result<ObservedGoalBehavior> {
    let escaped_did = escape_graphql_string(agent_did);
    let query = format!(
        r#"{{
            AgentBehaviorReadiness(filter: {{ agent_did: {{ _eq: "{escaped_did}" }} }}, limit: 1) {{
                agent_did snapshot_json updated_at
            }}
            AgentRuntime(filter: {{ agent_did: {{ _eq: "{escaped_did}" }} }}, limit: 1) {{
                reconcile_phase last_reconcile_result
            }}
        }}"#
    );
    let response =
        graphql_with_transaction_retry(node, &query, "query goal behavior readiness").await?;
    let readiness_row = rows::<AgentBehaviorReadinessRow>(&response, "AgentBehaviorReadiness")?
        .into_iter()
        .next();
    let runtime = rows::<serde_json::Value>(&response, "AgentRuntime")?
        .into_iter()
        .next();
    let settled = runtime.as_ref().is_some_and(|row| {
        goal_reconcile_settled(
            row.get("reconcile_phase").and_then(|value| value.as_str()),
            row.get("last_reconcile_result")
                .and_then(|value| value.as_str()),
        )
    });
    let behavior_id = behavior_id.map(str::trim).filter(|id| !id.is_empty());
    let projected = match behavior_id {
        Some(behavior_id) => project_behavior_readiness(
            readiness_row.as_ref(),
            agent_did,
            [behavior_id],
            None,
            Utc::now(),
        )
        .behaviors
        .remove(behavior_id)
        .unwrap_or(ProjectedBehaviorReadiness::Unknown(
            BehaviorReadinessUnknownReason::BehaviorNotAssigned,
        )),
        None => {
            ProjectedBehaviorReadiness::Unknown(BehaviorReadinessUnknownReason::BehaviorNotAssigned)
        }
    };
    let observation = goal_behavior_observation(&projected);
    let label = behavior_id.unwrap_or("<none>");
    let reason = match projected {
        ProjectedBehaviorReadiness::Unavailable(reason) => {
            format!(
                "behavior {label} is unavailable: {}",
                reason.public_message()
            )
        }
        ProjectedBehaviorReadiness::Unknown(
            BehaviorReadinessUnknownReason::BehaviorNotAssigned,
        ) => {
            format!("behavior {label} is not assigned to this runtime")
        }
        ProjectedBehaviorReadiness::Ready | ProjectedBehaviorReadiness::Unknown(_) => {
            format!("behavior {label} readiness is not established")
        }
    };
    Ok(ObservedGoalBehavior {
        observation,
        readiness: observe_goal_behavior_readiness(observation, settled),
        reason,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projected_readiness_maps_onto_the_modeled_observation() {
        use BehaviorReadinessUnavailableReason as Reason;
        use BehaviorReadinessUnknownReason as Unknown;
        assert_eq!(
            goal_behavior_observation(&ProjectedBehaviorReadiness::Ready),
            GoalBehaviorObservation::Ready
        );
        assert_eq!(
            goal_behavior_observation(&ProjectedBehaviorReadiness::Unavailable(
                Reason::BackendTemporarilyUnavailable
            )),
            GoalBehaviorObservation::BackendRecovering
        );
        for reason in [
            Reason::BehaviorDisabled,
            Reason::RuntimeConfigurationInvalid,
            Reason::BackendNotConfigured,
            Reason::BackendDisabled,
            Reason::CredentialsRequired,
            Reason::InferenceProfileInvalid,
            Reason::ToolConfigurationInvalid,
            Reason::ToolSurfaceUnavailable,
            Reason::ExecutorStartFailed,
        ] {
            assert_eq!(
                goal_behavior_observation(&ProjectedBehaviorReadiness::Unavailable(reason)),
                GoalBehaviorObservation::Unavailable,
                "{reason:?}"
            );
        }
        assert_eq!(
            goal_behavior_observation(&ProjectedBehaviorReadiness::Unknown(
                Unknown::BehaviorNotAssigned
            )),
            GoalBehaviorObservation::Unassigned
        );
        for reason in [
            Unknown::ReadinessMissing,
            Unknown::ReadinessMalformed,
            Unknown::ReadinessVersionUnsupported,
            Unknown::ReadinessStale,
            Unknown::ProcessNotReady,
            Unknown::RouterGenerationStale,
        ] {
            assert_eq!(
                goal_behavior_observation(&ProjectedBehaviorReadiness::Unknown(reason)),
                GoalBehaviorObservation::Unknown,
                "{reason:?}"
            );
        }
    }

    #[test]
    fn only_idle_error_free_reconciliation_is_settled() {
        assert!(goal_reconcile_settled(Some("idle"), Some("applied")));
        assert!(goal_reconcile_settled(Some("idle"), Some("")));
        assert!(goal_reconcile_settled(Some("idle"), None));
        assert!(!goal_reconcile_settled(Some("idle"), Some("error")));
        for phase in ["debouncing", "resolving", "diffing", "applying"] {
            assert!(!goal_reconcile_settled(Some(phase), Some("applied")));
        }
        assert!(!goal_reconcile_settled(None, None));
    }

    #[test]
    fn only_an_unclaimed_readiness_rejection_is_uncharged() {
        let rejected = AgentRequestRow {
            request_id: "child".into(),
            lifecycle_state: Some(RequestLifecycleState::Failed),
            failure_reason: Some(
                BehaviorReadinessUnavailableReason::RuntimeConfigurationInvalid
                    .public_message()
                    .into(),
            ),
            ..Default::default()
        };
        assert_eq!(
            goal_failure_cause(&rejected),
            GoalFailureCause::BehaviorUnavailable
        );
        let unassigned = AgentRequestRow {
            failure_reason: Some(gents_protocol::row::BEHAVIOR_NOT_ASSIGNED_MESSAGE.into()),
            ..rejected.clone()
        };
        assert_eq!(
            goal_failure_cause(&unassigned),
            GoalFailureCause::BehaviorUnavailable
        );
        let claimed = AgentRequestRow {
            execution_generation: Some("generation".into()),
            ..rejected.clone()
        };
        assert_eq!(goal_failure_cause(&claimed), GoalFailureCause::Attempt);
        let other = AgentRequestRow {
            failure_reason: Some("request admission denied: bad signature".into()),
            ..rejected.clone()
        };
        assert_eq!(goal_failure_cause(&other), GoalFailureCause::Attempt);
        let dead = AgentRequestRow {
            lifecycle_state: Some(RequestLifecycleState::Dead),
            ..rejected
        };
        assert_eq!(goal_failure_cause(&dead), GoalFailureCause::Attempt);
    }
}
