use gents_protocol::goal_control::{GoalControlOutcome, GoalOperation, SessionGoalControl};
use gents_protocol::request_admission::AgentRequestCreate;

use super::*;
#[cfg(test)]
mod contract_tests;
#[cfg(test)]
mod receipt_tests;
#[cfg(test)]
mod start_tests;
use gents_store::config_client::ConfigApplyTxn;
use gents_store::identity::NodeIdentity;

#[cfg(any(test, feature = "test-fixtures"))]
pub async fn agent_document_for_test(
    access: &gents_store::config_client::ConfigAccess,
    node: &str,
    agent: &str,
) -> String {
    access
        .transact_readonly("test.goal.agent_document", |txn| {
            Box::pin(async move {
                gents_documents::read_desired_state_record_in_txn(
                    txn,
                    gents_store::collection::Collection::Agent,
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
    Set(Box<crate::operations::goal::GoalSetPlan>),
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
    let input = gents_store::config_client::client_request_input(request)?;
    let graph =
        match gents_store::config_client::verify_request_evidence(input, evidence.request.clone())
            .await
        {
            Ok(graph) if graph.signer_did() == command.requester_did => std::sync::Arc::new(graph),
            Ok(_)
            | Err(
                gents_store::config_client::ConfigClientError::ClientAgentGraph(_)
                | gents_store::config_client::ConfigClientError::Json(_),
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
            let graph = match gents_store::config_client::verify_goal_delete_evidence(
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
                    gents_store::config_client::ConfigClientError::ClientVerifiedMutationGraph(_)
                    | gents_store::config_client::ConfigClientError::Json(_),
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
    gents_store::graphql::fence_arrivals(txn, "AgentSession").await?;
    gents_store::graphql::fence_arrivals(txn, "AgentRequest").await?;
    gents_store::graphql::fence_arrivals(txn, "Goal").await?;
    gents_store::graphql::fence_arrivals(txn, "GoalCreationClaim").await?;
    let session = gents_store::config_client::agent_session::load_agent_session_owner_in_txn(
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
    let actor = crate::operations::goal::acting_as_signer(
        txn,
        &command.node_did,
        &session.session.agent_did,
        &command.requester_did,
    )
    .await?;
    if let GoalOperation::Create {
        objective,
        token_budget,
    } = &command.operation
    {
        let admission = match crate::operations::goal::admit_start_in_txn(
            txn,
            actor,
            &command.node_did,
            &command.session_id,
            objective,
            *token_budget,
            command.expected.as_ref(),
            None,
        )
        .await?
        {
            Ok(admission) => admission,
            Err(refusal) => return Ok(Err(refusal.to_string())),
        };
        let replacement = match (admission.decision, admission.existing) {
            (gents_protocol::operations::goal::GoalStartDecision::Replace, Some(goal)) => {
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
            _ => None,
        };
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
        let expected_goal_id = replacement
            .as_ref()
            .map(|(_, identity)| identity.goal_id.clone())
            .unwrap_or_else(|| deterministic_goal_id(&command.node_did, &command.session_id));
        if let Err(refusal) = crate::operations::goal::check_first_request(
            request,
            &command.node_did,
            &command.session_id,
            &session.session.agent_did,
            session.session.requester_did.as_deref(),
            &command.requester_did,
            &expected_goal_id,
            Some(&format!("session-goal:{}", command.command_id)),
        ) {
            return Ok(Err(refusal.to_string()));
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
    let input = |status: Option<GoalStatus>, objective: Option<String>, budget: Option<i64>| {
        gents_protocol::operations::goal::GoalSetInput {
            objective,
            token_budget: budget.map(Some),
            status,
            expected: Some(gents_protocol::operations::goal::GoalExpected::Revision(
                expected.clone(),
            )),
            ..Default::default()
        }
    };
    let input = match &command.operation {
        GoalOperation::Edit {
            objective,
            token_budget,
        } => input(None, objective.clone(), *token_budget),
        GoalOperation::Pause {} => input(Some(GoalStatus::Paused), None, None),
        GoalOperation::Stop {} => input(Some(GoalStatus::Stopped), None, None),
        GoalOperation::Resume {} => {
            let current = gents_store::graphql::newest_document_composite_commit_in_txn(
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
            return Ok(prepare_goal_resume(goal, state, &requests));
        }
        GoalOperation::Create { .. } => unreachable!(),
    };
    Ok(
        match crate::operations::goal::admit_set_in_txn(txn, actor, &goal, &input, None, None)
            .await?
        {
            Ok(plan) => Ok(PreparedGoalControl::Set(Box::new(plan))),
            Err(refusal) => Err(refusal.to_string()),
        },
    )
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
            gents_session::session::ensure_session_in_txn(
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
        PreparedGoalControl::Set(plan) => {
            let updated = crate::operations::goal::apply_set_in_txn(txn, &plan)
                .await?
                .context("goal changed while staging its control")?;
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
