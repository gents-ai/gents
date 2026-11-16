use super::*;
use gents_documents::mailbox_write_decl::validate_mailbox_write_decl;

pub(super) fn stored_value(item: &MailboxItem) -> Value {
    let mut value = serde_json::to_value(item).unwrap();
    for field in ["created_at", "updated_at", "resolved_at", "deadline_at"] {
        if let Some(text) = value[field].as_str() {
            value[field] = json!(DateTime::parse_from_rfc3339(text).unwrap().to_rfc3339());
        }
    }
    value
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
    let access = tests::agent_access(&node);
    let context = tests::context("did:test:owner");
    for (index, case) in gents_lean_vocab::families::mailbox_notification_contracts_stamp::cases::<
        String,
        MailboxWriteOutcome,
    >()
    .into_iter()
    .enumerate()
    {
        let identity = match case.input.mode.as_str() {
            "event" => NotificationIdentity::Event,
            "condition" => NotificationIdentity::Condition {
                key: format!("condition-{index}"),
            },
            other => panic!("unmapped notification mode {other}"),
        };
        let source = identity
            .source_id(
                &context.node_did,
                &context.requester_did,
                &context.agent_did,
                &format!("event-{index}"),
            )
            .unwrap();
        let mut args = tests::args(MailboxAction::Ack, &source);
        let initial = if case.input.open_exists {
            Some(
                stamp_notification(
                    &node,
                    &access,
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
        if !case.input.same_content {
            args.title = "Updated reading: \"disk\"\n82%".into();
        }
        let receipt = stamp_notification(
            &node,
            &access,
            &context,
            args.clone(),
            &identity,
            MAILBOX_CLOSE_COLLECTIONS,
        )
        .await
        .unwrap();
        let persisted = tests::load_as_creator(&node, &receipt.item.doc_id)
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
        tests::dismiss_as_creator(&node, &receipt.item.doc_id)
            .await
            .unwrap();
        let terminal = tests::load_as_creator(&node, &receipt.item.doc_id)
            .await
            .unwrap()
            .unwrap();
        let next = stamp_notification(
            &node,
            &access,
            &context,
            args,
            &identity,
            MAILBOX_CLOSE_COLLECTIONS,
        )
        .await
        .unwrap();
        assert_ne!(next.item.doc_id, terminal.doc_id);
        assert_eq!(
            serde_json::to_value(
                tests::load_as_creator(&node, &terminal.doc_id)
                    .await
                    .unwrap()
            )
            .unwrap(),
            serde_json::to_value(Some(terminal)).unwrap()
        );
        case.check(
            gents_lean_vocab::families::mailbox_notification_contracts_stamp::StampOutput {
                outcome: receipt.outcome,
                after_dismiss: next.outcome,
            },
        );
    }
    node.shutdown().await.expect("shut down test node");
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
    let access = tests::agent_access(&node);
    let context = tests::context("did:test:owner");
    let identity = NotificationIdentity::Condition {
        key: "monitor".into(),
    };
    let source = identity
        .source_id(
            &context.node_did,
            &context.requester_did,
            &context.agent_did,
            "request",
        )
        .unwrap();
    let mut args = tests::args(MailboxAction::Ack, &source);
    let first = stamp_notification(
        &node,
        &access,
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
        &access,
        &context,
        args.clone(),
        &identity,
        MAILBOX_CLOSE_COLLECTIONS
    )
    .await
    .is_err());
    args.action = MailboxAction::Ack;
    tests::dismiss_as_creator(&node, &first.doc_id)
        .await
        .unwrap();
    args.title = "Must not overwrite terminal content".into();
    assert!(
        notification::reuse_or_update(&node, &access, &context, &args, &identity, &first)
            .await
            .is_err()
    );
    let terminal = tests::load_as_creator(&node, &first.doc_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(terminal.title, first.title);
    assert_eq!(terminal.parsed_status(), Some(MailboxStatus::Dismissed));
    node.shutdown().await.expect("shut down test node");
}

#[tokio::test]
async fn content_tool_provenance_follows_lean_model() {
    use gents_loop::tool::{AdmittedClientContext, Tool, ToolRequestContext};
    let cases = gents_lean_vocab::families::mailbox_content_provenance::cases();
    for case in &cases {
        let node = tests::test_node().await;
        let access = tests::agent_access(&node);
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
        )
        .with_access(access.clone());
        let mut observations = Vec::new();
        let mut previous_doc: Option<String> = None;
        for step in &case.input.steps {
            let request_doc = format!("request-{}", step.request_id);
            let request_context = ToolRequestContext {
                request_id: step.request_id.clone(),
                request_doc_id: request_doc.clone(),
                node_did: context.node_did.clone(),
                agent_did: context.agent_did.clone(),
                session_id: "session".into(),
                caused_by_source_doc_id: Some(step.cause_doc_id.clone()),
                admitted_client: Some(AdmittedClientContext {
                    signer_did: tests::fixture_recipient(&step.requester),
                    request_doc_id: request_doc,
                    session_doc_id: "session-doc".into(),
                    agent_doc_id: "agent-doc".into(),
                }),
            };
            let args = MailboxContentArgs {
                title: step.title.clone(),
                summary: None,
                payload: None,
                question: None,
            };
            let receipt = tool
                .call_with_request(args, &request_context)
                .await
                .unwrap();
            let receipt: Value = serde_json::from_str(&receipt).unwrap();
            let doc = receipt["item"]["_docID"].as_str().unwrap().to_owned();
            observations.push(
                gents_lean_vocab::families::mailbox_content_provenance::Observation {
                    outcome: receipt["outcome"].as_str().unwrap().to_owned(),
                    title: receipt["item"]["title"].as_str().unwrap().to_owned(),
                    requester_admitted: receipt["item"]["requester_did"]
                        == tests::fixture_recipient(&step.requester),
                    request_id: receipt["item"]["request_id"].as_str().map(str::to_owned),
                    cause_doc_id: receipt["item"]["cause_doc_id"].as_str().map(str::to_owned),
                    same_document: previous_doc.as_deref() == Some(doc.as_str()),
                },
            );
            previous_doc = Some(doc);
        }
        case.check(gents_lean_vocab::families::mailbox_content_provenance::Output { observations });
        node.shutdown().await.expect("shut down test node");
    }
}

#[tokio::test]
async fn concurrent_notifications_converge_without_cross_requester_collisions() {
    let node = tests::test_node().await;
    let access = tests::agent_access(&node);
    let identity = NotificationIdentity::Condition {
        key: "monitor".into(),
    };
    let mut item_ids = Vec::new();
    for owner in ["did:test:alice", "did:test:bob"] {
        let context = tests::context(owner);
        let source = identity
            .source_id(
                &context.node_did,
                &context.requester_did,
                &context.agent_did,
                "request",
            )
            .unwrap();
        let args = tests::args(MailboxAction::Ack, &source);
        let receipts = futures::future::join_all((0..4).map(|_| {
            stamp_notification(
                &node,
                &access,
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
        assert!(receipts.iter().all(
            |r| r.item.doc_id == first.doc_id && r.item.requester_did == context.requester_did
        ));
        let items = tests::list_as_creator(&node, owner, Some(MailboxStatus::Open))
            .await
            .unwrap();
        assert_eq!(items.len(), 1);
        item_ids.push(first.doc_id.clone());
    }
    assert_ne!(item_ids[0], item_ids[1]);
    node.shutdown().await.expect("shut down test node");
}

#[tokio::test]
async fn question_files_an_ask_whatever_the_surface_policy_and_refuses_invalid_payloads() {
    use gents_loop::tool::{AdmittedClientContext, Tool, ToolRequestContext};
    use gents_protocol::mailbox_question::{
        MailboxQuestion, MailboxQuestionOption, MAILBOX_QUESTION_VERSION,
    };
    let node = tests::test_node().await;
    let access = tests::agent_access(&node);
    let context = tests::context("did:test:owner");
    let tool = MailboxCreateTool::new(node.clone(), MailboxNotificationPolicy::default())
        .with_access(access.clone());
    let request_context = ToolRequestContext {
        request_id: "ask".into(),
        request_doc_id: "request-doc".into(),
        node_did: context.node_did.clone(),
        agent_did: context.agent_did.clone(),
        session_id: "session".into(),
        caused_by_source_doc_id: None,
        admitted_client: Some(AdmittedClientContext {
            signer_did: context.requester_did.clone(),
            request_doc_id: "request-doc".into(),
            session_doc_id: "session-doc".into(),
            agent_doc_id: "agent-doc".into(),
        }),
    };
    let question = |prompt: &str, options: &[&str]| MailboxQuestion {
        version: MAILBOX_QUESTION_VERSION,
        prompt: prompt.into(),
        options: options
            .iter()
            .map(|id| MailboxQuestionOption {
                id: (*id).into(),
                label: id.to_uppercase(),
                description: None,
                prompt: None,
            })
            .collect(),
        multi_select: false,
        allow_free_text: true,
        optional: None,
        recommended_order: None,
    };
    let call = |args: MailboxContentArgs| tool.call_with_request(args, &request_context);
    let content = |title: &str, payload: Option<&str>, question: Option<MailboxQuestion>| {
        MailboxContentArgs {
            title: title.into(),
            summary: None,
            payload: payload.map(str::to_owned),
            question,
        }
    };
    for invalid in [
        content("One option", None, Some(question("Ship?", &["yes"]))),
        content("Duplicate", None, Some(question("Ship?", &["yes", "yes"]))),
        content(
            "Both",
            Some("extra"),
            Some(question("Ship?", &["yes", "no"])),
        ),
    ] {
        assert!(call(invalid).await.is_err());
    }
    assert!(tests::list_as_creator(&node, &context.requester_did, None)
        .await
        .unwrap()
        .is_empty());

    let first = question("Ship the release?", &["yes", "no"]);
    let receipt: Value = serde_json::from_str(
        &call(content("Release", None, Some(first.clone())))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(receipt["outcome"], "created");
    assert_eq!(receipt["item"]["kind"], "ask");
    assert_eq!(receipt["item"]["action"], "start_request");
    let stored = MailboxQuestion::from_payload(receipt["item"]["payload"].as_str().unwrap());
    assert_eq!(stored.unwrap(), first);
    let second: Value = serde_json::from_str(
        &call(content(
            "Backend",
            None,
            Some(question("Which backend?", &["a", "b"])),
        ))
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(second["outcome"], "created");
    assert_ne!(second["item"]["_docID"], receipt["item"]["_docID"]);
    let retitled: Value = serde_json::from_str(
        &call(content("Release B", None, Some(first.clone())))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(retitled["outcome"], "created");
    assert_ne!(retitled["item"]["_docID"], receipt["item"]["_docID"]);
    let again: Value = serde_json::from_str(
        &call(content("Release", None, Some(first.clone())))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(again["outcome"], "reused");
    assert_eq!(again["item"]["_docID"], receipt["item"]["_docID"]);
    let flag: Value =
        serde_json::from_str(&call(content("Note", Some("fyi"), None)).await.unwrap()).unwrap();
    assert_eq!(flag["item"]["kind"], "flag");
    assert_eq!(flag["item"]["action"], "ack");
    node.shutdown().await.expect("shut down test node");
}
