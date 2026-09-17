use super::*;
use crate::config_client::ConfigAccess;
use crate::identity::{AgentIdentity, KeyIdentity};
use crate::watcher::AgentRequest;

async fn signed_reply(
    node: &EmbeddedNode,
    identity: &KeyIdentity,
    item: &MailboxItem,
    id: &str,
) -> AgentRequest {
    persisted_reply(node, identity, item, id, false).await
}

async fn persisted_reply(
    node: &EmbeddedNode,
    identity: &KeyIdentity,
    item: &MailboxItem,
    id: &str,
    tamper_signature: bool,
) -> AgentRequest {
    use gents_protocol::request_admission::{AgentRequestAdmissionRecord, AgentRequestCreate};
    let mut create = AgentRequestCreate::base(
        id,
        identity.did(),
        identity.did(),
        "operator",
        "session-1",
        "Approved",
        "interactive",
        &Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        AgentRequestAdmissionRecord::local_self(identity.did()),
    );
    create.caused_by_source_doc_id = Some(item.doc_id.clone());
    crate::sign_agent_request_create(identity, &mut create)
        .await
        .unwrap();
    if tamper_signature {
        create.content = "Tampered after signing".into();
    }
    let response = node.execute(&create.graphql_mutation().unwrap()).await;
    let doc = single_mutation_document(&response, "create_AgentRequest")
        .unwrap()
        .unwrap();
    let doc_id = doc["_docID"].as_str().unwrap();
    let query = format!(
        r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
        escape_graphql_string(doc_id),
        crate::request_admission::SIGNED_REQUEST_FIELDS
    );
    let response = node.execute(&query).await;
    let record = rows::<gents_protocol::row::AgentRequestRow>(&response, "AgentRequest")
        .unwrap()
        .remove(0);
    AgentRequest::try_from(record).unwrap()
}

async fn claim(node: &EmbeddedNode, request: &AgentRequest, abort: bool) -> Result<()> {
    ConfigAccess::transact_local(node, None, "test.mailbox_reply", |txn| Box::pin(async move {
        let mutation = format!(r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ lifecycle_state: "claimed" }}) {{ _docID }} }}"#,
            escape_graphql_string(&request.doc_id));
        txn.execute_local_response(&mutation).await?;
        claim_reply_in_txn(&txn, request, &Utc::now().to_rfc3339()).await?;
        anyhow::ensure!(!abort, "injected transaction failure");
        Ok(())
    })).await
}

async fn stored_lifecycle(node: &EmbeddedNode, request: &AgentRequest) -> String {
    let query = format!(
        r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ lifecycle_state }} }}"#,
        escape_graphql_string(&request.doc_id)
    );
    rows::<Value>(&node.execute(&query).await, "AgentRequest").unwrap()[0]["lifecycle_state"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn signed_reply_claim_rolls_back_and_rejects_replay_and_dismissal() {
    let node = tests::test_node().await;
    let temp = tempfile::tempdir().unwrap();
    let identity = KeyIdentity::load_or_create(temp.path().join("reply.key"), None).unwrap();
    let mut context = tests::context(identity.did());
    context.agent_did = identity.did().into();
    let item = stamp_create(
        &node,
        &context,
        tests::args(MailboxAction::StartRequest, "approval"),
    )
    .await
    .unwrap();
    let request = signed_reply(&node, &identity, &item, "approved-request").await;
    let forged = persisted_reply(&node, &identity, &item, "forged-request", true).await;
    assert!(crate::lifecycle::is_claim_admission_error(
        &claim(&node, &forged, false).await.unwrap_err()
    ));
    assert_eq!(stored_lifecycle(&node, &forged).await, "pending");
    assert!(claim(&node, &request, true).await.is_err());
    assert_eq!(stored_lifecycle(&node, &request).await, "pending");
    assert_eq!(
        load_mailbox_item(&node, &item.doc_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        "open"
    );
    // The sweep cannot consume a valid but still-unclaimed reply either.
    assert_eq!(sweep_open_mailbox_items(&node).await.unwrap().acted, 0);
    claim(&node, &request, false).await.unwrap();
    claim(&node, &request, false).await.unwrap();
    let replay = signed_reply(&node, &identity, &item, "replayed-request").await;
    let error = claim(&node, &replay, false).await.unwrap_err();
    assert!(crate::lifecycle::is_claim_admission_error(&error));
    assert_eq!(stored_lifecycle(&node, &replay).await, "pending");
    let stored = load_mailbox_item(&node, &item.doc_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        stored.resolved_doc_id.as_deref(),
        Some(request.doc_id.as_str())
    );
    let declined = stamp_create(
        &node,
        &context,
        tests::args(MailboxAction::StartRequest, "declined"),
    )
    .await
    .unwrap();
    let denied = signed_reply(&node, &identity, &declined, "declined-request").await;
    dismiss_mailbox_item(&node, &declined.doc_id, identity.did())
        .await
        .unwrap();
    assert!(crate::lifecycle::is_claim_admission_error(
        &claim(&node, &denied, false).await.unwrap_err()
    ));
    assert_eq!(
        load_mailbox_item(&node, &declined.doc_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        "dismissed"
    );
    let raced = stamp_create(
        &node,
        &context,
        tests::args(MailboxAction::StartRequest, "competing-replies"),
    )
    .await
    .unwrap();
    let first = signed_reply(&node, &identity, &raced, "first-reply").await;
    let second = signed_reply(&node, &identity, &raced, "second-reply").await;
    let (first_result, second_result) =
        tokio::join!(claim(&node, &first, false), claim(&node, &second, false));
    assert_ne!(first_result.is_ok(), second_result.is_ok());
    let (winner, loser) = if first_result.is_ok() {
        (&first, &second)
    } else {
        (&second, &first)
    };
    assert_eq!(stored_lifecycle(&node, winner).await, "claimed");
    assert_eq!(stored_lifecycle(&node, loser).await, "pending");
    assert_eq!(
        load_mailbox_item(&node, &raced.doc_id)
            .await
            .unwrap()
            .unwrap()
            .resolved_doc_id
            .as_deref(),
        Some(winner.doc_id.as_str())
    );
}

#[test]
fn generated_reply_cases_drive_claim_validation() {
    let cases = &crate::lean_vocab_test::lean_contract_snapshot().mailbox_reply_cases;
    assert_eq!(cases.len(), 216);
    assert!(cases
        .iter()
        .any(|case| case["variant"] == "expired-deadline"
            && case["status"] == "acted"
            && case["resolved_doc_id"] == "request-doc"
            && case["accepted"] == true));
    for case in cases {
        let text = |key: &str| case[key].as_str().unwrap();
        let item: MailboxItem = serde_json::from_value(json!({
            "_docID": text("mailbox_doc_id"), "item_key": "notice",
            "requester_did": "owner", "agent_did": "agent", "status": text("status"),
            "kind": "gate", "action": text("handling"), "title": "Repair",
            "source_kind": "session", "source_id": "source",
            "target_agent_did": text("target_agent_did"),
            "target_behavior_id": text("target_behavior_id"),
            "session_id": case["bound_session_id"],
            "resolved_doc_id": text("resolved_doc_id"), "created_at": "2026-01-01T00:00:00Z"
        }))
        .unwrap();
        let mut request = AgentRequest::try_from(
            serde_json::from_value::<gents_protocol::row::AgentRequestRow>(json!({
                "_docID": text("request_doc_id"), "request_id": "request",
                "agent_did": text("agent_did"), "requester_did": text("requester_did"),
                "behavior_id": text("behavior_id"), "session_id": text("session_id"),
                "caused_by_source_doc_id": text("source_doc_id"),
                "execution_origin": if case["interactive"] == true { "interactive" } else { "event" },
                "content": "Approved", "created_at": "2026-01-01T00:00:00Z"
            })).unwrap()
        ).unwrap();
        // Keep the empty-requester witness rather than relying on row normalization.
        request.requester_did = Some(text("requester_did").into());
        let result = reply::validate_reply_claim(
            &item,
            &request,
            case["authenticated"].as_bool().unwrap(),
            case["deadline_valid"].as_bool().unwrap(),
        );
        assert_eq!(
            result.is_ok(),
            case["accepted"].as_bool().unwrap(),
            "{case}"
        );
        if result.is_ok() {
            assert_eq!(case["post_status"], "acted");
            assert_eq!(case["post_resolved_doc_id"], request.doc_id);
        }
    }
}
