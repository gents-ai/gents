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

#[cfg(test)]
pub(crate) async fn agent_document_for_test(
    access: &crate::store::config_client::ConfigAccess,
    node: &str,
    agent: &str,
) -> String {
    access
        .transact_readonly("test.goal.agent_document", |txn| {
            Box::pin(async move {
                crate::store::config_client::read_desired_state_record_in_txn(
                    txn,
                    crate::Collection::Agent,
                    node,
                    agent,
                )
                .await
            })
        })
        .await
        .unwrap()
        .unwrap()
        .0
}

pub(crate) enum PreparedGoalControl {
    Create {
        objective: String,
        token_budget: Option<i64>,
        request: Box<AgentRequestCreate>,
        replacement: Option<(GoalDocument, GoalCreationIdentity)>,
        client_graphs: Option<GoalClientGraphs>,
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

pub(crate) struct GoalClientGraphs {
    request: std::sync::Arc<db::block::builder::VerifiedClientCreate>,
    replacement_delete: Option<std::sync::Arc<db::block::builder::VerifiedClientMutation>>,
}

pub(crate) async fn attach_client_goal_graphs(
    txn: &ConfigApplyTxn<'_>,
    command: &SessionGoalControl,
    prepared: &mut PreparedGoalControl,
) -> Result<Result<(), String>> {
    let PreparedGoalControl::Create {
        request,
        replacement,
        client_graphs,
        ..
    } = prepared
    else {
        return Ok(Err("native Goal graphs require a creation".into()));
    };
    let Some(evidence) = command.native_evidence.as_ref() else {
        return Ok(Err("goal creation has no native graph evidence".into()));
    };
    if txn.managed_agent_signer().is_none()
        || txn.actor_did() != Some(command.agent_did.as_str())
        || request.admission.signer_did != command.requester_did
        || request.agent_did != command.agent_did
        || request.agent_doc_id.as_deref() != Some(command.agent_doc_id.as_str())
        || request.session_doc_id != command.session_doc_id
        || command.initial_request.as_deref() != Some(request.as_ref())
    {
        return Ok(Err("goal creation actor or physical targets differ".into()));
    }
    let input = crate::store::config_client::client_request_input(request)?;
    let graph =
        match crate::store::config_client::verify_request_evidence(input, evidence.request.clone())
            .await
        {
            Ok(graph) if graph.signer_did() == command.requester_did => std::sync::Arc::new(graph),
            Ok(_)
            | Err(
                crate::store::config_client::ConfigClientError::ClientAgentGraph(_)
                | crate::store::config_client::ConfigClientError::Json(_),
            ) => return Ok(Err("invalid native goal request evidence".into())),
            Err(error) => return Err(error.into()),
        };
    let deletion = match (replacement, evidence.replacement_delete.as_ref()) {
        (None, None) => None,
        (Some((prior, _)), Some(evidence)) => {
            let subject = ::identity::Did::new(&command.requester_did)?;
            let observed = txn
                .observe_document_access(
                    &subject,
                    "Goal",
                    std::slice::from_ref(&prior.doc_id),
                    defra_node::DocumentAccessMode::Named {
                        permission: "delete".into(),
                        node_permission: acp::nac::NodePermission::DocumentDelete,
                    },
                )
                .await?;
            match observed.as_slice() {
                [defra_node::DocumentAccessDecision::Allowed] => {}
                [defra_node::DocumentAccessDecision::Denied] => {
                    return Ok(Err("permission denied or goal unavailable".into()))
                }
                _ => anyhow::bail!("native goal deletion access is unavailable"),
            }
            let graph = match crate::store::config_client::verify_goal_delete_evidence(
                txn,
                &prior.doc_id,
                evidence.clone(),
            )
            .await
            {
                Ok(graph)
                    if graph.signer_did() == command.requester_did
                        && graph.doc_id() == prior.doc_id =>
                {
                    graph
                }
                Ok(_)
                | Err(
                    crate::store::config_client::ConfigClientError::ClientVerifiedMutationGraph(_)
                    | crate::store::config_client::ConfigClientError::Json(_),
                ) => return Ok(Err("invalid or stale native goal deletion evidence".into())),
                Err(error) => return Err(error.into()),
            };
            Some(std::sync::Arc::new(graph))
        }
        _ => {
            return Ok(Err(
                "goal replacement evidence does not match its target".into()
            ))
        }
    };
    *client_graphs = Some(GoalClientGraphs {
        request: graph,
        replacement_delete: deletion,
    });
    Ok(Ok(()))
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
    let Some(session) = session else {
        return Ok(Err("create the session before starting its goal".into()));
    };
    if command.session_doc_id.is_none() {
        return Ok(Err("create the session before starting its goal".into()));
    }
    if Some(session.doc_id.as_str()) != command.session_doc_id.as_deref() {
        return Ok(Err(
            "session changed; refresh before changing its goal".into()
        ));
    }
    if session.session.agent_did != command.agent_did {
        return Ok(Err("permission denied or session unavailable".into()));
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
                    Goal(filter: {{session_doc_id: {{_eq: "{}"}}, goal_id: {{_eq: "{}"}}}}, showDeleted: true) {{_docID}}
                    GoalCreationClaim(filter: {{session_doc_id: {{_eq: "{}"}}, creation_key: {{_eq: "{}"}}}}, showDeleted: true) {{_docID}}
                }}"#, escape_graphql_string(&session.doc_id), escape_graphql_string(&identity.goal_id),
                    escape_graphql_string(&session.doc_id), escape_graphql_string(&identity.creation_key))).await?;
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
            || request.requester_did
                != session
                    .session
                    .requester_did
                    .as_deref()
                    .unwrap_or(&command.requester_did)
            || request.admission.signer_did != command.requester_did
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
            || session.session.agent_did != request.agent_did
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
            client_graphs: None,
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

fn client_goal_transaction_actor_matches(transaction_actor: Option<&str>, requester: &str) -> bool {
    !requester.is_empty() && transaction_actor == Some(requester)
}

pub(crate) async fn stage_goal_control(
    txn: &ConfigApplyTxn<'_>,
    identity: &dyn NodeIdentity,
    prepared: PreparedGoalControl,
) -> Result<GoalControlOutcome> {
    let (goal_id, request_id) = match prepared {
        PreparedGoalControl::Create {
            objective,
            token_budget,
            request,
            replacement,
            client_graphs,
        } => {
            anyhow::ensure!(
                client_goal_transaction_actor_matches(txn.actor_did(), &request.admission.signer_did)
                    || client_graphs.is_some(),
                "client Goal creation requires the requester's native transaction signer or a verified client graph handoff"
            );
            crate::session::ensure_session_in_txn(
                txn,
                &request.session_id,
                &request.node_did,
                &request.agent_did,
                Some(&request.requester_did),
                request.input.initial_title.clone(),
                None,
                &request.created_at,
            )
            .await?;
            if let Some((prior, _)) = &replacement {
                if let Some(graphs) = client_graphs.as_ref() {
                    let graph = graphs
                        .replacement_delete
                        .as_ref()
                        .context("verified replacement lacks its native deletion")?;
                    txn.delete_verified_configuration_document(
                        gents_protocol::schemas::GOAL_NAME,
                        &prior.doc_id,
                        graph.clone(),
                    )
                    .await?;
                } else {
                    txn.delete_configuration_document(
                        gents_protocol::schemas::GOAL_NAME,
                        &prior.doc_id,
                    )
                    .await?;
                }
            }
            let submitted = stage_goal_backed_request_with_identity_in_txn(
                txn,
                &request.node_did,
                &request.session_id,
                &objective,
                token_budget,
                &request,
                replacement.as_ref().map(|(_, identity)| identity),
                client_graphs.as_ref().map(|graphs| &graphs.request),
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
