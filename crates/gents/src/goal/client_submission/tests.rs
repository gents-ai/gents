use super::*;
use crate::runtime::agent::p2p_reconcile::session_goal_control::process_command;
use crate::store::identity::KeyIdentity;
use defra_node::EmbeddedNode;
use gents_protocol::goal_control::{
    GoalControlOutcome, GoalOperation, GoalRevision, SESSION_GOAL_CONTROL_VERSION,
};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

#[tokio::test]
async fn submission_returns_verified_rejection_after_ignoring_invalid_receipts() {
    let temp = tempfile::tempdir().unwrap();
    let identity: Arc<dyn NodeIdentity> =
        Arc::new(KeyIdentity::load_or_create(temp.path().join("runtime.key"), None).unwrap());
    let node = Arc::new(
        EmbeddedNode::builder()
            .with_node_identity_did(identity.did())
            .build()
            .await
            .unwrap(),
    );
    crate::store::schema::ensure_runtime_schemas(&node)
        .await
        .unwrap();
    let access = ConfigAccess::local(node.clone());
    let now = Utc::now();
    let command = SessionGoalControl {
        version: SESSION_GOAL_CONTROL_VERSION,
        command_id: "rejected-control".into(),
        requester_did: identity.did().into(),
        requester_peer_id: None,
        node_did: identity.did().into(),
        session_id: "absent-session".into(),
        session_doc_id: Some("absent-session-document".into()),
        issued_at: now.to_rfc3339(),
        expires_at: (now + chrono::Duration::minutes(5)).to_rfc3339(),
        expected: Some(GoalRevision {
            document: "absent-goal".into(),
            composite: "absent-revision".into(),
        }),
        operation: GoalOperation::Pause {},
        initial_request: None,
        signature: Vec::new(),
    };
    let prepared = prepare_session_goal_control(identity.as_ref(), command.clone())
        .await
        .unwrap();
    let before = access
        .execute("{ AgentSessionGoalControl { _docID } }")
        .await
        .unwrap();
    assert_eq!(before["data"]["AgentSessionGoalControl"], json!([]));
    assert!(
        recover_session_goal_control(&access, identity.as_ref(), &prepared)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        access
            .execute("{ AgentSessionGoalControl { _docID } }")
            .await
            .unwrap(),
        before,
        "preparation and observation must not publish an intent"
    );
    let submission = submit_session_goal_control(&access, identity.as_ref(), command);
    tokio::pin!(submission);
    let (doc_id, command): (String, SessionGoalControl) =
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let response = tokio::select! {
                    result = &mut submission => panic!("submission returned without a receipt: {result:?}"),
                    response = access.execute("{ AgentSessionGoalControl { _docID intent_json } }") => response.unwrap(),
                };
                if let Some(row) = response["data"]["AgentSessionGoalControl"]
                    .as_array()
                    .unwrap()
                    .first()
                {
                    break (
                        row["_docID"].as_str().unwrap().to_owned(),
                        serde_json::from_str(row["intent_json"].as_str().unwrap()).unwrap(),
                    );
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    assert_eq!(
        command, prepared,
        "publication must retain its prepared bytes"
    );
    let mut invalid = SessionGoalControlReceipt {
        version: SESSION_GOAL_CONTROL_VERSION,
        command_id: command.command_id.clone(),
        command_digest: command.computed_digest().unwrap(),
        requester_did: command.requester_did.clone(),
        node_did: command.node_did.clone(),
        session_id: command.session_id.clone(),
        outcome: GoalControlOutcome::Rejected {
            reason: "untrusted refusal".into(),
        },
        processed_at: now.to_rfc3339(),
        signature: Vec::new(),
    };
    for foreign_scope in [false, true] {
        if foreign_scope {
            invalid.session_id = "another-session".into();
        }
        invalid.signature = identity
            .sign(&invalid.signing_payload().unwrap())
            .await
            .unwrap();
        if !foreign_scope {
            invalid.signature[0] ^= 1;
        }
        access
            .write(
                "test.goal_submission_untrusted_receipt",
                &format!(
                    r#"mutation {{ update_AgentSessionGoalControl(docID: "{}", input: {{ receipt_json: "{}" }}) {{ _docID }} }}"#,
                    escape_graphql_string(&doc_id),
                    escape_graphql_string(&serde_json::to_string(&invalid).unwrap())
                ),
            )
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(600), &mut submission)
                .await
                .is_err(),
            "untrusted receipt ended the wait (foreign scope: {foreign_scope})"
        );
        assert!(
            recover_session_goal_control(&access, identity.as_ref(), &prepared)
                .await
                .unwrap()
                .is_none()
        );
    }
    process_command(&node, &identity, &doc_id).await.unwrap();
    let response = access
        .execute(&format!(
            r#"{{ AgentSessionGoalControl(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ receipt_json }} }}"#,
            escape_graphql_string(&doc_id)
        ))
        .await
        .unwrap();
    let expected: SessionGoalControlReceipt = serde_json::from_str(
        response["data"]["AgentSessionGoalControl"][0]["receipt_json"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    expected.validate_for(&command).unwrap();
    assert!(identity
        .verify(
            identity.did(),
            &expected.signing_payload().unwrap(),
            &expected.signature
        )
        .await
        .unwrap());
    assert!(matches!(
        expected.outcome,
        GoalControlOutcome::Rejected { .. }
    ));
    let received = tokio::time::timeout(Duration::from_secs(5), &mut submission)
        .await
        .unwrap()
        .expect("verified runtime rejection remains a typed receipt");
    assert_eq!(received, expected);
    let query = "{ AgentSessionGoalControl { _docID intent_json receipt_json } }";
    let before = access.execute(query).await.unwrap();
    assert_eq!(
        recover_session_goal_control(&access, identity.as_ref(), &prepared)
            .await
            .unwrap(),
        Some(expected.clone())
    );
    assert_eq!(
        submit_prepared_session_goal_control(&access, identity.as_ref(), &prepared)
            .await
            .unwrap(),
        expected
    );
    let mut changed = prepared.clone();
    changed.operation = GoalOperation::Stop {};
    let changed = prepare_session_goal_control(identity.as_ref(), changed)
        .await
        .unwrap();
    assert!(
        recover_session_goal_control(&access, identity.as_ref(), &changed)
            .await
            .is_err()
    );
    assert!(
        submit_prepared_session_goal_control(&access, identity.as_ref(), &changed)
            .await
            .is_err()
    );
    let mut corrupted = prepared;
    corrupted.signature[0] ^= 1;
    assert!(
        recover_session_goal_control(&access, identity.as_ref(), &corrupted)
            .await
            .is_err()
    );
    assert!(
        submit_prepared_session_goal_control(&access, identity.as_ref(), &corrupted)
            .await
            .is_err()
    );
    assert_eq!(access.execute(query).await.unwrap(), before);
    let response = access
        .execute("{ AgentRequest { _docID } AgentSessionGoalControl { _docID } }")
        .await
        .unwrap();
    assert_eq!(response["data"]["AgentRequest"], json!([]));
    assert_eq!(
        response["data"]["AgentSessionGoalControl"],
        json!([{ "_docID": doc_id }])
    );
}
