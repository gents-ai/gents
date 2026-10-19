use anyhow::{ensure, Context, Result};
use gents_desktop_core::client::ClientCore;
use gents_protocol::goal_control::{GoalControlOutcome, GoalOperation};

use crate::types::{
    GoalControlAction, GoalControlRequest, GoalControlResult, GoalControlView, SessionGoalView,
};

pub async fn session_goal(
    core: &ClientCore,
    node_did: &str,
    session_id: &str,
) -> Result<Option<SessionGoalView>> {
    let observation = core.session_goal_observation(node_did, session_id).await?;
    Ok(observation.map(|observation| SessionGoalView {
        node_did: observation.node_did,
        session_id: observation.session_id,
        session_doc_id: observation.session_doc_id,
        observed_at: observation.observed_at,
        goal: observation.goal.map(|goal| {
            let snapshot = goal.snapshot;
            GoalControlView {
                goal_id: snapshot.goal_id,
                revision: goal.revision,
                objective: snapshot.objective,
                status: snapshot.status,
                token_budget: snapshot.token_budget.map(|budget| budget.to_string()),
                tokens_used: snapshot.tokens_used.to_string(),
                active_time_seconds: snapshot.active_time_seconds.to_string(),
                continuation_sequence: snapshot.continuation_sequence.to_string(),
                last_blocked_reason: snapshot.last_blocked_reason,
                last_failure: snapshot.last_failure,
                completion_evidence: snapshot.completion_evidence,
            }
        }),
    }))
}

fn budget(raw: Option<String>) -> Result<Option<i64>> {
    raw.map(|raw| {
        let budget: i64 = raw
            .parse()
            .context("token budget must be a positive whole number within the runtime limit")?;
        ensure!(budget > 0, "token budget must be positive");
        Ok(budget)
    })
    .transpose()
}

pub async fn control_goal(
    core: &ClientCore,
    request: GoalControlRequest,
) -> Result<GoalControlResult> {
    let operation = match request.action {
        GoalControlAction::Create {
            objective,
            token_budget,
        } => GoalOperation::Create {
            objective,
            token_budget: budget(token_budget)?,
        },
        GoalControlAction::Edit {
            objective,
            token_budget,
        } => GoalOperation::Edit {
            objective,
            token_budget: budget(token_budget)?,
        },
        GoalControlAction::Pause {} => GoalOperation::Pause {},
        GoalControlAction::Resume {} => GoalOperation::Resume {},
    };
    ensure!(!request.node_did.trim().is_empty(), "node_did is required");
    let session_id = match request.session_id {
        Some(session) => {
            ensure!(!session.trim().is_empty(), "session_id is required");
            session
        }
        None => {
            ensure!(
                matches!(operation, GoalOperation::Create { .. }),
                "select a session before changing its goal"
            );
            uuid::Uuid::new_v4().to_string()
        }
    };
    let input = gents_protocol::request_input::RequestInput {
        cwd: super::chat::chat_folder(request.cwd.as_deref())?,
        captured_context: request.captured_context.unwrap_or_default(),
        ..Default::default()
    };
    let receipt = core
        .control_session_goal(
            &request.node_did,
            &session_id,
            request.session_doc_id,
            request.expected,
            operation,
            request.agent_id.as_deref(),
            input,
        )
        .await?;
    match receipt.outcome {
        GoalControlOutcome::Applied {
            goal_id,
            request_id,
        } => Ok(GoalControlResult {
            goal_id,
            session_id,
            request_id,
        }),
        GoalControlOutcome::Rejected { reason } => {
            anyhow::bail!("Runtime rejected goal control: {reason}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_preserves_the_full_signed_integer_range() {
        assert_eq!(budget(Some(i64::MAX.to_string())).unwrap(), Some(i64::MAX));
        assert_eq!(budget(None).unwrap(), None);
        for invalid in ["0", "-1", "1.5", "1e6", "9223372036854775808", ""] {
            assert!(budget(Some(invalid.into())).is_err(), "{invalid}");
        }
    }
}
