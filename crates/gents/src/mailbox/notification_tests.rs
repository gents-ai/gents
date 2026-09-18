use super::*;

fn stored_value(item: &MailboxItem) -> Value {
    let mut value = serde_json::to_value(item).unwrap();
    for field in ["created_at", "updated_at", "resolved_at", "deadline_at"] {
        if let Some(text) = value[field].as_str() {
            value[field] = json!(DateTime::parse_from_rfc3339(text).unwrap().to_rfc3339());
        }
    }
    value
}

#[test]
fn notification_outcomes_follow_lean_cases() {
    let cases = &crate::lean_vocab_test::lean_contract_snapshot().mailbox_notification_cases;
    assert_eq!(cases.len(), 8);
    for case in cases {
        let mode = if case["mode"] == "event" {
            NotificationIdentity::Event
        } else {
            NotificationIdentity::Condition { key: "disk".into() }
        };
        assert_eq!(
            serde_json::to_value(mode.write_outcome(
                case["open_exists"].as_bool().unwrap(),
                case["same_content"].as_bool().unwrap()
            ))
            .unwrap(),
            case["outcome"]
        );
    }
}

#[test]
fn notification_identity_is_configuration_and_runtime_owned() {
    let condition = NotificationIdentity::Condition { key: "disk".into() };
    assert_ne!(
        condition
            .source_id("agent-a", "alice", "behavior", "event")
            .unwrap(),
        condition
            .source_id("agent-b", "alice", "behavior", "event")
            .unwrap()
    );
    assert_ne!(
        condition
            .source_id("did:test:agent", "alice", "behavior", "event")
            .unwrap(),
        condition
            .source_id("did:test:agent", "bob", "behavior", "event")
            .unwrap()
    );
    assert_eq!(
        condition
            .source_id("did:test:agent", "did:test:owner", "behavior", "event-one")
            .unwrap(),
        condition
            .source_id("did:test:agent", "did:test:owner", "behavior", "event-two")
            .unwrap()
    );
    assert_ne!(
        condition
            .source_id("did:test:agent", "did:test:owner", "one", "event")
            .unwrap(),
        condition
            .source_id("did:test:agent", "did:test:owner", "two", "event")
            .unwrap()
    );
    assert_ne!(
        NotificationIdentity::Event
            .source_id("did:test:agent", "did:test:owner", "behavior", "one")
            .unwrap(),
        NotificationIdentity::Event
            .source_id("did:test:agent", "did:test:owner", "behavior", "two")
            .unwrap()
    );
    assert!(NotificationIdentity::Event
        .source_id("did:test:agent", "did:test:owner", "behavior", "")
        .is_err());
    assert!(NotificationIdentity::Condition { key: " ".into() }
        .source_id("did:test:agent", "did:test:owner", "behavior", "event")
        .is_err());
}

#[tokio::test]
async fn generated_notification_cases_drive_durable_writes() {
    let node = tests::test_node().await;
    let context = tests::context("did:test:owner");
    for (index, case) in crate::lean_vocab_test::lean_contract_snapshot()
        .mailbox_notification_cases
        .iter()
        .enumerate()
    {
        let identity = if case["mode"] == "event" {
            NotificationIdentity::Event
        } else {
            NotificationIdentity::Condition {
                key: format!("condition-{index}"),
            }
        };
        let source = identity
            .source_id(
                "did:test:agent",
                "did:test:owner",
                &context.behavior_id,
                &format!("event-{index}"),
            )
            .unwrap();
        let mut args = tests::args(MailboxAction::Ack, &source);
        let initial = if case["open_exists"] == true {
            Some(
                stamp_notification(
                    &node,
                    &context,
                    args.clone(),
                    &identity,
                    MAILBOX_CLOSE_COLLECTIONS,
                )
                .await
                .unwrap()
                .item,
            )
        } else {
            None
        };
        if case["same_content"] == false {
            args.title = "Updated reading: \"disk\"\n82%".into();
        }
        let receipt = stamp_notification(
            &node,
            &context,
            args.clone(),
            &identity,
            MAILBOX_CLOSE_COLLECTIONS,
        )
        .await
        .unwrap();
        assert_eq!(
            serde_json::to_value(receipt.outcome).unwrap(),
            case["outcome"]
        );
        let persisted = load_mailbox_item(&node, &receipt.item.doc_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored_value(&persisted), stored_value(&receipt.item));
        if let Some(initial) = initial {
            assert_eq!(initial.doc_id, receipt.item.doc_id);
            assert_eq!(initial.item_key, receipt.item.item_key);
            assert_eq!(
                stored_value(&initial)["created_at"],
                stored_value(&receipt.item)["created_at"]
            );
            if receipt.outcome == MailboxWriteOutcome::Reused {
                assert_eq!(initial.title, receipt.item.title);
            }
        }
        if receipt.outcome != MailboxWriteOutcome::Reused {
            assert_eq!(receipt.item.title, args.title);
        }
        dismiss_mailbox_item(&node, &receipt.item.doc_id, &context.requester_did)
            .await
            .unwrap();
        let terminal = load_mailbox_item(&node, &receipt.item.doc_id)
            .await
            .unwrap()
            .unwrap();
        let next = stamp_notification(&node, &context, args, &identity, MAILBOX_CLOSE_COLLECTIONS)
            .await
            .unwrap();
        assert_eq!(next.outcome, MailboxWriteOutcome::Created);
        assert_ne!(next.item.doc_id, terminal.doc_id);
        assert_eq!(
            serde_json::to_value(load_mailbox_item(&node, &terminal.doc_id).await.unwrap())
                .unwrap(),
            serde_json::to_value(Some(terminal)).unwrap()
        );
    }
    node.shutdown().await;
}

#[test]
fn notification_content_cannot_override_identity_or_policy() {
    for field in [
        "source_id",
        "source_kind",
        "request_id",
        "cause_doc_id",
        "requester_did",
        "kind",
        "action",
        "notification",
    ] {
        let mut input = json!({"title":"A finding"});
        input[field] = json!("forged");
        assert!(
            serde_json::from_value::<MailboxContentArgs>(input).is_err(),
            "accepted {field}"
        );
    }
    let mut decl = canonical_mailbox_write_decl();
    decl.notification.as_mut().unwrap().identity = NotificationIdentity::Condition {
        key: "monitor".into(),
    };
    validate_mailbox_write_decl(&decl).unwrap();
    decl.collection = "OtherCollection".into();
    assert!(validate_mailbox_write_decl(&decl).is_err());
    decl = canonical_mailbox_write_decl();
    decl.notification = None;
    assert!(validate_mailbox_write_decl(&decl).is_err());
}

#[tokio::test]
async fn notification_updates_reject_route_policy_and_terminal_races() {
    let node = tests::test_node().await;
    let context = tests::context("did:test:owner");
    let identity = NotificationIdentity::Condition {
        key: "monitor".into(),
    };
    let source = identity
        .source_id(
            "did:test:agent",
            "did:test:owner",
            &context.behavior_id,
            "request",
        )
        .unwrap();
    let mut args = tests::args(MailboxAction::Ack, &source);
    let first = stamp_notification(
        &node,
        &context,
        args.clone(),
        &identity,
        MAILBOX_CLOSE_COLLECTIONS,
    )
    .await
    .unwrap()
    .item;
    args.action = MailboxAction::StartRequest;
    assert!(stamp_notification(
        &node,
        &context,
        args.clone(),
        &identity,
        MAILBOX_CLOSE_COLLECTIONS
    )
    .await
    .is_err());
    args.action = MailboxAction::Ack;
    dismiss_mailbox_item(&node, &first.doc_id, &context.requester_did)
        .await
        .unwrap();
    args.title = "Must not overwrite terminal content".into();
    assert!(
        notification::reuse_or_update(&node, &context, &args, &identity, &first)
            .await
            .is_err()
    );
    let terminal = load_mailbox_item(&node, &first.doc_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(terminal.title, first.title);
    assert_eq!(terminal.parsed_status(), Some(MailboxStatus::Dismissed));
    node.shutdown().await;
}

#[tokio::test]
async fn content_tool_stamps_current_request_and_reuses_configured_condition() {
    use crate::llm::tool::Tool;
    use crate::tool_call_lifecycle::runtime::{
        scope_request_tool_execution, scope_tool_request_identity,
    };
    let node = tests::test_node().await;
    let mut context = tests::context("did:test:owner");
    context.session_id = None;
    let tool = MailboxCreateTool::new(
        node.clone(),
        MailboxNotificationPolicy {
            identity: NotificationIdentity::Condition {
                key: "monitor".into(),
            },
            ..Default::default()
        },
    );
    let mut doc_id = None;
    for (request, title, outcome) in [
        ("one", "Disk 81%, Docker unavailable", "created"),
        ("two", "Disk 82%, Docker unavailable", "updated"),
    ] {
        let result = node.execute(&format!(
            "mutation {{ create_AgentRequest(input: {{request_id: \"{request}\", agent_did: \"did:test:agent\", requester_did: \"did:test:owner\", behavior_id: \"operator\", caused_by_source_doc_id: \"input-{request}\"}}) {{_docID}} }}"
        )).await;
        assert!(!result.has_errors(), "{:?}", result.errors);
        let args = MailboxContentArgs {
            title: title.into(),
            summary: None,
            payload: None,
        };
        let receipt = scope_tool_request_identity(
            Some(context.requester_did.clone()),
            Some(context.agent_did.clone()),
            Some(context.behavior_id.clone()),
            Some(request.into()),
            scope_request_tool_execution(
                None,
                tokio_util::sync::CancellationToken::new(),
                tool.call(args),
            ),
        )
        .await
        .unwrap();
        let receipt: Value = serde_json::from_str(&receipt).unwrap();
        assert_eq!(receipt["outcome"], outcome);
        assert_eq!(receipt["item"]["title"], title);
        assert_eq!(receipt["item"]["requester_did"], context.requester_did);
        // Creation provenance stays immutable when current content changes.
        assert_eq!(receipt["item"]["request_id"], "one");
        assert_eq!(receipt["item"]["cause_doc_id"], "input-one");
        let current = receipt["item"]["_docID"].clone();
        if let Some(previous) = &doc_id {
            assert_eq!(previous, &current);
        }
        doc_id = Some(current);
    }
    let mut wrong = context.clone();
    wrong.requester_did = "did:test:other".into();
    assert!(notification::request_provenance(&node, "one", &wrong)
        .await
        .is_err());
    node.shutdown().await;
}

#[tokio::test]
async fn concurrent_notifications_converge_without_cross_requester_collisions() {
    let node = tests::test_node().await;
    let identity = NotificationIdentity::Condition {
        key: "monitor".into(),
    };
    let mut item_ids = Vec::new();
    for owner in ["did:test:alice", "did:test:bob"] {
        let context = tests::context(owner);
        let source = identity
            .source_id("did:test:agent", owner, &context.behavior_id, "request")
            .unwrap();
        let args = tests::args(MailboxAction::Ack, &source);
        let receipts = futures::future::join_all((0..4).map(|_| {
            stamp_notification(
                &node,
                &context,
                args.clone(),
                &identity,
                MAILBOX_CLOSE_COLLECTIONS,
            )
        }))
        .await
        .into_iter()
        .collect::<Result<Vec<_>>>()
        .unwrap();
        assert_eq!(
            receipts
                .iter()
                .filter(|r| r.outcome == MailboxWriteOutcome::Created)
                .count(),
            1
        );
        let first = &receipts[0].item;
        assert!(receipts
            .iter()
            .all(|r| r.item.doc_id == first.doc_id && r.item.requester_did == owner));
        let items = list_mailbox_items(&node, owner, Some(MailboxStatus::Open))
            .await
            .unwrap();
        assert_eq!(items.len(), 1);
        item_ids.push(first.doc_id.clone());
    }
    assert_ne!(item_ids[0], item_ids[1]);
    node.shutdown().await;
}

#[tokio::test]
async fn provenance_keeps_execution_owner_resolved_selection() {
    let node = tests::test_node().await;
    let context = tests::context("did:test:owner");
    let response = node.execute("mutation { create_AgentRequest(input: {request_id: \"default-selection\", agent_did: \"did:test:agent\", requester_did: \"did:test:owner\"}) {_docID} }").await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    // The execution owner may resolve the default behavior and create a session.
    notification::request_provenance(&node, "default-selection", &context)
        .await
        .unwrap();
    let mut foreign = context.clone();
    foreign.agent_did = "did:test:other-agent".into();
    assert!(
        notification::request_provenance(&node, "default-selection", &foreign)
            .await
            .is_err()
    );
    node.shutdown().await;
}
