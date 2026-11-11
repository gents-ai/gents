use super::*;
use crate::goal::operator_resume::support::{Fixture, SESSION};
use crate::goal::session_goal_control::process_command;
use gents_protocol::goal_control::{
    GoalControlOutcome, GoalOperation, GoalRevision, SESSION_GOAL_CONTROL_VERSION,
};
use serde_json::{json, Value};
use std::sync::Arc;

fn modeled_case(name: &str) -> &'static Value {
    crate::lean_vocab_test::lean_contract_snapshot()
        .goal_client_control_cases
        .iter()
        .find(|case| case["name"] == name)
        .unwrap_or_else(|| panic!("modeled goal client control case {name}"))
}

async fn session_doc_id(fixture: &Fixture) -> String {
    let identity = fixture.identity.as_ref();
    fixture
        .user_access()
        .transact_readonly("test.goal_submission_session", |txn| {
            Box::pin(async move {
                crate::store::config_client::agent_session::load_agent_session_owner_in_txn(
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
    let mut changes =
        crate::store::subscription_source::subscribe_collections(&fixture.node, ["AgentSessionGoalControl"]).unwrap();
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

fn modeled_revision(fixture: &Fixture, case: &Value, current: &str) -> GoalRevision {
    GoalRevision {
        document: if case["expected_revision"]["document"] == case["before"]["revision"]["document"]
        {
            fixture.goal.doc_id.clone()
        } else {
            "foreign-goal-document".into()
        },
        composite: if case["expected_revision"]["composite"]
            == case["before"]["revision"]["composite"]
        {
            current.to_owned()
        } else {
            "stale-goal-commit".into()
        },
    }
}

#[tokio::test]
async fn authorized_user_pauses_then_resumes_a_running_goal_through_runtime_receipts() {
    let pause = modeled_case("active_authorized_busy_current_pause");
    let resume = modeled_case("paused_authorized_busy_current_resume");
    assert_eq!(pause["facts"]["unfinished_request"], true);
    assert_eq!(
        pause["expected"]["execution"],
        resume["before"]["execution"]
    );
    let fixture = Fixture::new_with_parent_state(&pause["before"]["execution"], false).await;
    let access = fixture.user_access();
    let user = fixture.user.clone();

    let revision = fixture.goal_commit(&fixture.goal.doc_id).await.cid;
    let command = signed_control(
        &fixture,
        &access,
        user.as_ref(),
        modeled_revision(&fixture, pause, &revision),
        GoalOperation::Pause {},
    )
    .await;
    let (_, receipt) =
        submit_while_runtime_answers(&fixture, &access, user.as_ref(), &command).await;
    assert!(
        matches!(receipt.outcome, GoalControlOutcome::Applied { .. }),
        "{:?}",
        receipt.outcome
    );
    assert_eq!(fixture.observe().await, pause["expected"]["execution"]);

    let revision = fixture.goal_commit(&fixture.goal.doc_id).await.cid;
    let command = signed_control(
        &fixture,
        &access,
        user.as_ref(),
        modeled_revision(&fixture, resume, &revision),
        GoalOperation::Resume {},
    )
    .await;
    let (_, receipt) =
        submit_while_runtime_answers(&fixture, &access, user.as_ref(), &command).await;
    assert!(
        matches!(receipt.outcome, GoalControlOutcome::Applied { .. }),
        "{:?}",
        receipt.outcome
    );
    assert_eq!(fixture.observe().await, resume["expected"]["execution"]);
    fixture.node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn user_without_session_access_receives_the_modeled_refusal() {
    let case = modeled_case("active_denied_busy_current_pause");
    assert_eq!(case["facts"]["authorized"], false);
    assert!(case["expected"].is_null());
    let fixture = Fixture::new_with_parent_state(&case["before"]["execution"], false).await;
    let owner_access = fixture.user_access();
    let requester = Arc::new(
        tokio::task::spawn_blocking(|| {
            crate::support::fixtures::test_identity("goal-submission-denied-requester")
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
        crate::store::config_client::NodePermission::CollectionGet,
        crate::store::config_client::NodePermission::DocumentRead,
        crate::store::config_client::NodePermission::DocumentUpdate,
        crate::store::config_client::NodePermission::DacRelationAdd,
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
        crate::store::config_client::GraphqlEndpoint::as_principal(endpoint, requester.did())
            .with_client_signing_capability(requester.client_signing_capability().unwrap())
            .unwrap()
            .with_client_signed_agent_creates(),
    );
    let revision = fixture.goal_commit(&fixture.goal.doc_id).await.cid;
    let before = fixture.observe().await;
    let command = signed_control(
        &fixture,
        &access,
        requester.as_ref(),
        modeled_revision(&fixture, case, &revision),
        GoalOperation::Pause {},
    )
    .await;
    let (_, receipt) =
        submit_while_runtime_answers(&fixture, &access, requester.as_ref(), &command).await;
    assert_eq!(
        receipt.outcome,
        GoalControlOutcome::Rejected {
            reason: "permission denied or session unavailable".into()
        }
    );
    assert_eq!(fixture.observe().await, before);
    assert_eq!(
        fixture.goal_commit(&fixture.goal.doc_id).await.cid,
        revision
    );
    fixture.node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn cross_actor_stale_goal_rejection_is_durable_and_recoverable() {
    let modeled = modeled_case("active_authorized_idle_foreign_document_pause");
    assert_eq!(modeled["facts"]["authorized"], true);
    assert_eq!(modeled["facts"]["unfinished_request"], false);
    assert_eq!(modeled["expected_revision"]["document"], "foreign-goal");
    assert_eq!(
        modeled["expected_revision"]["composite"],
        "revision-current"
    );
    assert!(modeled["expected"].is_null());
    let fixture = Fixture::new(&json!({
        "status": "active", "sequence": 0, "blocked_audits": 0,
        "wrapup_requested": false, "wrapup_completed": false,
        "tokens_used": 0, "token_budget": 1000, "last_continued_from": null
    }))
    .await;
    let identity: Arc<dyn NodeIdentity> = fixture.identity.clone();
    let user = fixture.user.clone();
    let access = fixture.user_access();
    let current_revision = fixture.goal_commit(&fixture.goal.doc_id).await.cid;
    let query = "{ AgentSessionGoalControl { _docID intent_json } AgentRequest { _docID } Goal { _docID status } }";
    let goal_before = access
        .execute("{ AgentRequest { _docID } Goal { _docID status } }")
        .await
        .unwrap();
    let prepared = signed_control(
        &fixture,
        &access,
        user.as_ref(),
        modeled_revision(&fixture, modeled, &current_revision),
        GoalOperation::Pause {},
    )
    .await;
    let (doc_id, receipt) =
        submit_while_runtime_answers(&fixture, &access, user.as_ref(), &prepared).await;
    assert!(matches!(
        receipt.outcome,
        GoalControlOutcome::Rejected { .. }
    ));
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
        .expect("modeled rejection must be recoverable");
    assert_eq!(recovered, receipt);
    assert_eq!(
        submit_prepared_session_goal_control(&access, user.as_ref(), &prepared)
            .await
            .unwrap(),
        recovered
    );
    assert_eq!(access.execute(query).await.unwrap(), before);
    fixture.node.shutdown().await.expect("node shutdown");
}
