use anyhow::{ensure, Context, Result};
use gents_desktop_core::client::ClientCore;
use gents_protocol::goal_control::{GoalControlOutcome, GoalOperation};

use crate::error::{BridgeError, BridgeErrorCode};
use crate::types::{
    GoalControlAction, GoalControlRequest, GoalControlResult, GoalControlView, SessionGoalState,
    SessionGoalView,
};
use gents_automation::goal::SessionGoalObservationState;
use gents_protocol::goal_control::SessionGoalObservation;

pub async fn session_goal(
    core: &ClientCore,
    node_did: &str,
    session_id: &str,
) -> Result<SessionGoalState> {
    let state = core.session_goal_observation(node_did, session_id).await?;
    Ok(session_goal_state(state, |observation| {
        let waiting = waiting_question_ids(core, node_did, session_id, &observation);
        goal_view(observation, waiting)
    }))
}

/// Carries the owner's state and `next` to the desktop DTO.
fn session_goal_state(
    state: SessionGoalObservationState,
    view: impl FnOnce(SessionGoalObservation) -> SessionGoalView,
) -> SessionGoalState {
    let next = state.next();
    match state {
        SessionGoalObservationState::NoReadableSession => {
            SessionGoalState::NoReadableSession { next }
        }
        SessionGoalObservationState::Unknown(cause) => SessionGoalState::Unknown {
            cause,
            next,
            reason: gents_automation::goal::GoalInputRefused::from(cause).to_string(),
        },
        SessionGoalObservationState::Observed(observation) => SessionGoalState::Observed {
            observation: Box::new(view(*observation)),
            next,
        },
    }
}

fn waiting_question_ids(
    core: &ClientCore,
    node_did: &str,
    session_id: &str,
    observation: &SessionGoalObservation,
) -> Option<Vec<String>> {
    let snapshot = core.stores().snapshot(node_did);
    let mut sessions = snapshot.sessions.iter().filter(|session| {
        session.node_did == node_did
            && session.session_id == session_id
            && session.requester_did == observation.requester_did
    });
    let source = sessions.next().filter(|_| sessions.next().is_none())?;
    let latest = source.observation.as_ref()?.latest_request.as_ref()?;
    let ids = gents_automation::goal::awaiting_goal_question_ids(
        observation,
        latest,
        &snapshot.requests,
        &snapshot.mailbox_items,
        chrono::Utc::now(),
    );
    (!ids.is_empty()).then_some(ids)
}

/// The desktop's Goal view. A signed projection carries the runtime's raw
/// counters and text, so this is where they are made displayable: a counter
/// below zero shows as zero, and text that is only whitespace shows as absent
/// (empty for the always-present objective and status).
fn goal_view(
    observation: SessionGoalObservation,
    waiting_question_ids: Option<Vec<String>>,
) -> SessionGoalView {
    let present = |value: String| Some(value).filter(|value| !value.trim().is_empty());
    let shown = |value: String| present(value).unwrap_or_default();
    let count = |value: i64| value.max(0).to_string();
    SessionGoalView {
        waiting_question_ids,
        node_did: observation.node_did,
        session_id: observation.session_id,
        session_doc_id: observation.session_doc_id,
        observed_at: observation.observed_at,
        goal: observation.goal.map(|goal| {
            let snapshot = goal.snapshot;
            GoalControlView {
                goal_id: snapshot.goal_id,
                revision: goal.revision,
                objective: shown(snapshot.objective),
                status: shown(snapshot.status),
                token_budget: snapshot.token_budget.map(|budget| budget.to_string()),
                tokens_used: count(snapshot.tokens_used),
                active_time_seconds: count(snapshot.active_time_seconds),
                continuation_sequence: count(snapshot.continuation_sequence),
                wrapup_requested: snapshot.wrapup_requested,
                wrapup_completed: snapshot.wrapup_completed,
                last_blocked_reason: snapshot.last_blocked_reason.and_then(present),
                last_failure: snapshot.last_failure.and_then(present),
                completion_evidence: snapshot.completion_evidence.and_then(present),
            }
        }),
    }
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
    control_goal_with_reference(core, request, None).await
}

pub(super) async fn control_goal_with_reference(
    core: &ClientCore,
    request: GoalControlRequest,
    resume: Option<crate::types::GoalSubmissionReceipt>,
) -> Result<GoalControlResult, BridgeError> {
    if let Some(submission) = &resume {
        if request.client_did.as_deref() != Some(submission.client_did.as_str())
            || request.command_id.as_deref() != Some(submission.reference.command_id.as_str())
            || request.node_did != submission.node_did
            || request.session_id.as_deref() != Some(submission.session_id.as_str())
        {
            return Err(invalid_goal_argument(anyhow::anyhow!(
                "Goal resume differs from its original caller, runtime or destination"
            )));
        }
    }
    let command_id = match (request.client_did.as_deref(), request.command_id.as_deref()) {
        (None, None) => None,
        (Some(client), Some(command))
            if client == core.node_identity().did() && !command.trim().is_empty() =>
        {
            if request.session_id.is_none() {
                return Err(invalid_goal_argument(anyhow::anyhow!(
                    "a retained Goal command requires its destination session"
                )));
            }
            Some(command.to_owned())
        }
        _ => {
            return Err(invalid_goal_argument(anyhow::anyhow!(
            "a retained Goal command requires its original native client and command identities"
        )))
        }
    };
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
    if request
        .attachments
        .as_ref()
        .is_some_and(|items| !items.is_empty())
        && !matches!(operation, GoalOperation::Create { .. })
    {
        return Err(invalid_goal_argument(anyhow::anyhow!(
            "Attachments belong to a new Goal's opening message"
        )));
    }
    let input = gents_protocol::request_input::RequestInput {
        cwd: super::chat::chat_folder(request.cwd.as_deref()).map_err(invalid_goal_argument)?,
        captured_context: request.captured_context.unwrap_or_default(),
        attachments: request.attachments.unwrap_or_default(),
        ..Default::default()
    };
    let starts_goal = matches!(operation, GoalOperation::Create { .. });
    let receipt = if let Some(command_id) = &command_id {
        core.control_session_goal_correlated(
            command_id,
            &request.node_did,
            &session_id,
            request.session_doc_id,
            request.expected,
            operation,
            request.agent_did.as_deref(),
            input,
            resume.as_ref().map(|submission| &submission.reference),
        )
        .await
    } else {
        core.control_session_goal(
            &request.node_did,
            &session_id,
            request.session_doc_id,
            request.expected,
            operation,
            request.agent_did.as_deref(),
            input,
        )
        .await
    }
    .map_err(BridgeError::classify_transport_error)?;
    if let Some(command_id) = command_id {
        let retained = core
            .recover_goal_submission(&request.node_did, &command_id)
            .await
            .map_err(BridgeError::classify_transport_error)?
            .context("confirmed Goal command has no retained caller binding")
            .map_err(invalid_goal_argument)?;
        return super::goal_submission::applied(
            &retained,
            super::goal_submission::reference(core, &retained),
        );
    }
    goal_control_result(session_id, starts_goal, receipt.outcome)
}

fn invalid_goal_argument(error: anyhow::Error) -> BridgeError {
    BridgeError::new(BridgeErrorCode::InvalidArgument, error.to_string())
}

pub(super) fn goal_control_result(
    session_id: String,
    starts_goal: bool,
    outcome: GoalControlOutcome,
) -> Result<GoalControlResult, BridgeError> {
    match outcome {
        GoalControlOutcome::Applied {
            goal_id,
            request_id,
        } => Ok(GoalControlResult {
            submission_receipt: None,
            started_goal_id: (starts_goal && request_id.is_some()).then(|| goal_id.clone()),
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
    use gents_protocol::goal_control::{GoalRevision, ObservedGoal};
    use gents_store::config_client::agent_session::AgentSessionRowRead;
    use serde_json::{json, Value};

    fn modeled_observation(goal: &Value) -> SessionGoalObservation {
        SessionGoalObservation {
            version: gents_protocol::goal_control::SESSION_GOAL_CONTROL_VERSION,
            node_did: "did:key:node".into(),
            agent_did: "did:key:agent".into(),
            session_id: "chat".into(),
            session_doc_id: "session-document".into(),
            requester_did: Some("did:key:user".into()),
            observed_at: "2026-11-08T00:00:00Z".into(),
            goal: (!goal.is_null()).then(|| {
                let document: gents_automation::goal::GoalDocument =
                    serde_json::from_value(json!({
                        "_docID": "goal-document", "node_did": "did:key:node", "session_id": "chat",
                        "goal_id": format!("goal:{}", goal["goal"]), "objective": "Modeled",
                        "status": "active"
                    }))
                    .unwrap();
                ObservedGoal {
                    assignment_root_request_doc_id: goal["root"]
                        .as_u64()
                        .map(|root| format!("request-document:{root}")),
                    revision: GoalRevision {
                        document: document.doc_id.clone(),
                        composite: "composite".into(),
                    },
                    snapshot: gents_automation::goal::snapshot(&document, chrono::Utc::now()),
                }
            }),
            signature: Vec::new(),
        }
    }

    #[test]
    fn goal_view_clamps_negative_counters_and_blanks_whitespace_text() {
        let mut observation = modeled_observation(&json!({"goal": 1, "root": null}));
        let snapshot = &mut observation.goal.as_mut().unwrap().snapshot;
        snapshot.objective = "  \n".into();
        snapshot.status = "\t".into();
        snapshot.tokens_used = -5;
        snapshot.active_time_seconds = -1;
        snapshot.continuation_sequence = -2;
        snapshot.last_blocked_reason = Some(" ".into());
        snapshot.last_failure = Some("usage_limited: out".into());
        snapshot.completion_evidence = Some(String::new());

        let shown = goal_view(observation, None).goal.unwrap();

        assert_eq!(
            (
                shown.objective.as_str(),
                shown.status.as_str(),
                shown.tokens_used.as_str(),
                shown.active_time_seconds.as_str(),
                shown.continuation_sequence.as_str(),
            ),
            ("", "", "0", "0", "0")
        );
        assert_eq!(shown.last_blocked_reason, None);
        assert_eq!(shown.last_failure.as_deref(), Some("usage_limited: out"));
        assert_eq!(shown.completion_evidence, None);
    }

    /// Replays Lean `SessionObservation.view`, `next` and `refusal` through
    /// the owner's classifier and the DTO projection the desktop receives.
    #[test]
    fn generated_session_goal_views_name_their_cause() {
        use gents_lean_vocab::families::goal_session_observation_contracts_presentation as family;
        let row = gents_protocol::session::decode_session_row(&json!({
            "_docID": "session-document", "session_id": "chat", "node_did": "did:key:node",
            "requester_did": "did:key:user", "agent_did": "did:key:agent",
            "created_at": "2026-11-08T00:00:00Z",
        }))
        .unwrap();
        let number = |value: &str, prefix: &str| -> u64 {
            value.strip_prefix(prefix).unwrap().parse().unwrap()
        };
        gents_lean_vocab::families::goal_session_observation_contracts_presentation::replay::<String, String>(|case: &family::Case<String>| {
            let session = match case.session.as_str() {
                "absent" => AgentSessionRowRead::Absent,
                "present" => AgentSessionRowRead::Present(Box::new(row.clone())),
                other => panic!("unmodeled session read {other}"),
            };
            let projection = match case.projection.kind.as_str() {
                "missing" => gents_automation::goal::SessionGoalProjectionRead::Missing,
                "unverified" => gents_automation::goal::SessionGoalProjectionRead::Unverified,
                "verified" => gents_automation::goal::SessionGoalProjectionRead::Verified(
                    Box::new(modeled_observation(
                        &serde_json::to_value(&case.projection.goal).unwrap(),
                    )),
                ),
                other => panic!("unmodeled projection read {other}"),
            };
            let state = gents_automation::goal::observe_session_goal(&session, projection);
            let root = match &state {
                gents_automation::goal::SessionGoalObservationState::Observed(observation) => {
                    observation.goal.as_ref().and_then(|goal| {
                        goal.assignment_root_request_doc_id
                            .as_deref()
                            .map(|root| number(root, "request-document:"))
                    })
                }
                _ => None,
            };
            let dto = serde_json::to_value(session_goal_state(state, |observation| {
                goal_view(observation, None)
            }))
            .unwrap();
            let view = match dto["kind"].as_str().unwrap() {
                "observed" => match dto["observation"]["goal"]["goalId"].as_str() {
                    Some(goal_id) => family::ViewWire {
                        kind: "goal".into(),
                        goal: Some(family::Reference {
                            goal: number(goal_id, "goal:"),
                            root,
                        }),
                        cause: None,
                    },
                    None => family::ViewWire {
                        kind: "no_goal".into(),
                        goal: None,
                        cause: None,
                    },
                },
                "no_readable_session" => family::ViewWire {
                    kind: "unavailable".into(),
                    goal: None,
                    cause: Some("session_not_readable".into()),
                },
                "unknown" => {
                    let refused: gents_automation::goal::SessionGoalUnknownCause =
                        serde_json::from_value(dto["cause"].clone()).unwrap();
                    assert_eq!(
                        dto["reason"],
                        gents_automation::goal::GoalInputRefused::from(refused).to_string(),
                        "the view's reason is the refusal a send returns: {dto}"
                    );
                    family::ViewWire {
                        kind: "unavailable".into(),
                        goal: None,
                        cause: Some(dto["cause"].as_str().unwrap().to_owned()),
                    }
                }
                other => panic!("unmodeled view {other}"),
            };
            family::ViewOutput {
                view,
                next: serde_json::from_value(dto["next"].clone()).unwrap(),
            }
        });
    }

    #[test]
    fn budget_preserves_the_full_signed_integer_range() {
        assert_eq!(budget(Some(i64::MAX.to_string())).unwrap(), Some(i64::MAX));
        assert_eq!(budget(None).unwrap(), None);
        for invalid in ["0", "-1", "1.5", "1e6", "9223372036854775808", ""] {
            assert!(budget(Some(invalid.into())).is_err(), "{invalid}");
        }
    }

    #[test]
    fn applied_creation_and_resumption_keep_distinct_opening_provenance() {
        for (starts_goal, request_id, expected) in [
            (true, Some("opening"), Some("goal")),
            (false, Some("resumed"), None),
            (false, None, None),
            (true, None, None),
        ] {
            let result = goal_control_result(
                "chat".into(),
                starts_goal,
                GoalControlOutcome::Applied {
                    goal_id: "goal".into(),
                    request_id: request_id.map(str::to_owned),
                },
            )
            .unwrap();
            assert_eq!(result.started_goal_id.as_deref(), expected);
            assert_eq!(result.request_id.as_deref(), request_id);
        }
    }

    #[test]
    fn goal_control_rejection_keeps_its_reason_and_code_in_host_and_tauri_payloads() {
        let reason = "wait for this session to finish before starting a goal";
        let error = goal_control_result(
            "chat".into(),
            true,
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
