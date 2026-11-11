use super::*;

pub(super) fn pause_reason(
    terminal: GoalRequestTerminal,
    control_unavailable: bool,
) -> &'static str {
    match terminal {
        GoalRequestTerminal::Interrupted => "interrupted",
        GoalRequestTerminal::Superseded => "superseded",
        GoalRequestTerminal::Completed if control_unavailable => {
            "Goal control attribution remained unavailable after the bounded recovery attempts"
        }
        GoalRequestTerminal::Completed => "completed request produced no model or tool activity",
        GoalRequestTerminal::Failed | GoalRequestTerminal::Dead => {
            "infrastructure retry budget exhausted"
        }
    }
}

impl GoalSource {
    pub(super) async fn reject_non_canonical_goal(
        &self,
        actor: &identity::Did,
        goal: &GoalDocument,
    ) -> Result<()> {
        if goal.parsed_status() == Some(GoalStatus::Active) {
            let now = Utc::now();
            let active_time = goal.current_active_time_seconds(now);
            let updated_at = escape_graphql_string(&now.to_rfc3339());
            let _ = update_goal_fields_if_status_from_access(
                &self.agent_access(actor)?,
                goal,
                GoalStatus::Active,
                &format!(
                    r#"status: "paused", active_time_seconds: {active_time}, active_started_at: null, last_failure: "duplicate non-canonical goal", updated_at: "{updated_at}""#
                ),
            )
            .await?;
        } else {
            tracing::warn!(
                goal_id = %goal.goal_id,
                doc_id = %goal.doc_id,
                status = %goal.status,
                "ignored non-canonical goal without applying an illegal status transition"
            );
        }
        Ok(())
    }

    pub(super) async fn handle_usage_limit(
        &self,
        actor: &identity::Did,
        goal: &GoalDocument,
        status: GoalStatus,
        terminal: GoalRequestTerminal,
        latest: &AgentRequestRow,
    ) -> Result<bool> {
        if status == GoalStatus::Active
            && matches!(
                terminal,
                GoalRequestTerminal::Failed | GoalRequestTerminal::Dead
            )
        {
            if let Some(reason) = self
                .request_usage_limit_failure(actor, &goal.node_did, &latest.request_id)
                .await?
            {
                let post = goal
                    .state()
                    .and_then(|state| state.step(GoalAction::UsageLimit))
                    .context("usage-limit transition must be legal from active")?;
                let now = Utc::now();
                let active_time = goal.current_active_time_seconds(now);
                let updated_at = escape_graphql_string(&now.to_rfc3339());
                let reason = escape_graphql_string(&reason);
                if !update_goal_fields_if_status_from_access(
                    &self.agent_access(actor)?,
                    goal,
                    GoalStatus::Active,
                    &format!(
                        r#"status: "{}", active_time_seconds: {active_time}, active_started_at: null, last_failure: "{reason}", updated_at: "{updated_at}""#,
                        post.status.as_str()
                    ),
                )
                .await?
                {
                    return Ok(true);
                }
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(super) async fn reset_blocked_audits(
        &self,
        actor: &identity::Did,
        goal: &mut GoalDocument,
        status: GoalStatus,
        terminal: GoalRequestTerminal,
        latest: &AgentRequestRow,
    ) -> Result<bool> {
        if terminal == GoalRequestTerminal::Completed
            && goal.consecutive_blocked_audits.unwrap_or_default() > 0
            && goal.last_blocked_request_id.as_deref() != Some(latest.request_id.as_str())
        {
            let updated_at = escape_graphql_string(&Utc::now().to_rfc3339());
            if !update_goal_fields_if_status_from_access(
                &self.agent_access(actor)?,
                goal,
                status,
                &format!(
                    r#"consecutive_blocked_audits: 0, last_blocked_request_id: null, last_blocked_reason: null, updated_at: "{updated_at}""#
                ),
            )
            .await?
            {
                return Ok(false);
            }
            goal.consecutive_blocked_audits = Some(0);
            goal.last_blocked_request_id = None;
            goal.last_blocked_reason = None;
        }
        Ok(true)
    }

    pub(super) async fn begin_wrapup(
        &self,
        actor: &identity::Did,
        goal: &mut GoalDocument,
        status: GoalStatus,
        wrapup: bool,
    ) -> Result<bool> {
        if wrapup && status == GoalStatus::Active {
            let now = Utc::now();
            let active_time = goal.current_active_time_seconds(now);
            let updated_at = escape_graphql_string(&now.to_rfc3339());
            if !update_goal_fields_if_status_from_access(
                &self.agent_access(actor)?,
                goal,
                GoalStatus::Active,
                &format!(
                    r#"status: "budget_limited", wrapup_requested: true, active_time_seconds: {active_time}, active_started_at: null, updated_at: "{updated_at}""#
                ),
            )
            .await?
            {
                return Ok(false);
            }
            goal.status = GoalStatus::BudgetLimited.as_str().to_string();
            goal.wrapup_requested = Some(true);
        }
        Ok(true)
    }

    pub(super) async fn claim_decided_continuation(
        &self,
        actor: &identity::Did,
        goal: &GoalDocument,
        latest: &AgentRequestRow,
        decision: GoalDecision,
        failure_name: &str,
    ) -> Result<Option<GoalContinuationPhase>> {
        let claimed = if decision == GoalDecision::Retry {
            claim_retry_continuation_from_access(
                &self.agent_access(actor)?,
                goal,
                &latest.request_id,
                goal.infrastructure_retry_count.unwrap_or_default(),
                failure_name,
            )
            .await?
        } else {
            claim_continuation_from_access(&self.agent_access(actor)?, goal, &latest.request_id)
                .await?
        };
        let claimed_phase = goal_continuation_materialization_step(
            GoalContinuationPhase::Unclaimed,
            GoalContinuationAction::Claim(claimed),
        );
        if claimed_phase != GoalContinuationPhase::Claimed {
            return Ok(None);
        }
        let materialization_phase = goal_continuation_materialization_step(
            claimed_phase,
            GoalContinuationAction::Materialize,
        );
        Ok(Some(materialization_phase))
    }
}
