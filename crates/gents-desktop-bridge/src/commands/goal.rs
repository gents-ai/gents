use anyhow::{ensure, Context, Result};
use gents_desktop_core::client::ClientCore;
use gents_protocol::goal_control::{GoalControlOutcome, GoalOperation};

use crate::error::{BridgeError, BridgeErrorCode};
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
) -> Result<GoalControlResult, BridgeError> {
    let operation = match request.action {
        GoalControlAction::Create {
            objective,
            token_budget,
        } => GoalOperation::Create {
            objective,
            token_budget: budget(token_budget).map_err(invalid_goal_argument)?,
        },
        GoalControlAction::Edit {
            objective,
            token_budget,
        } => GoalOperation::Edit {
            objective,
            token_budget: budget(token_budget).map_err(invalid_goal_argument)?,
        },
        GoalControlAction::Pause {} => GoalOperation::Pause {},
        GoalControlAction::Resume {} => GoalOperation::Resume {},
        GoalControlAction::Stop {} => GoalOperation::Stop {},
    };
    if request.node_did.trim().is_empty() {
        return Err(BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            "node_did is required",
        ));
    }
    let session_id = match request.session_id {
        Some(session) => {
            if session.trim().is_empty() {
                return Err(BridgeError::new(
                    BridgeErrorCode::InvalidArgument,
                    "session_id is required",
                ));
            }
            session
        }
        None => {
            if !matches!(operation, GoalOperation::Create { .. }) {
                return Err(BridgeError::new(
                    BridgeErrorCode::InvalidArgument,
                    "select a session before changing its goal",
                ));
            }
            uuid::Uuid::new_v4().to_string()
        }
    };
    let input = gents_protocol::request_input::RequestInput {
        cwd: super::chat::chat_folder(request.cwd.as_deref()).map_err(invalid_goal_argument)?,
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
        .await
        .map_err(BridgeError::classify_transport_error)?;
    goal_control_result(session_id, receipt.outcome)
}

fn invalid_goal_argument(error: anyhow::Error) -> BridgeError {
    BridgeError::new(BridgeErrorCode::InvalidArgument, error.to_string())
}

fn goal_control_result(
    session_id: String,
    outcome: GoalControlOutcome,
) -> Result<GoalControlResult, BridgeError> {
    match outcome {
        GoalControlOutcome::Applied {
            goal_id,
            request_id,
        } => Ok(GoalControlResult {
            goal_id,
            session_id,
            request_id,
        }),
        GoalControlOutcome::Rejected { reason } => Err(BridgeError::new(
            BridgeErrorCode::GoalControlRejected,
            reason,
        )),
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

    #[test]
    fn goal_control_rejection_keeps_its_reason_and_code_in_host_and_tauri_payloads() {
        let reason = "wait for this session to finish before starting a goal";
        let error = goal_control_result(
            "chat".into(),
            GoalControlOutcome::Rejected {
                reason: reason.into(),
            },
        )
        .unwrap_err();
        assert_eq!(error.code.as_str(), "goalControlRejected");
        let tauri = serde_json::to_value(&error).unwrap();
        let host =
            serde_json::to_value(crate::host::protocol::HostMessage::response(1, Err(error)))
                .unwrap();
        assert_eq!(host["error"], tauri);
        assert_eq!(host["error"]["code"], "goalControlRejected");
        assert_eq!(host["error"]["message"], reason);
        assert_eq!(host["error"]["retryable"], false);
        assert!(host["error"]["endpoint"].is_null());
    }

    #[test]
    fn goal_control_transport_lookalike_is_not_a_confirmed_rejection() {
        let error = BridgeError::classify_transport_error(anyhow::anyhow!(
            "Runtime rejected goal control: wait for this session to finish before starting a goal"
        ));
        assert_eq!(error.code, BridgeErrorCode::Unknown);
    }
}
