use super::*;
use crate::request::watcher::AgentRequest;
use crate::store::identity::{KeyIdentity, NodeIdentity};

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
    persisted_request(
        node,
        identity,
        Some(&item.doc_id),
        id,
        tamper_signature,
        "Approved",
        None,
    )
    .await
}

async fn persisted_request(
    node: &EmbeddedNode,
    identity: &KeyIdentity,
    source_doc_id: Option<&str>,
    id: &str,
    tamper_signature: bool,
    content: &str,
    question_reply: Option<&gents_protocol::mailbox_question::MailboxQuestionReply>,
) -> AgentRequest {
    use gents_protocol::request_admission::{AgentRequestAdmissionRecord, AgentRequestCreate};
    let context = defra_core::RequestContext::default()
        .with_identity(Some(tests::root_identity().did().to_owned()));
    for permission in [
        acp::nac::NodePermission::DocumentRead,
        acp::nac::NodePermission::DocumentUpdate,
    ] {
        node.add_nac_actor_relationship_as(&context, permission.as_str(), identity.did())
            .await
            .unwrap();
    }
    let mut create = AgentRequestCreate::base(
        gents_protocol::request_admission::RequestPurpose::Normal,
        id,
        tests::root_identity().did(),
        identity.did(),
        tests::agent_identity().did(),
        "session-1",
        content,
        "interactive",
        Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        AgentRequestAdmissionRecord::local_self(identity.did()),
    );
    create.caused_by_source_doc_id = source_doc_id.map(str::to_owned);
    create.input.question_reply = question_reply.cloned();
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
    node.add_dac_actor_relationship(
        tests::root_identity().did(),
        "AgentRequest",
        doc_id,
        "working_agent",
        tests::agent_identity().did(),
    )
    .await
    .unwrap();
    let query = format!(
        r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
        escape_graphql_string(doc_id),
        crate::request::admission::SIGNED_REQUEST_FIELDS
    );
    let response = node.execute(&query).await;
    let record = rows::<gents_protocol::row::AgentRequestRow>(&response, "AgentRequest")
        .unwrap()
        .remove(0);
    AgentRequest::try_from(record).unwrap()
}

async fn claim(node: &Arc<EmbeddedNode>, request: &AgentRequest, abort: bool) -> Result<()> {
    tests::agent_access(node).transact_write("test.mailbox_reply", |txn| Box::pin(async move {
        let mutation = format!(r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ lifecycle_state: "claimed" }}) {{ _docID }} }}"#,
            escape_graphql_string(&request.doc_id));
        txn.execute_local_response(&mutation).await?;
        claim_reply_in_txn(txn, request, &Utc::now().to_rfc3339()).await?;
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
    context.node_did = tests::root_identity().did().into();
    let item = tests::stamp_create(
        &node,
        &context,
        tests::args(MailboxAction::StartRequest, "approval"),
    )
    .await
    .unwrap();
    let request = signed_reply(&node, &identity, &item, "approved-request").await;
    let forged = persisted_reply(&node, &identity, &item, "forged-request", true).await;
    assert!(crate::request::lifecycle::is_claim_admission_error(
        &claim(&node, &forged, false).await.unwrap_err()
    ));
    assert_eq!(stored_lifecycle(&node, &forged).await, "pending");
    assert!(claim(&node, &request, true).await.is_err());
    assert_eq!(stored_lifecycle(&node, &request).await, "pending");
    assert_eq!(
        tests::load_as_creator(&node, &item.doc_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        "open"
    );
    assert_eq!(
        sweep_open_mailbox_items(
            &node,
            tests::root_identity().did(),
            &[tests::agent_identity()],
            None
        )
        .await
        .unwrap()
        .report
        .acted,
        0
    );
    claim(&node, &request, false).await.unwrap();
    claim(&node, &request, false).await.unwrap();
    let replay = signed_reply(&node, &identity, &item, "replayed-request").await;
    let error = claim(&node, &replay, false).await.unwrap_err();
    assert!(crate::request::lifecycle::is_claim_admission_error(&error));
    assert_eq!(stored_lifecycle(&node, &replay).await, "pending");
    let stored = tests::load_as_creator(&node, &item.doc_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        stored.resolved_doc_id.as_deref(),
        Some(request.doc_id.as_str())
    );
    let declined = tests::stamp_create(
        &node,
        &context,
        tests::args(MailboxAction::StartRequest, "declined"),
    )
    .await
    .unwrap();
    let denied = signed_reply(&node, &identity, &declined, "declined-request").await;
    tests::dismiss_as_creator(&node, &declined.doc_id)
        .await
        .unwrap();
    assert!(crate::request::lifecycle::is_claim_admission_error(
        &claim(&node, &denied, false).await.unwrap_err()
    ));
    assert_eq!(
        tests::load_as_creator(&node, &declined.doc_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        "dismissed"
    );
    let raced = tests::stamp_create(
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
        tests::load_as_creator(&node, &raced.doc_id)
            .await
            .unwrap()
            .unwrap()
            .resolved_doc_id
            .as_deref(),
        Some(winner.doc_id.as_str())
    );
}

#[tokio::test]
async fn modern_questions_require_correlated_replies_and_preserve_skip_claims() {
    use gents_protocol::mailbox_question::{MailboxQuestion, MailboxQuestionAnswer};
    let node = tests::test_node().await;
    let temp = tempfile::tempdir().unwrap();
    let identity = KeyIdentity::load_or_create(temp.path().join("questions.key"), None).unwrap();
    let mut context = tests::context(identity.did());
    context.node_did = tests::root_identity().did().into();
    for optional in [false, true] {
        let payload = json!({
            "version": 2, "prompt": "Any further constraints?", "options": [],
            "allow_free_text": true, "optional": optional,
        })
        .to_string();
        let question = MailboxQuestion::from_payload(&payload).unwrap();
        let mut args = tests::args(MailboxAction::StartRequest, &format!("question-{optional}"));
        args.payload = Some(payload);
        let item = tests::stamp_create(&node, &context, args).await.unwrap();
        let error = tests::dismiss_as_creator(&node, &item.doc_id)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("original item"));
        assert_eq!(
            tests::load_as_creator(&node, &item.doc_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            "open"
        );
        let skipped = question.reply_content(
            "Constraints",
            &MailboxQuestionAnswer {
                skip: Some(true),
                ..Default::default()
            },
        );
        assert_eq!(skipped.is_ok(), optional);
        let answer = MailboxQuestionAnswer {
            question_hash: Some(
                gents_protocol::mailbox_question::question_source_hash(
                    &item.title,
                    item.summary.as_deref(),
                    item.payload.as_deref().unwrap(),
                    item.request_id.as_deref(),
                )
                .unwrap(),
            ),
            skip: optional.then_some(true),
            free_text: (!optional).then(|| "Keep the existing data".into()),
            ..Default::default()
        };
        let content = question.reply_content(&item.title, &answer).unwrap();
        let evidence = gents_protocol::mailbox_question::MailboxQuestionReply::new(
            &item.doc_id,
            &item.title,
            item.summary.as_deref(),
            item.payload.as_deref().unwrap(),
            item.request_id.as_deref(),
            answer,
        )
        .unwrap();
        let request = persisted_request(
            &node,
            &identity,
            Some(&item.doc_id),
            &format!("reply-{optional}"),
            false,
            &content,
            Some(&evidence),
        )
        .await;
        assert_eq!(request.content, content);
        let mut stale = item.clone();
        stale.summary = Some("A changed decision context".into());
        assert!(reply::validate_reply_claim(&stale, &request, true, true, true).is_err());
        let mut missing = request.clone();
        missing.input.question_reply = None;
        assert!(reply::validate_reply_claim(&item, &missing, true, true, true).is_err());
        assert!(claim(&node, &request, true).await.is_err());
        assert_eq!(
            tests::load_as_creator(&node, &item.doc_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            "open"
        );
        claim(&node, &request, false).await.unwrap();
        claim(&node, &request, false).await.unwrap();
        let observed = tests::dismiss_as_creator(&node, &item.doc_id)
            .await
            .unwrap();
        assert_eq!(observed.status, "acted");
        let mut changed_after_claim = observed.clone();
        changed_after_claim.summary = Some("Later context".into());
        assert!(
            reply::validate_reply_claim(&changed_after_claim, &request, true, false, false).is_ok()
        );
        assert_eq!(
            observed.resolved_doc_id.as_deref(),
            Some(request.doc_id.as_str())
        );
        let replay = signed_reply(&node, &identity, &item, &format!("replay-{optional}")).await;
        assert!(claim(&node, &replay, false).await.is_err());
        assert_eq!(stored_lifecycle(&node, &replay).await, "pending");
    }
    node.shutdown().await.expect("shut down test node");
}

#[tokio::test]
async fn modeled_handoff_positive_sequence_maps_to_native_owners() {
    use crate::request::lifecycle::{
        ClaimOutcome, ExecutionOrigin, RequestLifecycle, RequestTerminalOutcome, TerminalizeResult,
    };
    use crate::request::streaming::DefraStreamWriter;
    use std::time::Duration;

    let modeled = crate::lean_vocab_test::lean_contract_snapshot()
        .mailbox_handoff_cases
        .iter()
        .find(|case| case.variant == "fresh-linked")
        .expect("modeled linked handoff case");
    assert_eq!(modeled.request_id, modeled.producer_request_id);
    assert_eq!(modeled.request_id, modeled.tool_request_id);
    assert_eq!(modeled.session_id, modeled.reply_session_id);
    assert_eq!(
        modeled.reply_bound_session_id.as_deref(),
        Some(modeled.session_id.as_str())
    );
    assert_eq!(modeled.producer_node_did, modeled.question_node_did);
    assert_eq!(
        modeled.producer_requester_did,
        modeled.question_requester_did
    );
    let node = tests::test_node().await;
    let temp = tempfile::tempdir().unwrap();
    let identity = KeyIdentity::load_or_create(temp.path().join("handoff.key"), None).unwrap();
    let producer = persisted_request(
        &node,
        &identity,
        None,
        "producer-request",
        false,
        "Approved",
        None,
    )
    .await;
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        node.clone(),
        tests::agent_identity().did(),
        tests::root_identity().did(),
        producer.clone(),
        60,
        ExecutionOrigin::Interactive,
        "handoff-test",
    );
    lifecycle
        .bind_managed_agent(tests::agent_identity().as_ref())
        .unwrap();
    assert_eq!(
        lifecycle.claim_with_identity().await.unwrap(),
        ClaimOutcome::Claimed
    );
    let writer =
        DefraStreamWriter::new_for_lifecycle(node.clone(), &lifecycle, Duration::ZERO).unwrap();
    lifecycle.begin_owned_execution(&writer).await.unwrap();

    let context = tests::context(identity.did());
    let notification_identity = NotificationIdentity::Event;
    let source_id = notification_identity
        .source_id(
            &context.node_did,
            &context.requester_did,
            &context.agent_did,
            &producer.request_id,
        )
        .unwrap();
    let receipt = stamp_notification(
        &node,
        &tests::agent_access(&node),
        &context,
        FileMailboxItemArgs {
            kind: serde_json::from_value(json!(modeled.kind)).unwrap(),
            action: serde_json::from_value(json!(modeled.action)).unwrap(),
            title: modeled.content.clone(),
            summary: None,
            payload: None,
            source_kind: MailboxSourceKind::Agent,
            source_id,
            session_id: None,
            request_id: Some(producer.request_id.clone()),
            cause_doc_id: None,
            expected_collection: None,
            parent_item_id: None,
            deadline_at: None,
        },
        &notification_identity,
        MAILBOX_CLOSE_COLLECTIONS,
    )
    .await
    .unwrap();
    let receipt = serde_json::to_value(receipt).unwrap();
    assert_eq!(receipt["outcome"], "created");
    let mailbox_doc_id = receipt["item"]["_docID"].as_str().unwrap();
    let filed = tests::load_as_creator(&node, mailbox_doc_id)
        .await
        .unwrap()
        .unwrap();
    let returned: MailboxItem = serde_json::from_value(receipt["item"].clone()).unwrap();
    assert_eq!(
        super::notification_tests::stored_value(&filed),
        super::notification_tests::stored_value(&returned)
    );
    assert_eq!(filed.status == "open", modeled.stored_open_receipt);
    assert_eq!(
        filed.request_id.as_deref(),
        Some(producer.request_id.as_str())
    );
    assert_eq!(
        filed.session_id.as_deref(),
        Some(producer.session_id.as_str())
    );

    assert!(modeled.handoff_accepted);
    assert_eq!(
        lifecycle
            .terminalize_owned(
                RequestTerminalOutcome::Completed,
                gents_protocol::output::TerminalOutput::NoMessage,
                None,
            )
            .await
            .unwrap(),
        TerminalizeResult::Won,
    );
    assert_eq!(
        tests::load_as_creator(&node, mailbox_doc_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        "open"
    );

    let reply = signed_reply(&node, &identity, &filed, "reply-request").await;
    assert_ne!(reply.doc_id, producer.doc_id);
    assert_eq!(reply.session_id, producer.session_id);
    assert_eq!(
        reply.caused_by_source_doc_id.as_deref(),
        Some(mailbox_doc_id)
    );
    let mut reply_lifecycle = RequestLifecycle::new_with_execution_binding(
        node.clone(),
        tests::agent_identity().did(),
        tests::root_identity().did(),
        reply.clone(),
        60,
        ExecutionOrigin::Interactive,
        "handoff-test",
    );
    reply_lifecycle
        .bind_managed_agent(tests::agent_identity().as_ref())
        .unwrap();
    assert_eq!(
        reply_lifecycle.claim_with_identity().await.unwrap(),
        ClaimOutcome::Claimed
    );
    let consumed = tests::load_as_creator(&node, mailbox_doc_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(consumed.status, "acted");
    assert_eq!(
        consumed.resolved_doc_id.as_deref(),
        Some(reply.doc_id.as_str())
    );
    assert!(modeled.linked_reply_accepted);
    node.shutdown().await.expect("shut down test node");
}

#[test]
fn generated_reply_cases_drive_claim_validation() {
    let cases = &crate::lean_vocab_test::lean_contract_snapshot().mailbox_reply_cases;
    assert!(!cases.is_empty());
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
            "requester_did": "owner", "node_did": "agent", "status": text("status"),
            "kind": "gate", "action": text("handling"), "title": "Repair",
            "source_kind": "session", "source_id": "source",
            "target_node_did": text("target_node_did"),
            "target_agent_did": text("target_agent_did"),
            "session_id": case["bound_session_id"],
            "resolved_doc_id": text("resolved_doc_id"), "created_at": "2026-01-01T00:00:00Z"
        }))
        .unwrap();
        let mut request = AgentRequest::try_from(
            serde_json::from_value::<gents_protocol::row::AgentRequestRow>(json!({
                "_docID": text("request_doc_id"), "request_id": "request",
                "purpose": "normal",
                "node_did": text("node_did"), "requester_did": text("requester_did"),
                "agent_did": text("agent_did"), "session_id": text("session_id"),
                "caused_by_source_doc_id": text("source_doc_id"),
                "execution_origin": if case["interactive"] == true { "interactive" } else { "event" },
                "content": "Approved", "created_at": "2026-01-01T00:00:00Z"
            })).unwrap()
        ).unwrap();
        request.requester_did = Some(text("requester_did").into());
        if case["question_current"] == false {
            request.input.question_reply =
                Some(gents_protocol::mailbox_question::MailboxQuestionReply {
                    question_version: None,
                    source_doc_id: "other-question".into(),
                    question_hash: "invalid".into(),
                    answer: Default::default(),
                });
        }
        let result = reply::validate_reply_claim(
            &item,
            &request,
            case["authenticated"].as_bool().unwrap(),
            case["recipient_authorized"].as_bool().unwrap(),
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
