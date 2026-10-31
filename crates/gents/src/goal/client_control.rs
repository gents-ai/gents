use gents_protocol::goal_control::{GoalControlOutcome, GoalOperation, SessionGoalControl};
use gents_protocol::request_admission::{AgentRequestCreate, RequestPurpose};

use super::*;
#[cfg(test)]
mod contract_tests;
#[cfg(test)]
mod receipt_tests;
#[cfg(test)]
mod start_tests;
use crate::store::config_client::ConfigApplyTxn;
use crate::store::identity::NodeIdentity;

pub(crate) enum PreparedGoalControl {
    Create {
        objective: String,
        token_budget: Option<i64>,
        request: Box<AgentRequestCreate>,
        replacement: Option<(GoalDocument, GoalCreationIdentity)>,
    },
    Edit {
        goal: GoalDocument,
        fields: GoalEdit,
    },
    Pause(GoalDocument),
    Stop(GoalDocument),
    ResumeCurrent(GoalDocument),
    ResumeIdle {
        goal: GoalDocument,
        predecessor: String,
    },
}

/// Preflight performs no document writes. Once staging begins, every error
/// aborts the command transaction, including its receipt.
pub(crate) async fn prepare_goal_control(
    txn: &ConfigApplyTxn<'_>,
    command: &SessionGoalControl,
) -> Result<Result<PreparedGoalControl, String>> {
    crate::store::graphql::fence_arrivals(txn, "AgentSession").await?;
    crate::store::graphql::fence_arrivals(txn, "AgentRequest").await?;
    crate::store::graphql::fence_arrivals(txn, "Goal").await?;
    crate::store::graphql::fence_arrivals(txn, "GoalCreationClaim").await?;
    let session = crate::session::load_agent_session_owner_in_txn(
        txn,
        &command.node_did,
        &command.session_id,
    )
    .await?;
    if session.as_ref().map(|session| session.doc_id.as_str()) != command.session_doc_id.as_deref()
        || session.as_ref().is_some_and(|session| {
            session.session.requester_did.as_deref() != Some(command.requester_did.as_str())
        })
    {
        return Ok(Err(
            "session changed; refresh before changing its goal".into()
        ));
    }
    let goal = load_canonical_goal_in_txn(txn, &command.node_did, &command.session_id).await?;
    let requests = if matches!(
        command.operation,
        GoalOperation::Create { .. } | GoalOperation::Resume {}
    ) {
        super::request_head::load_session_requests(txn, &command.node_did, &command.session_id)
            .await?
    } else {
        Vec::new()
    };
    let idle = goal_session_is_idle(&requests);
    if let GoalOperation::Create {
        objective,
        token_budget,
    } = &command.operation
    {
        let replacement = match (goal, command.expected.as_ref()) {
            (None, None) => None,
            (Some(goal), Some(expected)) => {
                if !matches!(
                    goal.parsed_status(),
                    Some(GoalStatus::Complete | GoalStatus::Stopped)
                ) {
                    return Ok(Err(
                        "only a completed or removed goal can be replaced".into()
                    ));
                }
                let current = crate::store::graphql::newest_document_composite_commit_in_txn(
                    txn,
                    &goal.doc_id,
                    "goal.replace_preflight",
                )
                .await?
                .context("goal has no unambiguous composite revision")?;
                if expected.document != goal.doc_id || expected.composite != current.cid {
                    return Ok(Err(
                        "goal changed; refresh before starting another goal".into()
                    ));
                }
                let identity = replacement_goal_identity(
                    &command.node_did,
                    &command.session_id,
                    &command.command_id,
                );
                let response = txn.execute(&format!(r#"{{
                    Goal(filter: {{node_did: {{_eq: "{}"}}, goal_id: {{_eq: "{}"}}}}, showDeleted: true) {{_docID}}
                    GoalCreationClaim(filter: {{creation_key: {{_eq: "{}"}}}}, showDeleted: true) {{_docID}}
                }}"#, escape_graphql_string(&command.node_did), escape_graphql_string(&identity.goal_id),
                    escape_graphql_string(&identity.creation_key))).await?;
                if ["Goal", "GoalCreationClaim"].into_iter().any(|collection| {
                    response["data"][collection]
                        .as_array()
                        .is_none_or(|rows| !rows.is_empty())
                }) {
                    return Ok(Err("the replacement goal identity is already in use".into()));
                }
                Some((goal, identity))
            }
            _ => {
                return Ok(Err(
                    "this chat's goal changed; refresh before starting another goal".into(),
                ))
            }
        };
        if !idle {
            return Ok(Err(
                "wait for this session to finish before starting a goal".into(),
            ));
        }
        let claim = if replacement.is_some() {
            None
        } else {
            load_goal_creation_claim_fingerprint_in_txn(txn, &command.node_did, &command.session_id)
                .await?
        };
        let creation = GoalCreateRequest {
            caller: command.node_did.clone(),
            current_session: command.session_id.clone(),
            requested_owner: command.node_did.clone(),
            requested_session: command.session_id.clone(),
            objective: objective.clone(),
            objective_nonempty: !objective.is_empty(),
            token_budget: token_budget.map(i128::from),
            goal_tools: true,
            goal_create: true,
        };
        if !matches!(
            decide_model_goal_create(&creation, claim.as_ref()),
            GoalCreateDisposition::Fresh | GoalCreateDisposition::Idempotent
        ) {
            return Ok(Err("goal creation conflicts with this session's creation claim or has an invalid objective or budget; use a new session for a different goal".into()));
        }
        let Some(request) = &command.initial_request else {
            return Ok(Err("goal creation lacks its signed first request".into()));
        };
        if replacement.is_some()
            && requests.iter().any(|prior| {
                prior.request_id == request.request_id || prior.retry_key == request.retry_key
            })
        {
            return Ok(Err(
                "the new goal's first request has already been used".into()
            ));
        }
        if let Err(error) = request.graphql_input_fields() {
            return Ok(Err(error.into()));
        }
        let expected_goal_id = replacement
            .as_ref()
            .map(|(_, identity)| identity.goal_id.clone())
            .unwrap_or_else(|| deterministic_goal_id(&command.node_did, &command.session_id));
        let first_assignment = gents_protocol::request_input::GoalAssignmentInput {
            goal_id: expected_goal_id,
            root_request_doc_id: None,
        };
        if request.node_did != command.node_did
            || request.session_id != command.session_id
            || request.requester_did != command.requester_did
            || request.purpose != RequestPurpose::Normal
            || request.execution_origin != "interactive"
            || request.input.goal_continuation.is_some()
            || request.input.goal_assignment.as_ref() != Some(&first_assignment)
            || !request.input.queue.as_ref().is_some_and(|queue| {
                queue.source == gents_protocol::request_input::QueueSource::User
                    && queue.policy == gents_protocol::request_input::QueuePolicy::Append
                    && queue.position.is_none()
                    && queue.key.is_none()
                    && queue.interrupted_request_id.is_none()
                    && queue.background_completion_wake_version.is_none()
            })
            || request.retry_parent_request.is_some()
            || request.caused_by_trigger_id.is_some()
            || request.caused_by_source_doc_id.is_some()
            || request.caused_by_parent_tool_call_doc_id.is_some()
            || request.retry_key.as_deref()
                != Some(format!("session-goal:{}", command.command_id).as_str())
            || session
                .as_ref()
                .is_some_and(|session| session.session.agent_id != request.agent_id)
        {
            return Ok(Err(
                "initial goal request does not match its session and author".into(),
            ));
        }
        return Ok(Ok(PreparedGoalControl::Create {
            objective: objective.clone(),
            token_budget: *token_budget,
            request: request.clone(),
            replacement,
        }));
    }
    let Some(goal) = goal else {
        return Ok(Err("this session has no goal".into()));
    };
    let Some(expected) = command.expected.as_ref() else {
        return Ok(Err("goal control lacks its observed revision".into()));
    };
    let current = crate::store::graphql::newest_document_composite_commit_in_txn(
        txn,
        &goal.doc_id,
        "goal.control_preflight",
    )
    .await?
    .context("goal has no unambiguous composite revision")?;
    if expected.document != goal.doc_id || expected.composite != current.cid {
        return Ok(Err(
            "goal changed; refresh before applying this control".into()
        ));
    }
    let Some(state) = goal.state() else {
        return Ok(Err("goal has an unknown status".into()));
    };
    let prepared = match &command.operation {
        GoalOperation::Edit {
            objective,
            token_budget,
        } => {
            let fields = GoalEdit {
                objective: objective.clone(),
                budget: *token_budget,
            };
            if let Err(error) = fields.validate() {
                return Ok(Err(error.to_string()));
            }
            if !super::edit::GoalEditOrigin::Client.can_edit_status(state.status) {
                return Ok(Err("only an active or paused goal can be edited".into()));
            }
            PreparedGoalControl::Edit { goal, fields }
        }
        GoalOperation::Pause {} => {
            if state.step(GoalAction::Pause).is_none() {
                return Ok(Err("goal status does not allow pause".into()));
            }
            PreparedGoalControl::Pause(goal)
        }
        GoalOperation::Stop {} => {
            if state.step(GoalAction::Stop).is_none() {
                return Ok(Err("goal status does not allow removal".into()));
            }
            PreparedGoalControl::Stop(goal)
        }
        GoalOperation::Resume {} => match prepare_goal_resume(goal, state, &requests) {
            Ok(prepared) => prepared,
            Err(error) => return Ok(Err(error)),
        },
        GoalOperation::Create { .. } => unreachable!(),
    };
    Ok(Ok(prepared))
}

pub(crate) fn prepare_goal_resume(
    goal: GoalDocument,
    state: GoalState,
    requests: &[gents_protocol::row::AgentRequestRow],
) -> Result<PreparedGoalControl, String> {
    let idle = goal_session_is_idle(requests);
    if state.step(GoalAction::Resume).is_none() || (!idle && state.status != GoalStatus::Paused) {
        return Err("goal status does not allow resume".into());
    }
    if !idle {
        return Ok(PreparedGoalControl::ResumeCurrent(goal));
    }
    let Some(parent) = latest_goal_request(&goal, requests) else {
        return Err("goal has no authenticated predecessor to resume".into());
    };
    if !parent
        .lifecycle_state
        .is_some_and(RequestLifecycleState::is_terminal)
    {
        return Err("goal predecessor is not terminal".into());
    }
    Ok(PreparedGoalControl::ResumeIdle {
        predecessor: parent.request_id.clone(),
        goal,
    })
}

pub(crate) async fn stage_goal_control(
    txn: &ConfigApplyTxn<'_>,
    identity: &dyn NodeIdentity,
    prepared: PreparedGoalControl,
) -> Result<GoalControlOutcome> {
    let node_did = match &prepared {
        PreparedGoalControl::Create { request, .. } => &request.node_did,
        PreparedGoalControl::Edit { goal, .. }
        | PreparedGoalControl::Pause(goal)
        | PreparedGoalControl::Stop(goal)
        | PreparedGoalControl::ResumeCurrent(goal)
        | PreparedGoalControl::ResumeIdle { goal, .. } => &goal.node_did,
    };
    anyhow::ensure!(
        identity.did() == node_did,
        "goal control requires its node signing identity"
    );
    let (goal_id, request_id) = match prepared {
        PreparedGoalControl::Create {
            objective,
            token_budget,
            request,
            replacement,
        } => {
            crate::session::ensure_session_in_txn(
                txn,
                &request.session_id,
                &request.node_did,
                &request.agent_id,
                Some(&request.requester_did),
                request.input.initial_title.clone(),
                None,
                &request.created_at,
            )
            .await?;
            if let Some((prior, _)) = &replacement {
                let response = txn
                    .execute(&format!(
                        r#"mutation {{ delete_Goal(docID: "{}") {{_docID}} }}"#,
                        escape_graphql_string(&prior.doc_id)
                    ))
                    .await?;
                anyhow::ensure!(
                    mutation_row_count(&response["data"]["delete_Goal"]) == 1,
                    "terminal goal was not retired"
                );
            }
            let submitted = stage_goal_backed_request_with_identity_in_txn(
                txn,
                &request.node_did,
                &request.session_id,
                &objective,
                token_budget,
                &request,
                replacement.as_ref().map(|(_, identity)| identity),
            )
            .await?;
            let goal = load_canonical_goal_in_txn(txn, &request.node_did, &request.session_id)
                .await?
                .context("new goal disappeared during creation")?;
            anyhow::ensure!(
                replacement
                    .as_ref()
                    .is_none_or(|(prior, identity)| goal.goal_id == identity.goal_id
                        && goal.doc_id != prior.doc_id
                        && prior.assignment_root_request_doc_id.as_deref()
                            != Some(&submitted.doc_id)),
                "replacement goal reused its prior identity or ancestry"
            );
            (goal.goal_id, Some(submitted.request_id))
        }
        PreparedGoalControl::Edit { goal, fields } => {
            let updated = super::edit::stage_goal_edit(
                txn,
                &goal,
                &fields,
                super::edit::GoalEditOrigin::Client,
            )
            .await?
            .context("goal changed while staging its edit")?;
            (updated.goal_id, None)
        }
        operation @ (PreparedGoalControl::Pause(_) | PreparedGoalControl::Stop(_)) => {
            let (goal, status) = match operation {
                PreparedGoalControl::Pause(goal) => (goal, GoalStatus::Paused),
                PreparedGoalControl::Stop(goal) => (goal, GoalStatus::Stopped),
                _ => unreachable!(),
            };
            let updated = set_goal_in_txn(
                txn,
                &goal.node_did,
                &goal.session_id,
                None,
                Some(status),
                None,
                None,
            )
            .await?;
            (updated.goal_id, None)
        }
        PreparedGoalControl::ResumeCurrent(goal) => {
            super::operator_resume::stage_busy_resume(txn, identity, &goal).await?;
            (goal.goal_id, None)
        }
        PreparedGoalControl::ResumeIdle { goal, predecessor } => {
            let resumed = super::operator_resume::stage_resume(
                txn,
                identity,
                &goal.node_did,
                &goal.session_id,
                &predecessor,
            )
            .await?;
            (resumed.goal_id, Some(resumed.request_id))
        }
    };
    Ok(GoalControlOutcome::Applied {
        goal_id,
        request_id,
    })
}
