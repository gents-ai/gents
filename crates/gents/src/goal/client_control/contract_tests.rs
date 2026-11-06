use super::*;
use crate::goal::operator_resume::support::{execute, Fixture, SESSION};
use crate::runtime::agent::p2p_reconcile::session_goal_control::process_command;
use gents_protocol::goal_control::{
    GoalRevision, SessionGoalControlReceipt, SESSION_GOAL_CONTROL_VERSION,
};
use serde_json::Value;
use std::sync::Arc;

#[derive(Deserialize)]
struct Contracts {
    goal_client_control_cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    name: String,
    before: Value,
    expected_revision: GoalRevision,
    operation: Value,
    facts: Value,
    expected: Option<Value>,
}

pub(super) async fn persist_command(
    access: &crate::ConfigAccess,
    command: &SessionGoalControl,
) -> String {
    try_persist_command(access, command).await.unwrap()
}

pub(super) async fn try_persist_command(
    access: &crate::ConfigAccess,
    command: &SessionGoalControl,
) -> anyhow::Result<String> {
    let document = crate::goal::client_submission::publish(access, command).await?;
    crate::goal::client_submission::grant_command_relationships(access, command, &document).await?;
    Ok(document)
}

pub(super) async fn receipt(
    access: &crate::ConfigAccess,
    doc_id: &str,
) -> Option<SessionGoalControlReceipt> {
    let response = access.execute(&format!(r#"{{ AgentSessionGoalReceipt(filter: {{ command_doc_id: {{ _eq: "{}" }} }}) {{ receipt_json }} }}"#,
        escape_graphql_string(doc_id))).await.unwrap();
    response["data"]["AgentSessionGoalReceipt"][0]["receipt_json"]
        .as_str()
        .map(|raw| serde_json::from_str(raw).unwrap())
}

#[tokio::test]
async fn generated_goal_client_controls_drive_signed_commands() {
    let contracts: Contracts = gents_lean_contract::load_contract_snapshot().unwrap();
    assert_eq!(contracts.goal_client_control_cases.len(), 756);
    for case in contracts.goal_client_control_cases {
        let fixture = Fixture::new_with_parent_state(
            &case.before["execution"],
            case.facts["unfinished_request"] == false,
        )
        .await;
        execute(&fixture.agent_access(), &format!(r#"mutation {{ update_Goal(docID: "{}", input: {{ objective: "{}" }}) {{ _docID }} }}"#,
            escape_graphql_string(&fixture.goal.doc_id), escape_graphql_string(case.before["objective"].as_str().unwrap()))).await;
        let access = fixture.user_access();
        let session = access
            .transact_readonly("test.goal_session", |txn| {
                Box::pin(async {
                    crate::session::load_agent_session_owner_in_txn(
                        txn,
                        fixture.identity.did(),
                        SESSION,
                    )
                    .await
                })
            })
            .await
            .unwrap()
            .unwrap();
        let revision = fixture.goal_commit(&fixture.goal.doc_id).await.cid;
        let before = fixture.observe().await;
        let mut operation = case.operation.clone();
        if let Some(budget) = operation.as_object_mut().unwrap().remove("budget") {
            operation["token_budget"] = budget;
        }
        let now = Utc::now();
        let mut command = SessionGoalControl {
            version: SESSION_GOAL_CONTROL_VERSION,
            command_id: uuid::Uuid::new_v4().to_string(),
            requester_did: fixture.user.did().into(),
            requester_peer_id: None,
            node_did: fixture.identity.did().into(),
            agent_did: session.session.agent_did.clone(),
            agent_doc_id: super::agent_document_for_test(
                &access,
                fixture.identity.did(),
                &session.session.agent_did,
            )
            .await,
            session_id: SESSION.into(),
            session_doc_id: Some(session.doc_id),
            issued_at: now.to_rfc3339(),
            expires_at: (now + chrono::Duration::minutes(5)).to_rfc3339(),
            expected: Some(GoalRevision {
                document: if case.expected_revision.document == case.before["revision"]["document"]
                {
                    fixture.goal.doc_id.clone()
                } else {
                    "foreign-goal-document".into()
                },
                composite: if case.expected_revision.composite
                    == case.before["revision"]["composite"]
                {
                    revision.clone()
                } else {
                    "stale-goal-commit".into()
                },
            }),
            operation: serde_json::from_value(operation).unwrap(),
            initial_request: None,
            native_evidence: None,
            signature: Vec::new(),
        };
        command.signature = fixture
            .user
            .sign(&command.signing_payload().unwrap())
            .await
            .unwrap();
        if case.facts["authorized"] == false {
            command.signature[0] ^= 1;
        }
        let doc_id = persist_command(&access, &command).await;
        let identity: Arc<dyn NodeIdentity> = fixture.identity.clone();
        assert_eq!(
            process_command(&fixture.node, &identity, &doc_id, fixture.agent.as_ref())
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
        let current = fixture.current_goal().await;
        match case.expected {
            Some(expected) => {
                assert!(
                    matches!(receipt.outcome, GoalControlOutcome::Applied { .. }),
                    "{}: {:?}",
                    case.name,
                    receipt.outcome
                );
                assert_eq!(
                    fixture.observe().await,
                    expected["execution"],
                    "{} execution",
                    case.name
                );
                assert_eq!(
                    current.objective,
                    expected["objective"].as_str().unwrap(),
                    "{} objective",
                    case.name
                );
            }
            None => {
                assert!(
                    matches!(receipt.outcome, GoalControlOutcome::Rejected { .. }),
                    "{}",
                    case.name
                );
                assert_eq!(
                    fixture.observe().await,
                    before,
                    "{} unchanged execution",
                    case.name
                );
                assert_eq!(
                    current.objective,
                    case.before["objective"].as_str().unwrap(),
                    "{} unchanged objective",
                    case.name
                );
                assert_eq!(
                    fixture.goal_commit(&fixture.goal.doc_id).await.cid,
                    revision,
                    "{} no goal write",
                    case.name
                );
            }
        }
        let first_receipt = serde_json::to_value(receipt).unwrap();
        process_command(&fixture.node, &identity, &doc_id, fixture.agent.as_ref())
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(self::receipt(&access, &doc_id).await.unwrap()).unwrap(),
            first_receipt,
            "{} receipt replay",
            case.name
        );
        fixture.node.shutdown().await.expect("node shutdown");
    }
}

#[tokio::test]
async fn goal_command_signature_binds_modeled_physical_targets() {
    let directory = tempfile::tempdir().unwrap();
    let user = crate::store::identity::KeyIdentity::load_or_create(
        directory.path().join("user.key"),
        None,
    )
    .unwrap();
    let cases = &crate::lean_vocab_test::lean_contract_snapshot().cross_actor_authorization_cases;
    let baseline = cases
        .iter()
        .find(|case| case.name == "committed_claim")
        .unwrap();
    let mut command = SessionGoalControl {
        version: SESSION_GOAL_CONTROL_VERSION,
        command_id: baseline.intent.document.clone(),
        requester_did: user.did().into(),
        requester_peer_id: None,
        node_did: "node".into(),
        agent_did: baseline.intent.selected_agent.clone(),
        agent_doc_id: baseline.intent.agent_document.clone(),
        session_id: "session".into(),
        session_doc_id: Some(baseline.intent.session.clone()),
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
    for name in ["wrong_session", "wrong_agent_document"] {
        let case = cases.iter().find(|case| case.name == name).unwrap();
        let mut changed = command.clone();
        changed.session_doc_id = Some(case.observation.session.clone());
        changed.agent_doc_id = case.observation.agent_document.clone();
        let verified = user
            .verify(
                user.did(),
                &changed.signing_payload().unwrap(),
                &command.signature,
            )
            .await
            .unwrap_or(false);
        assert_eq!(
            verified,
            case.decision == crate::lean_vocab_test::LeanCrossActorDecision::Claimed,
            "{name}"
        );
    }
    assert!(user
        .verify(
            user.did(),
            &command.signing_payload().unwrap(),
            &command.signature
        )
        .await
        .unwrap());
}
