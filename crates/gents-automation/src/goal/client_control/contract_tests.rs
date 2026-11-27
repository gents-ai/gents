use super::*;
use crate::goal::operator_resume::support::FixtureNode;
use crate::goal::session_goal_control::process_command;
use gents_protocol::goal_control::{
    GoalRevision, SessionGoalControlReceipt, SESSION_GOAL_CONTROL_VERSION,
};
use std::sync::Arc;

pub(super) async fn persist_command(
    access: &gents_store::config_client::ConfigAccess,
    command: &SessionGoalControl,
) -> String {
    try_persist_command(access, command).await.unwrap()
}

pub(super) async fn try_persist_command(
    access: &gents_store::config_client::ConfigAccess,
    command: &SessionGoalControl,
) -> anyhow::Result<String> {
    let document = crate::goal::client_submission::publish(access, command).await?;
    crate::goal::client_submission::grant_command_relationships(access, command, &document).await?;
    Ok(document)
}

pub(super) async fn receipt(
    access: &gents_store::config_client::ConfigAccess,
    doc_id: &str,
) -> Option<SessionGoalControlReceipt> {
    let response = access.execute(&format!(r#"{{ AgentSessionGoalReceipt(filter: {{ command_doc_id: {{ _eq: "{}" }} }}) {{ receipt_json }} }}"#,
        escape_graphql_string(doc_id))).await.unwrap();
    response["data"]["AgentSessionGoalReceipt"][0]["receipt_json"]
        .as_str()
        .map(|raw| serde_json::from_str(raw).unwrap())
}

const CASES_PER_NODE: usize = 19;
const NODES_AT_ONCE: usize = 4;

fn modeled_operation(kind: &str, objective: &Option<String>, budget: Option<i64>) -> GoalOperation {
    match kind {
        "create" => GoalOperation::Create {
            objective: objective.clone().unwrap(),
            token_budget: budget,
        },
        "edit" => GoalOperation::Edit {
            objective: objective.clone(),
            token_budget: budget,
        },
        "pause" => GoalOperation::Pause {},
        "resume" => GoalOperation::Resume {},
        "stop" => GoalOperation::Stop {},
        other => panic!("unknown modeled goal operation {other}"),
    }
}

#[test]
fn generated_goal_client_controls_match_decision_owner() {
    use gents_lean_vocab::families::goal_client_control_contracts_control as family;
    for modeled_case in family::cases::<String>() {
        let case = &modeled_case.input;
        let goal = GoalDocument {
            doc_id: case.before.revision.document.clone(),
            goal_id: "goal-1".into(),
            session_id: "session".into(),
            node_did: "owner".into(),
            objective: case.before.objective.clone(),
            status: case.before.execution.status.clone(),
            token_budget: case
                .before
                .execution
                .token_budget
                .map(|value| i64::try_from(value).unwrap()),
            assignment_root_request_doc_id: None,
            tokens_used: Some(i64::try_from(case.before.execution.tokens_used).unwrap()),
            token_usage_baseline: None,
            active_time_seconds: Some(0),
            active_started_at: None,
            blocked_reason: None,
            last_continued_from_request_id: case
                .before
                .execution
                .last_continued_from
                .map(|id| format!("req-{id}")),
            continuation_sequence: Some(i64::try_from(case.before.execution.sequence).unwrap()),
            wrapup_requested: Some(case.before.execution.wrapup_requested),
            wrapup_completed: Some(case.before.execution.wrapup_completed),
            infrastructure_retry_count: Some(0),
            max_infrastructure_retries: None,
            last_failure: None,
            completion_evidence: None,
            auto_resume_at_reset: None,
            created_at: Some("2026-01-01T00:00:00Z".into()),
            updated_at: Some("2026-01-01T00:00:00Z".into()),
            progress_record: None,
        };
        let parent_state = if case.facts.unfinished_request {
            "processing"
        } else {
            "completed"
        };
        let parent_row: gents_protocol::row::AgentRequestRow =
            serde_json::from_value(serde_json::json!({
                "_docID": "req-doc-10",
                "request_id": "req-10",
                "purpose": "normal",
                "node_did": "owner",
                "requester_did": "user",
                "agent_did": "agent",
                "session_id": "session",
                "session_doc_id": "session-doc",
                "agent_doc_id": "agent-doc",
                "content": "Original context",
                "execution_origin": "interactive",
                "lifecycle_state": parent_state,
                "created_at": "2020-01-01T00:00:00Z",
                "caused_by_correlation": "graph-correlation",
                "caused_by_source_doc_id": "source",
                "caused_by_trigger_context": "{\"contract\":\"context\"}",
            }))
            .unwrap();
        let requests = [parent_row.clone()];
        let current_revision = GoalRevision {
            document: case.before.revision.document.clone(),
            composite: case.before.revision.composite.clone(),
        };
        let expected_revision = GoalRevision {
            document: case.expected_revision.document.clone(),
            composite: case.expected_revision.composite.clone(),
        };
        let operation = modeled_operation(
            &case.operation.kind,
            &case.operation.objective,
            case.operation.budget,
        );
        let observed = match super::plan_existing_goal_control(
            case.facts.authorized,
            gents_protocol::operations::goal::GoalActor::Person,
            goal,
            Some(&current_revision),
            &expected_revision,
            &operation,
            &requests,
        ) {
            Err(_) => None,
            Ok(PreparedGoalControl::Set(plan)) => {
                let mut execution = case.before.execution.clone();
                execution.status = plan.post.status.as_str().to_owned();
                execution.wrapup_requested = plan.post.wrapup_requested;
                execution.wrapup_completed = plan.post.wrapup_completed;
                if let Some(budget) = plan.token_budget {
                    execution.token_budget = budget.map(|value| u64::try_from(value).unwrap());
                }
                Some(family::GoalSnapshot {
                    revision: family::Revision {
                        document: case.before.revision.document.clone(),
                        composite: case.next_composite.clone(),
                    },
                    execution,
                    objective: plan
                        .objective
                        .unwrap_or_else(|| case.before.objective.clone()),
                })
            }
            Ok(PreparedGoalControl::ResumeCurrent(goal)) => {
                let post = crate::goal::operator_resume::busy_resume_post(&goal).unwrap();
                let mut execution = case.before.execution.clone();
                execution.status = post.status.as_str().to_owned();
                execution.wrapup_requested = post.wrapup_requested;
                execution.wrapup_completed = post.wrapup_completed;
                Some(family::GoalSnapshot {
                    revision: family::Revision {
                        document: case.before.revision.document.clone(),
                        composite: case.next_composite.clone(),
                    },
                    execution,
                    objective: goal.objective,
                })
            }
            Ok(PreparedGoalControl::ResumeIdle { goal, predecessor }) => {
                let parent = gents_protocol::row::AgentRequest::try_from(parent_row).unwrap();
                match crate::goal::operator_resume::decide_idle_resume(
                    &goal,
                    &requests,
                    &requests[0],
                    &parent,
                    "agent".into(),
                    Utc::now(),
                    0,
                ) {
                    Err(_) => None,
                    Ok(idle) => {
                        assert_eq!(
                            idle.create.caused_by_parent_request_id.as_deref(),
                            Some(predecessor.as_str())
                        );
                        let mut execution = case.before.execution.clone();
                        execution.status = idle.post.status.as_str().to_owned();
                        execution.wrapup_requested = idle.post.wrapup_requested;
                        execution.wrapup_completed = idle.post.wrapup_completed;
                        execution.sequence = u64::try_from(idle.sequence).unwrap();
                        execution.last_continued_from = Some(10);
                        execution.latest_request = 20;
                        execution
                            .children
                            .push(case.facts.resume.as_ref().unwrap().binding.clone());
                        Some(family::GoalSnapshot {
                            revision: family::Revision {
                                document: case.before.revision.document.clone(),
                                composite: case.next_composite.clone(),
                            },
                            execution,
                            objective: goal.objective,
                        })
                    }
                }
            }
            Ok(PreparedGoalControl::Create { .. }) => unreachable!(),
        };
        modeled_case.check(observed);
    }
}

type SignedTransactionCase = gents_lean_vocab::family::Case<
    gents_lean_vocab::families::goal_client_control_contracts_signed_control_transactions::ControlInput<String>,
    Option<
        gents_lean_vocab::families::goal_client_control_contracts_signed_control_transactions::GoalSnapshot<String>,
    >,
>;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn generated_goal_client_control_transactions_drive_signed_commands() {
    use futures::StreamExt;
    let cases =
        gents_lean_vocab::families::goal_client_control_contracts_signed_control_transactions::cases::<
            String,
        >();
    let mut chunks: Vec<Vec<SignedTransactionCase>> = Vec::new();
    let mut iter = cases.into_iter();
    loop {
        let chunk: Vec<_> = iter.by_ref().take(CASES_PER_NODE).collect();
        if chunk.is_empty() {
            break;
        }
        chunks.push(chunk);
    }
    futures::stream::iter(chunks)
        .map(|chunk| {
            tokio::spawn(async move {
                let node = FixtureNode::start_with_shared_agent().await;
                for case in &chunk {
                    drive_signed_command(&node, case).await;
                }
                node.shutdown().await;
            })
        })
        .buffer_unordered(NODES_AT_ONCE)
        .for_each(|result| async { result.unwrap() })
        .await;
}

async fn drive_signed_command(node: &FixtureNode, modeled_case: &SignedTransactionCase) {
    let case = &modeled_case.input;
    let before_execution = serde_json::to_value(&case.before.execution).unwrap();
    let terminal_parent = !case.facts.unfinished_request;
    let fixture = node
        .fixture_for_client_control(&before_execution, &case.before.objective, terminal_parent)
        .await;
    let (requester, access) = if !case.facts.authorized {
        node.denied_requester_for(&fixture).await
    } else {
        (fixture.user.clone(), fixture.user_access())
    };
    let revision = fixture.goal_commit(&fixture.goal.doc_id).await.cid;
    let before = fixture.observe().await;
    let operation = modeled_operation(
        &case.operation.kind,
        &case.operation.objective,
        case.operation.budget,
    );
    let now = Utc::now();
    let mut command = SessionGoalControl {
        version: SESSION_GOAL_CONTROL_VERSION,
        command_id: uuid::Uuid::new_v4().to_string(),
        requester_did: requester.did().into(),
        requester_peer_id: None,
        node_did: fixture.identity.did().into(),
        agent_did: fixture.agent.did().into(),
        agent_doc_id: fixture.agent_doc_id.clone(),
        session_id: fixture.session.clone(),
        session_doc_id: Some(fixture.session_doc_id.clone()),
        issued_at: now.to_rfc3339(),
        expires_at: (now + chrono::Duration::minutes(5)).to_rfc3339(),
        expected: Some(GoalRevision {
            document: if case.expected_revision.document == case.before.revision.document {
                fixture.goal.doc_id.clone()
            } else {
                "foreign-goal-document".into()
            },
            composite: if case.expected_revision.composite == case.before.revision.composite {
                revision.clone()
            } else {
                "stale-goal-commit".into()
            },
        }),
        operation,
        initial_request: None,
        native_evidence: None,
        signature: Vec::new(),
    };
    command.signature = requester
        .sign(&command.signing_payload().unwrap())
        .await
        .unwrap();
    let doc_id = persist_command(&access, &command).await;
    let identity: Arc<dyn NodeIdentity> = fixture.identity.clone();
    assert_eq!(
        process_command(
            &fixture.node,
            &identity,
            &doc_id,
            fixture.agent.as_ref(),
            None
        )
        .await
        .unwrap(),
        None,
        "{}",
        case.name
    );
    let receipt = receipt(&access, &doc_id).await.unwrap();
    receipt.validate_for(&command).unwrap();
    assert!(identity
        .verify(
            &receipt.agent_did,
            &receipt.signing_payload().unwrap(),
            &receipt.signature
        )
        .await
        .unwrap());
    let observed = match &receipt.outcome {
        GoalControlOutcome::Applied { .. } => {
            Some(serde_json::from_value(fixture.control_snapshot(&revision).await).unwrap())
        }
        GoalControlOutcome::Rejected { .. } => {
            assert_eq!(
                fixture.observe().await,
                before,
                "{} unchanged execution",
                case.name
            );
            assert_eq!(
                fixture.current_goal().await.objective,
                case.before.objective,
                "{} unchanged objective",
                case.name
            );
            assert_eq!(
                fixture.goal_commit(&fixture.goal.doc_id).await.cid,
                revision,
                "{} no goal write",
                case.name
            );
            None
        }
    };
    modeled_case.check(observed);
    let first_receipt = serde_json::to_value(receipt).unwrap();
    process_command(
        &fixture.node,
        &identity,
        &doc_id,
        fixture.agent.as_ref(),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        serde_json::to_value(self::receipt(&access, &doc_id).await.unwrap()).unwrap(),
        first_receipt,
        "{} receipt replay",
        case.name
    );
}

#[tokio::test]
async fn goal_command_signature_binds_modeled_physical_targets() {
    let directory = tempfile::tempdir().unwrap();
    let user =
        gents_store::identity::KeyIdentity::load_or_create(directory.path().join("user.key"), None)
            .unwrap();
    for case in
        gents_lean_vocab::families::cross_actor_authorization_goal_command_signed_targets::cases()
    {
        let input = &case.input;
        let mut command = SessionGoalControl {
            version: SESSION_GOAL_CONTROL_VERSION,
            command_id: "request-doc".into(),
            requester_did: user.did().into(),
            requester_peer_id: None,
            node_did: "node".into(),
            agent_did: "agent".into(),
            agent_doc_id: input.signed_agent_document.clone(),
            session_id: "session".into(),
            session_doc_id: Some(input.signed_session.clone()),
            issued_at: "2026-11-04T00:00:00Z".into(),
            expires_at: "2026-11-04T00:01:00Z".into(),
            expected: Some(GoalRevision {
                document: "goal".into(),
                composite: "revision".into(),
            }),
            operation: GoalOperation::Pause {},
            initial_request: None,
            native_evidence: None,
            signature: Vec::new(),
        };
        command.signature = user
            .sign(&command.signing_payload().unwrap())
            .await
            .unwrap();
        let mut changed = command.clone();
        changed.session_doc_id = Some(input.session.clone());
        changed.agent_doc_id = input.agent_document.clone();
        let verified = user
            .verify(
                user.did(),
                &changed.signing_payload().unwrap(),
                &command.signature,
            )
            .await
            .unwrap_or(false);
        case.check(verified);
    }
}

/// The runtime command owner answers a client's idle resume by publishing the
/// continuation as the selected Agent; each modeled reader then reads that
/// request on the served node as the session model says.
#[tokio::test]
async fn generated_resume_continuation_is_read_through_the_session() {
    use gents_lean_vocab::families::goal_client_control_contracts_resume_readers as family;
    let cases = family::cases::<String>();
    let before = &cases.first().expect("modeled resume readers").input.before;
    let node = FixtureNode::start().await;
    let fixture = node
        .fixture_with_parent_state(&serde_json::to_value(&before.execution).unwrap(), true)
        .await;
    let access = fixture.user_access();
    let session = access
        .transact_readonly("test.goal_session", |txn| {
            Box::pin(async {
                gents_store::config_client::agent_session::load_agent_session_owner_in_txn(
                    txn,
                    fixture.identity.did(),
                    &fixture.session,
                )
                .await
            })
        })
        .await
        .unwrap()
        .unwrap();
    let revision = fixture.goal_commit(&fixture.goal.doc_id).await.cid;
    let now = Utc::now();
    let mut command = SessionGoalControl {
        version: SESSION_GOAL_CONTROL_VERSION,
        command_id: uuid::Uuid::new_v4().to_string(),
        requester_did: fixture.user.did().into(),
        requester_peer_id: None,
        node_did: fixture.identity.did().into(),
        agent_did: session.session.agent_did.clone(),
        agent_doc_id: fixture.agent_doc_id.clone(),
        session_id: fixture.session.clone(),
        session_doc_id: Some(session.doc_id),
        issued_at: now.to_rfc3339(),
        expires_at: (now + chrono::Duration::minutes(5)).to_rfc3339(),
        expected: Some(GoalRevision {
            document: fixture.goal.doc_id.clone(),
            composite: revision,
        }),
        operation: GoalOperation::Resume {},
        initial_request: None,
        native_evidence: None,
        signature: Vec::new(),
    };
    command.signature = fixture
        .user
        .sign(&command.signing_payload().unwrap())
        .await
        .unwrap();
    let doc_id = persist_command(&access, &command).await;
    let identity: Arc<dyn NodeIdentity> = fixture.identity.clone();
    process_command(
        &fixture.node,
        &identity,
        &doc_id,
        fixture.agent.as_ref(),
        None,
    )
    .await
    .unwrap();
    let GoalControlOutcome::Applied {
        request_id: Some(continuation),
        ..
    } = receipt(&access, &doc_id).await.unwrap().outcome
    else {
        panic!("idle client resume did not publish a continuation");
    };
    let outsider = Arc::new(
        tokio::task::spawn_blocking(|| {
            gents_store::test_support::test_identity("goal-resume-outsider")
        })
        .await
        .unwrap(),
    );
    let admin = gents_store::config_client::ConfigAccess::local_as(
        fixture.node.clone(),
        ::identity::Did::new(fixture.identity.did()).unwrap(),
    );
    admin
        .grant_node_permission(
            &::identity::Did::new(outsider.did()).unwrap(),
            gents_store::config_client::NodePermission::DocumentRead,
        )
        .await
        .unwrap();
    let endpoint = match &access {
        gents_store::config_client::ConfigAccess::Graphql(endpoint) => endpoint.url(),
        gents_store::config_client::ConfigAccess::Local(_) => {
            panic!("fixture requires served client access")
        }
    };
    let outsider_access = gents_store::config_client::ConfigAccess::Graphql(
        gents_store::config_client::GraphqlEndpoint::as_principal(endpoint, outsider.did())
            .with_client_signing_capability(outsider.client_signing_capability().unwrap())
            .unwrap(),
    );
    let query = format!(
        r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
        escape_graphql_string(&continuation)
    );
    for case in &cases {
        let reader = match case.input.reader.as_str() {
            "requester" => access.clone(),
            "agent" => fixture.agent_access(),
            "outsider" => outsider_access.clone(),
            other => panic!("unmapped modeled reader {other}"),
        };
        let rows = reader.execute(&query).await.unwrap();
        let readable = rows["data"]["AgentRequest"]
            .as_array()
            .is_some_and(|rows| rows.len() == 1);
        case.check(family::ResumeReaderOutput { readable });
    }
    node.shutdown().await;
}
