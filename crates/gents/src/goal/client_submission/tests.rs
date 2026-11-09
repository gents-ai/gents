use super::*;
use crate::runtime::agent::p2p_reconcile::session_goal_control::process_command;
use gents_protocol::goal_control::{
    GoalControlOutcome, GoalOperation, GoalRevision, SESSION_GOAL_CONTROL_VERSION,
};
use serde_json::json;
use std::sync::Arc;

#[tokio::test]
async fn cross_actor_stale_goal_rejection_is_durable_and_recoverable() {
    let modeled = crate::lean_vocab_test::lean_contract_snapshot()
        .goal_client_control_cases
        .iter()
        .find(|case| case["name"] == "active_authorized_idle_foreign_document_pause")
        .expect("modeled foreign Goal revision");
    assert_eq!(modeled["facts"]["authorized"], true);
    assert_eq!(modeled["facts"]["unfinished_request"], false);
    assert_eq!(modeled["expected_revision"]["document"], "foreign-goal");
    assert_eq!(
        modeled["expected_revision"]["composite"],
        "revision-current"
    );
    assert!(modeled["expected"].is_null());
    let fixture = crate::goal::operator_resume::support::Fixture::new(&json!({
        "status": "active", "sequence": 0, "blocked_audits": 0,
        "wrapup_requested": false, "wrapup_completed": false,
        "tokens_used": 0, "token_budget": 1000, "last_continued_from": null
    }))
    .await;
    let identity: Arc<dyn NodeIdentity> = fixture.identity.clone();
    let user = fixture.user.clone();
    let access = fixture.user_access();
    let session = access
        .transact_readonly("test.blocked_session", |txn| {
            Box::pin(async {
                crate::store::config_client::agent_session::load_agent_session_owner_in_txn(
                    txn,
                    identity.did(),
                    crate::goal::operator_resume::support::SESSION,
                )
                .await
            })
        })
        .await
        .unwrap()
        .unwrap();
    let now = Utc::now();
    let current_revision = fixture.goal_commit(&fixture.goal.doc_id).await.cid;
    let command = SessionGoalControl {
        version: SESSION_GOAL_CONTROL_VERSION,
        command_id: "blocked-control".into(),
        requester_did: user.did().into(),
        requester_peer_id: None,
        node_did: identity.did().into(),
        agent_did: fixture.parent.agent_did.clone(),
        agent_doc_id: crate::goal::client_control::agent_document_for_test(
            &access,
            identity.did(),
            &fixture.parent.agent_did,
        )
        .await,
        session_id: crate::goal::operator_resume::support::SESSION.into(),
        session_doc_id: Some(session.doc_id),
        issued_at: now.to_rfc3339(),
        expires_at: (now + chrono::Duration::minutes(5)).to_rfc3339(),
        expected: Some(GoalRevision {
            document: "absent-goal".into(),
            composite: current_revision,
        }),
        operation: GoalOperation::Pause {},
        initial_request: None,
        native_evidence: None,
        signature: Vec::new(),
    };
    let prepared = prepare_session_goal_control(&access, user.as_ref(), command)
        .await
        .unwrap();
    let error = submit_prepared_session_goal_control(&access, user.as_ref(), &prepared)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("blocked"));
    let before = access.execute("{ AgentSessionGoalControl { _docID intent_json } AgentRequest { _docID } Goal { _docID status } }").await.unwrap();
    let doc_id = before["data"]["AgentSessionGoalControl"][0]["_docID"]
        .as_str()
        .unwrap();
    assert_eq!(
        serde_json::from_str::<SessionGoalControl>(
            before["data"]["AgentSessionGoalControl"][0]["intent_json"]
                .as_str()
                .unwrap()
        )
        .unwrap(),
        prepared
    );
    process_command(&fixture.node, &identity, doc_id, fixture.agent.as_ref())
        .await
        .unwrap();
    assert_eq!(access.execute("{ AgentSessionGoalControl { _docID intent_json } AgentRequest { _docID } Goal { _docID status } }").await.unwrap(), before);
    let recovered = recover_session_goal_control(&access, user.as_ref(), &prepared)
        .await
        .unwrap()
        .expect("modeled rejection must be recoverable");
    recovered.validate_for(&prepared).unwrap();
    assert!(matches!(
        recovered.outcome,
        GoalControlOutcome::Rejected { .. }
    ));
    assert_eq!(
        submit_prepared_session_goal_control(&access, user.as_ref(), &prepared)
            .await
            .unwrap(),
        recovered
    );
    assert_eq!(access.execute("{ AgentSessionGoalControl { _docID intent_json } AgentRequest { _docID } Goal { _docID status } }").await.unwrap(), before);
    fixture.node.shutdown().await.expect("node shutdown");
}
