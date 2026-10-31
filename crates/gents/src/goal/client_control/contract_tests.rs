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
    let peer = command
        .requester_peer_id
        .as_deref()
        .map(|peer| format!(r#""{}""#, escape_graphql_string(peer)))
        .unwrap_or_else(|| "null".into());
    let response = access.write("test.goal_control", &format!(r#"mutation {{ create_AgentSessionGoalControl(input: {{
        command_id: "{}", requester_did: "{}", requester_peer_id: {peer}, node_did: "{}", session_id: "{}", intent_json: "{}"
    }}) {{ _docID }} }}"#, escape_graphql_string(&command.command_id), escape_graphql_string(&command.requester_did),
        escape_graphql_string(&command.node_did), escape_graphql_string(&command.session_id),
        escape_graphql_string(&serde_json::to_string(command).unwrap()))).await.unwrap();
    crate::store::graphql::created_doc_id(&response, "AgentSessionGoalControl").unwrap()
}

pub(super) async fn receipt(
    access: &crate::ConfigAccess,
    doc_id: &str,
) -> Option<SessionGoalControlReceipt> {
    let response = access.execute(&format!(r#"{{ AgentSessionGoalControl(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ receipt_json }} }}"#,
        escape_graphql_string(doc_id))).await.unwrap();
    response["data"]["AgentSessionGoalControl"][0]["receipt_json"]
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
        execute(&fixture.node, &format!(r#"mutation {{ update_Goal(docID: "{}", input: {{ objective: "{}" }}) {{ _docID }} }}"#,
            escape_graphql_string(&fixture.goal.doc_id), escape_graphql_string(case.before["objective"].as_str().unwrap()))).await;
        crate::session::ensure_session_with_agent_id_and_requester_did(
            &fixture.node,
            SESSION,
            fixture.identity.did(),
            "contract-behavior",
            Some(fixture.identity.did()),
        )
        .await
        .unwrap();
        let access = crate::ConfigAccess::local(fixture.node.clone());
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
        let revision = crate::store::graphql::newest_document_composite_commit(
            &fixture.node,
            &fixture.goal.doc_id,
            "test.goal_revision",
        )
        .await
        .unwrap()
        .unwrap()
        .cid;
        let before = fixture.observe().await;
        let mut operation = case.operation.clone();
        if let Some(budget) = operation.as_object_mut().unwrap().remove("budget") {
            operation["token_budget"] = budget;
        }
        let now = Utc::now();
        let mut command = SessionGoalControl {
            version: SESSION_GOAL_CONTROL_VERSION,
            command_id: uuid::Uuid::new_v4().to_string(),
            requester_did: fixture.identity.did().into(),
            requester_peer_id: None,
            node_did: fixture.identity.did().into(),
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
            signature: Vec::new(),
        };
        command.signature = fixture
            .identity
            .sign(&command.signing_payload().unwrap())
            .await
            .unwrap();
        if case.facts["authorized"] == false {
            command.signature[0] ^= 1;
        }
        let doc_id = persist_command(&access, &command).await;
        let identity: Arc<dyn NodeIdentity> = fixture.identity.clone();
        assert_eq!(
            process_command(&fixture.node, &identity, &doc_id)
                .await
                .unwrap(),
            None,
            "{}",
            case.name
        );
        let result = access.execute(&format!(r#"{{ AgentSessionGoalControl(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ receipt_json }} }}"#,
            escape_graphql_string(&doc_id))).await.unwrap();
        let receipt: SessionGoalControlReceipt = serde_json::from_str(
            result["data"]["AgentSessionGoalControl"][0]["receipt_json"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        receipt.validate_for(&command).unwrap();
        assert!(identity
            .verify(
                &receipt.node_did,
                &receipt.signing_payload().unwrap(),
                &receipt.signature
            )
            .await
            .unwrap());
        let current = load_canonical_goal(&fixture.node, identity.did(), SESSION)
            .await
            .unwrap()
            .unwrap();
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
                    crate::store::graphql::newest_document_composite_commit(
                        &fixture.node,
                        &fixture.goal.doc_id,
                        "test.unchanged_goal_revision"
                    )
                    .await
                    .unwrap()
                    .unwrap()
                    .cid,
                    revision,
                    "{} no goal write",
                    case.name
                );
            }
        }
        let first_receipt = serde_json::to_value(receipt).unwrap();
        process_command(&fixture.node, &identity, &doc_id)
            .await
            .unwrap();
        let replay = access.execute(&format!(r#"{{ AgentSessionGoalControl(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ receipt_json }} }}"#,
            escape_graphql_string(&doc_id))).await.unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(
                replay["data"]["AgentSessionGoalControl"][0]["receipt_json"]
                    .as_str()
                    .unwrap()
            )
            .unwrap(),
            first_receipt,
            "{} receipt replay",
            case.name
        );
        fixture.node.shutdown().await;
    }
}
