use super::*;
use crate::goal::operator_resume::support::{execute, Fixture, SESSION};
use crate::goal::session_goal_control::process_command;
use gents_protocol::goal_control::{
    GoalControlOutcome, GoalOperation, GoalRevision, SESSION_GOAL_CONTROL_VERSION,
};
use serde_json::Value;
use std::sync::Arc;

async fn modeled_fixture(execution: &Value, objective: &str, unfinished_request: bool) -> Fixture {
    let fixture = Fixture::new_with_parent_state(execution, !unfinished_request).await;
    execute(
        &fixture.agent_access(),
        &format!(
            r#"mutation {{ update_Goal(docID: "{}", input: {{ objective: "{}" }}) {{ _docID }} }}"#,
            escape_graphql_string(&fixture.goal.doc_id),
            escape_graphql_string(objective)
        ),
    )
    .await;
    fixture
}

fn modeled_operation(kind: &str, objective: Option<String>, budget: Option<i64>) -> GoalOperation {
    match kind {
        "edit" => GoalOperation::Edit {
            objective,
            token_budget: budget,
        },
        "pause" => GoalOperation::Pause {},
        "resume" => GoalOperation::Resume {},
        "stop" => GoalOperation::Stop {},
        other => panic!("unknown modeled goal operation {other}"),
    }
}

async fn session_doc_id(fixture: &Fixture) -> String {
    let identity = fixture.identity.as_ref();
    fixture
        .user_access()
        .transact_readonly("test.goal_submission_session", |txn| {
            Box::pin(async move {
                gents_store::config_client::agent_session::load_agent_session_owner_in_txn(
                    txn,
                    identity.did(),
                    SESSION,
                )
                .await
            })
        })
        .await
        .unwrap()
        .unwrap()
        .doc_id
}

async fn signed_control(
    fixture: &Fixture,
    access: &ConfigAccess,
    requester: &dyn NodeIdentity,
    expected: GoalRevision,
    operation: GoalOperation,
) -> SessionGoalControl {
    let now = Utc::now();
    let command = SessionGoalControl {
        version: SESSION_GOAL_CONTROL_VERSION,
        command_id: uuid::Uuid::new_v4().to_string(),
        requester_did: requester.did().into(),
        requester_peer_id: None,
        node_did: fixture.identity.did().into(),
        agent_did: fixture.parent.agent_did.clone(),
        agent_doc_id: fixture.agent_doc_id.clone(),
        session_id: SESSION.into(),
        session_doc_id: Some(session_doc_id(fixture).await),
        issued_at: now.to_rfc3339(),
        expires_at: (now + chrono::Duration::minutes(5)).to_rfc3339(),
        expected: Some(expected),
        operation,
        initial_request: None,
        native_evidence: None,
        signature: Vec::new(),
    };
    prepare_session_goal_control(access, requester, command)
        .await
        .unwrap()
}

async fn published_control(changes: &mut events::DocumentChangeSubscription) -> String {
    loop {
        let batch = changes
            .recv()
            .await
            .expect("goal control subscription closed");
        if let Some(change) = batch
            .changes
            .into_iter()
            .find(|change| !change.doc_id.is_empty())
        {
            return change.doc_id;
        }
    }
}

/// Submits through the client owner while the runtime's command owner answers
/// only after the publication is observed and the submission is still waiting.
async fn submit_while_runtime_answers(
    fixture: &Fixture,
    access: &ConfigAccess,
    requester: &dyn NodeIdentity,
    command: &SessionGoalControl,
) -> (String, SessionGoalControlReceipt) {
    let mut changes = gents_store::subscription_source::subscribe_collections(
        &fixture.node,
        ["AgentSessionGoalControl"],
    )
    .unwrap();
    let submission = submit_prepared_session_goal_control(access, requester, command);
    tokio::pin!(submission);
    let doc_id = tokio::select! {
        settled = &mut submission => {
            panic!("goal control settled before the runtime observed it: {settled:?}")
        }
        doc_id = published_control(&mut changes) => doc_id,
    };
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(500), &mut submission)
            .await
            .is_err(),
        "a saved control the runtime has not answered stays pending rather than failing"
    );
    let identity: Arc<dyn NodeIdentity> = fixture.identity.clone();
    let (receipt, processed) = tokio::join!(
        submission,
        process_command(&fixture.node, &identity, &doc_id, fixture.agent.as_ref())
    );
    assert_eq!(processed.unwrap(), None);
    let receipt = receipt.unwrap();
    receipt.validate_for(command).unwrap();
    (doc_id, receipt)
}

fn modeled_revision(
    fixture: &Fixture,
    same_document: bool,
    same_commit: bool,
    current: &str,
) -> GoalRevision {
    GoalRevision {
        document: if same_document {
            fixture.goal.doc_id.clone()
        } else {
            "foreign-goal-document".into()
        },
        composite: if same_commit {
            current.to_owned()
        } else {
            "stale-goal-commit".into()
        },
    }
}

async fn assert_unchanged(fixture: &Fixture, before: &Value, objective: &str, revision: &str) {
    assert_eq!(&fixture.observe().await, before);
    assert_eq!(fixture.current_goal().await.objective, objective);
    assert_eq!(
        fixture.goal_commit(&fixture.goal.doc_id).await.cid,
        revision
    );
}

#[tokio::test]
async fn authorized_user_pauses_then_resumes_a_running_goal_through_runtime_receipts() {
    for case in gents_lean_vocab::families::goal_client_control_contracts_submission_trace::cases::<
        String,
    >() {
        let input = &case.input;
        let fixture = modeled_fixture(
            &serde_json::to_value(&input.before.execution).unwrap(),
            &input.before.objective,
            input.steps.iter().any(|step| step.facts.unfinished_request),
        )
        .await;
        let access = fixture.user_access();
        let user = fixture.user.clone();
        let mut observed = Vec::new();
        for step in &input.steps {
            let revision = fixture.goal_commit(&fixture.goal.doc_id).await.cid;
            let before = fixture.observe().await;
            let objective = fixture.current_goal().await.objective;
            let command = signed_control(
                &fixture,
                &access,
                user.as_ref(),
                modeled_revision(&fixture, true, true, &revision),
                modeled_operation(
                    &step.operation.kind,
                    step.operation.objective.clone(),
                    step.operation.budget,
                ),
            )
            .await;
            let (_, receipt) =
                submit_while_runtime_answers(&fixture, &access, user.as_ref(), &command).await;
            observed.push(match receipt.outcome {
                GoalControlOutcome::Applied { .. } => {
                    Some(serde_json::from_value(fixture.control_snapshot(&revision).await).unwrap())
                }
                GoalControlOutcome::Rejected { .. } => {
                    assert_unchanged(&fixture, &before, &objective, &revision).await;
                    None
                }
            });
        }
        case.check(observed);
        fixture.node.shutdown().await.expect("node shutdown");
    }
}

#[tokio::test]
async fn user_without_session_access_receives_the_modeled_refusal() {
    for case in gents_lean_vocab::families::goal_client_control_contracts_denied_submission::cases::<
        String,
    >() {
        let input = &case.input;
        let fixture = modeled_fixture(
            &serde_json::to_value(&input.before.execution).unwrap(),
            &input.before.objective,
            input.facts.unfinished_request,
        )
        .await;
        let owner_access = fixture.user_access();
        let requester = Arc::new(
            tokio::task::spawn_blocking(|| {
                gents_store::test_support::test_identity("goal-submission-denied-requester")
            })
            .await
            .unwrap(),
        );
        let requester_did = ::identity::Did::new(requester.did()).unwrap();
        let admin = ConfigAccess::local_as(
            fixture.node.clone(),
            ::identity::Did::new(fixture.identity.did()).unwrap(),
        );
        for permission in [
            gents_store::config_client::NodePermission::CollectionGet,
            gents_store::config_client::NodePermission::DocumentRead,
            gents_store::config_client::NodePermission::DocumentUpdate,
            gents_store::config_client::NodePermission::DacRelationAdd,
        ] {
            admin
                .grant_node_permission(&requester_did, permission)
                .await
                .unwrap();
        }
        let session = session_doc_id(&fixture).await;
        owner_access
            .add_actor_relationship("AgentSession", &session, "reader", &requester_did)
            .await
            .unwrap();
        owner_access
            .add_actor_relationship(
                "Agent",
                &fixture.agent_doc_id,
                "session_writer",
                &requester_did,
            )
            .await
            .unwrap();
        let endpoint = match &owner_access {
            ConfigAccess::Graphql(endpoint) => endpoint.url(),
            ConfigAccess::Local(_) => panic!("fixture requires held client access"),
        };
        let access = ConfigAccess::Graphql(
            gents_store::config_client::GraphqlEndpoint::as_principal(endpoint, requester.did())
                .with_client_signing_capability(requester.client_signing_capability().unwrap())
                .unwrap()
                .with_client_signed_agent_creates(),
        );
        let revision = fixture.goal_commit(&fixture.goal.doc_id).await.cid;
        let before = fixture.observe().await;
        let objective = fixture.current_goal().await.objective;
        let command = signed_control(
            &fixture,
            &access,
            requester.as_ref(),
            modeled_revision(
                &fixture,
                input.expected_revision.document == input.before.revision.document,
                input.expected_revision.composite == input.before.revision.composite,
                &revision,
            ),
            modeled_operation(
                &input.operation.kind,
                input.operation.objective.clone(),
                input.operation.budget,
            ),
        )
        .await;
        let (_, receipt) =
            submit_while_runtime_answers(&fixture, &access, requester.as_ref(), &command).await;
        let observed = match receipt.outcome {
            GoalControlOutcome::Applied { .. } => {
                Some(serde_json::from_value(fixture.control_snapshot(&revision).await).unwrap())
            }
            GoalControlOutcome::Rejected { reason } => {
                assert_eq!(reason, "permission denied or session unavailable");
                assert_unchanged(&fixture, &before, &objective, &revision).await;
                None
            }
        };
        case.check(observed);
        fixture.node.shutdown().await.expect("node shutdown");
    }
}

#[tokio::test]
async fn cross_actor_stale_goal_rejection_is_durable_and_recoverable() {
    for case in gents_lean_vocab::families::goal_client_control_contracts_foreign_submission::cases::<
        String,
    >() {
        let input = &case.input;
        let fixture = modeled_fixture(
            &serde_json::to_value(&input.before.execution).unwrap(),
            &input.before.objective,
            input.facts.unfinished_request,
        )
        .await;
        let identity: Arc<dyn NodeIdentity> = fixture.identity.clone();
        let user = fixture.user.clone();
        let access = fixture.user_access();
        let current_revision = fixture.goal_commit(&fixture.goal.doc_id).await.cid;
        let execution_before = fixture.observe().await;
        let objective = fixture.current_goal().await.objective;
        let query = "{ AgentSessionGoalControl { _docID intent_json } AgentRequest { _docID } Goal { _docID status } }";
        let goal_before = access
            .execute("{ AgentRequest { _docID } Goal { _docID status } }")
            .await
            .unwrap();
        let prepared = signed_control(
            &fixture,
            &access,
            user.as_ref(),
            modeled_revision(
                &fixture,
                input.expected_revision.document == input.before.revision.document,
                input.expected_revision.composite == input.before.revision.composite,
                &current_revision,
            ),
            modeled_operation(
                &input.operation.kind,
                input.operation.objective.clone(),
                input.operation.budget,
            ),
        )
        .await;
        let (doc_id, receipt) =
            submit_while_runtime_answers(&fixture, &access, user.as_ref(), &prepared).await;
        let observed = match &receipt.outcome {
            GoalControlOutcome::Applied { .. } => Some(
                serde_json::from_value(fixture.control_snapshot(&current_revision).await).unwrap(),
            ),
            GoalControlOutcome::Rejected { .. } => {
                assert_unchanged(&fixture, &execution_before, &objective, &current_revision).await;
                let before = access.execute(query).await.unwrap();
                assert_eq!(
                    serde_json::from_str::<SessionGoalControl>(
                        before["data"]["AgentSessionGoalControl"][0]["intent_json"]
                            .as_str()
                            .unwrap()
                    )
                    .unwrap(),
                    prepared
                );
                assert_eq!(
                    before["data"]["AgentRequest"],
                    goal_before["data"]["AgentRequest"]
                );
                assert_eq!(before["data"]["Goal"], goal_before["data"]["Goal"]);
                process_command(&fixture.node, &identity, &doc_id, fixture.agent.as_ref())
                    .await
                    .unwrap();
                assert_eq!(access.execute(query).await.unwrap(), before);
                let recovered = recover_session_goal_control(&access, user.as_ref(), &prepared)
                    .await
                    .unwrap()
                    .expect("a refused submission is recoverable");
                assert_eq!(recovered, receipt);
                assert_eq!(
                    submit_prepared_session_goal_control(&access, user.as_ref(), &prepared)
                        .await
                        .unwrap(),
                    recovered
                );
                assert_eq!(access.execute(query).await.unwrap(), before);
                None
            }
        };
        case.check(observed);
        fixture.node.shutdown().await.expect("node shutdown");
    }
}
