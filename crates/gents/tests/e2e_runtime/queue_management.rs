use gents::agent::p2p_reconcile::session_commands::run_session_command_reconciler;
use gents::config_client::ConfigAccess;
use gents::graphql::escape_graphql_string;
use gents::lifecycle::{
    pending_steer_edit, pending_user_queue, submit_pending_user_edit, submit_pending_user_steer,
    PendingMessageEdit, PendingQueueEdit,
};
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use gents_protocol::request_input::QueueDelivery;
use gents_protocol::session_input_edit::SessionInputEditReceipt;

use crate::support::{test_db, TestDb};

pub(super) async fn enqueue(
    db: &TestDb,
    agent: &str,
    session: &str,
    id: &str,
    text: &str,
    delivery: QueueDelivery,
) -> String {
    let did = db.node_identity.did();
    let mut request = AgentRequestCreate::base(
        RequestPurpose::Normal,
        id,
        did,
        did,
        agent,
        session,
        text,
        "interactive",
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        AgentRequestAdmissionRecord::local_self(did),
    );
    request.input = gents::lifecycle::prepare_user_message_input(
        &ConfigAccess::local(db.node.clone()),
        did,
        session,
        request.input,
        delivery,
    )
    .await
    .unwrap();
    insert_signed(db, request).await
}

pub(super) async fn insert_signed(db: &TestDb, mut request: AgentRequestCreate) -> String {
    gents::sign_agent_request_create(db.node_identity.as_ref(), &mut request)
        .await
        .unwrap();
    let response = ConfigAccess::local(db.node.clone())
        .write(
            "test.queue_signed_input",
            &request.graphql_mutation().unwrap(),
        )
        .await
        .unwrap();
    gents::graphql::created_doc_id(&response, "AgentRequest").unwrap()
}

async fn submit_edit(
    db: &TestDb,
    session_id: &str,
    edit: PendingQueueEdit,
) -> anyhow::Result<SessionInputEditReceipt> {
    submit_edit_inner(db, session_id, edit, None).await
}

async fn submit_edit_inner(
    db: &TestDb,
    session_id: &str,
    edit: PendingQueueEdit,
    steer: Option<&str>,
) -> anyhow::Result<SessionInputEditReceipt> {
    let did = db.node_identity.did();
    let existing = ConfigAccess::local(db.node.clone())
        .execute(&format!(
            r#"{{ AgentSession(filter: {{ session_id: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
            escape_graphql_string(session_id)
        ))
        .await?;
    if existing["data"]["AgentSession"]
        .as_array()
        .is_none_or(Vec::is_empty)
    {
        let mut session = crate::support::session_document_in_scope(
            did,
            session_id,
            "general",
            "2026-10-10T00:00:00Z",
        );
        session.requester_did = Some(did.into());
        crate::support::create_session_document(&db.node, &session).await;
    }
    let cancel = tokio_util::sync::CancellationToken::new();
    let worker = tokio::spawn(run_session_command_reconciler(
        db.node.clone(),
        db.node_identity.clone(),
        cancel.clone(),
    ));
    let access = ConfigAccess::local(db.node.clone());
    let result = match steer {
        Some(selected) => {
            submit_pending_user_steer(
                &access,
                db.node_identity.as_ref(),
                AgentRequestAdmissionRecord::local_self(did),
                None,
                did,
                session_id,
                edit,
                selected,
                None,
            )
            .await
        }
        None => {
            submit_pending_user_edit(
                &access,
                db.node_identity.as_ref(),
                AgentRequestAdmissionRecord::local_self(did),
                None,
                did,
                session_id,
                edit,
            )
            .await
        }
    };
    cancel.cancel();
    worker.await??;
    result
}

pub(super) async fn row(db: &TestDb, doc: &str) -> serde_json::Value {
    let response = ConfigAccess::local(db.node.clone()).execute(&format!(r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ _docID request_id content input lifecycle_state failure_reason admission_signature superseded_by_request_doc_id terminal_output }} }}"#, escape_graphql_string(doc))).await.unwrap();
    let rows = response["data"]["AgentRequest"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    rows[0].clone()
}

#[tokio::test]
async fn signed_queue_edit_reorders_preserves_slots_and_cancels_atomically() {
    let db = test_db("signed-queue-management").await;
    let did = db.node_identity.did();
    let access = ConfigAccess::local(db.node.clone());
    let first = enqueue(
        &db,
        "general",
        "edit-session",
        "edit-first",
        "first original",
        QueueDelivery::Steer,
    )
    .await;
    let second = enqueue(
        &db,
        "general",
        "edit-session",
        "edit-second",
        "second original",
        QueueDelivery::Steer,
    )
    .await;
    let original_first = row(&db, &first).await;
    let original_second = row(&db, &second).await;
    let snapshot = pending_user_queue(&access, did, "edit-session", did)
        .await
        .unwrap();
    assert_eq!(
        snapshot
            .entries
            .iter()
            .map(|entry| &entry.request_doc_id)
            .collect::<Vec<_>>(),
        vec![&first, &second]
    );
    assert!(snapshot.entries.iter().all(|entry| entry.editable));
    let receipt = submit_edit(
        &db,
        "edit-session",
        PendingQueueEdit {
            expected_request_doc_ids: vec![first.clone(), second.clone()],
            selected_request_doc_ids: vec![first.clone(), second.clone()],
            messages: vec![
                PendingMessageEdit {
                    authored_input: None,
                    request_doc_id: second.clone(),
                    content: "second edited".into(),
                },
                PendingMessageEdit {
                    authored_input: None,
                    request_doc_id: first.clone(),
                    content: "first edited".into(),
                },
            ],
        },
    )
    .await
    .unwrap();
    assert_eq!(receipt.request_doc_ids.len(), 2);
    for (index, (old_id, old)) in [(&first, &original_first), (&second, &original_second)]
        .into_iter()
        .enumerate()
    {
        let old_after = row(&db, old_id).await;
        assert_eq!(old_after["content"], old["content"]);
        assert_eq!(old_after["admission_signature"], old["admission_signature"]);
        assert_eq!(old_after["lifecycle_state"], "superseded");
        assert_eq!(
            old_after["superseded_by_request_doc_id"],
            receipt.request_doc_ids[index]
        );
        let replacement = row(&db, &receipt.request_doc_ids[index]).await;
        assert_eq!(
            replacement["content"],
            ["second edited", "first edited"][index]
        );
        assert!(replacement["admission_signature"]
            .as_str()
            .is_some_and(|signature| !signature.is_empty()));
        assert_ne!(
            replacement["admission_signature"],
            old["admission_signature"]
        );
        assert_eq!(
            replacement["input"]["queue"]["position"]["slot_request_doc_id"],
            *old_id
        );
        assert_eq!(
            replacement["input"]["queue"]["position"]["replaces_request_doc_id"],
            *old_id
        );
    }
    let snapshot = pending_user_queue(&access, did, "edit-session", did)
        .await
        .unwrap();
    assert_eq!(
        snapshot
            .entries
            .iter()
            .map(|entry| entry.content.as_str())
            .collect::<Vec<_>>(),
        vec!["second edited", "first edited"]
    );
    let ids = snapshot
        .entries
        .iter()
        .map(|entry| entry.request_doc_id.clone())
        .collect::<Vec<_>>();
    let cancelled = submit_edit(
        &db,
        "edit-session",
        PendingQueueEdit {
            expected_request_doc_ids: ids.clone(),
            selected_request_doc_ids: ids.clone(),
            messages: Vec::new(),
        },
    )
    .await
    .unwrap();
    assert!(cancelled.request_doc_ids.is_empty());
    assert!(pending_user_queue(&access, did, "edit-session", did)
        .await
        .unwrap()
        .entries
        .is_empty());
    for id in ids {
        let cancelled = row(&db, &id).await;
        assert_eq!(cancelled["lifecycle_state"], "interrupted");
        assert_eq!(
            cancelled["terminal_output"],
            serde_json::to_value(gents_protocol::output::TerminalOutput::NoMessage).unwrap()
        );
    }
}

#[tokio::test]
async fn stale_queue_snapshot_rejects_edit_without_mutating_signed_requests() {
    let db = test_db("stale-queue-management").await;
    let did = db.node_identity.did();
    let access = ConfigAccess::local(db.node.clone());
    let first = enqueue(
        &db,
        "general",
        "stale-session",
        "stale-first",
        "unchanged first",
        QueueDelivery::Queue,
    )
    .await;
    let before = row(&db, &first).await;
    let second = enqueue(
        &db,
        "general",
        "stale-session",
        "stale-second",
        "concurrent append",
        QueueDelivery::Queue,
    )
    .await;
    let error = submit_edit(
        &db,
        "stale-session",
        PendingQueueEdit {
            expected_request_doc_ids: vec![first.clone()],
            selected_request_doc_ids: vec![first.clone()],
            messages: vec![PendingMessageEdit {
                authored_input: None,
                request_doc_id: first.clone(),
                content: "must never appear".into(),
            }],
        },
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("queue changed"));
    assert_eq!(row(&db, &first).await, before);
    let pending = pending_user_queue(&access, did, "stale-session", did)
        .await
        .unwrap();
    assert_eq!(
        pending
            .entries
            .iter()
            .map(|entry| entry.request_doc_id.as_str())
            .collect::<Vec<_>>(),
        vec![first.as_str(), second.as_str()]
    );
    assert!(pending
        .entries
        .iter()
        .all(|entry| entry.content != "must never appear"));
}

#[tokio::test]
async fn invalid_edited_attachment_does_not_supersede_the_original() {
    let db = test_db("invalid-edited-attachment").await;
    let original = enqueue(
        &db,
        "general",
        "invalid-edit-session",
        "invalid-edit-original",
        "original message",
        QueueDelivery::Queue,
    )
    .await;
    let before = row(&db, &original).await;
    let result = submit_edit(
        &db,
        "invalid-edit-session",
        PendingQueueEdit {
            expected_request_doc_ids: vec![original.clone()],
            selected_request_doc_ids: vec![original.clone()],
            messages: vec![PendingMessageEdit {
                request_doc_id: original.clone(),
                content: "replacement".into(),
                authored_input: Some(gents_protocol::request_input::AuthoredRequestInput {
                    attachments: vec![gents_protocol::request_input::RequestAttachment {
                        name: "image.png".into(),
                        media_type: "image/png".into(),
                        data: "not base64".into(),
                    }],
                    ..Default::default()
                }),
            }],
        },
    )
    .await;
    assert!(result.is_err());
    assert_eq!(row(&db, &original).await, before);
}

#[tokio::test]
async fn edited_skill_outside_context_does_not_supersede_the_original() {
    let db = test_db("invalid-edited-skill").await;
    let did = db.node_identity.did();
    crate::support::create_agent(&db.node, "general", did).await;
    let original = enqueue(
        &db,
        "general",
        "invalid-skill-session",
        "original",
        "original message",
        QueueDelivery::Queue,
    )
    .await;
    let before = row(&db, &original).await;
    let error = submit_edit(
        &db,
        "invalid-skill-session",
        PendingQueueEdit {
            expected_request_doc_ids: vec![original.clone()],
            selected_request_doc_ids: vec![original.clone()],
            messages: vec![PendingMessageEdit {
                request_doc_id: original.clone(),
                content: "changed".into(),
                authored_input: Some(gents_protocol::request_input::AuthoredRequestInput {
                    selected_skill_ids: vec!["unavailable".into()],
                    ..Default::default()
                }),
            }],
        },
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("skill"));
    assert_eq!(row(&db, &original).await, before);
}

#[tokio::test]
async fn authored_edit_keeps_queue_slot_and_retains_new_attachment_bytes() {
    let db = test_db("authored-queue-edit").await;
    let did = db.node_identity.did();
    crate::support::create_agent(&db.node, "general", did).await;
    let original = enqueue(
        &db,
        "general",
        "authored-edit-session",
        "authored-original",
        "original",
        QueueDelivery::Queue,
    )
    .await;
    let attachment = gents_protocol::request_input::RequestAttachment {
        name: "note.txt".into(),
        media_type: "text/plain".into(),
        data: "aGVsbG8=".into(),
    };
    let receipt = submit_edit(
        &db,
        "authored-edit-session",
        PendingQueueEdit {
            expected_request_doc_ids: vec![original.clone()],
            selected_request_doc_ids: vec![original.clone()],
            messages: vec![PendingMessageEdit {
                request_doc_id: original.clone(),
                content: "changed".into(),
                authored_input: Some(gents_protocol::request_input::AuthoredRequestInput {
                    attachments: vec![attachment.clone()],
                    ..Default::default()
                }),
            }],
        },
    )
    .await
    .unwrap();
    assert_eq!(receipt.request_doc_ids.len(), 1);
    assert_eq!(row(&db, &original).await["lifecycle_state"], "superseded");
    let replacement = row(&db, &receipt.request_doc_ids[0]).await;
    let input: gents_protocol::request_input::RequestInput =
        serde_json::from_value(replacement["input"].clone()).unwrap();
    assert_eq!(input.attachments, vec![attachment]);
    assert_eq!(
        input.queue.unwrap().position.unwrap().slot_request_doc_id,
        original
    );
    let pending = pending_user_queue(
        &ConfigAccess::local(db.node.clone()),
        did,
        "authored-edit-session",
        did,
    )
    .await
    .unwrap();
    assert_eq!(
        pending.entries[0].request_doc_id,
        receipt.request_doc_ids[0]
    );
}

#[tokio::test]
async fn authored_edit_changes_allowed_skills_without_losing_queue_position() {
    let db = test_db("allowed-edited-skill").await;
    let did = db.node_identity.did();
    crate::support::create_agent(&db.node, "general", did).await;
    let access = ConfigAccess::local(db.node.clone());
    let owner = escape_graphql_string(did);
    access.write("test.skill_context", &format!(r#"mutation {{ create_AgentContext(input: {{ node_did: "{owner}", context_id: "general:context", skill_ids: ["review"] }}) {{ _docID }} }}"#)).await.unwrap();
    access.write("test.attach_context", &format!(r#"mutation {{ update_Agent(filter: {{ node_did: {{ _eq: "{owner}" }}, agent_id: {{ _eq: "general" }} }}, input: {{ context_id: "general:context" }}) {{ _docID }} }}"#)).await.unwrap();
    let original = enqueue(
        &db,
        "general",
        "allowed-skill-session",
        "original",
        "original message",
        QueueDelivery::Queue,
    )
    .await;
    let receipt = submit_edit(
        &db,
        "allowed-skill-session",
        PendingQueueEdit {
            expected_request_doc_ids: vec![original.clone()],
            selected_request_doc_ids: vec![original.clone()],
            messages: vec![PendingMessageEdit {
                request_doc_id: original.clone(),
                content: "changed".into(),
                authored_input: Some(gents_protocol::request_input::AuthoredRequestInput {
                    selected_skill_ids: vec!["review".into()],
                    ..Default::default()
                }),
            }],
        },
    )
    .await
    .unwrap();
    let pending = pending_user_queue(&access, did, "allowed-skill-session", did)
        .await
        .unwrap();
    assert_eq!(pending.entries.len(), 1);
    assert_eq!(
        pending.entries[0].request_doc_id,
        receipt.request_doc_ids[0]
    );
    assert_eq!(pending.entries[0].selected_skill_ids, ["review"]);
    let replacement = row(&db, &receipt.request_doc_ids[0]).await;
    let input: gents_protocol::request_input::RequestInput =
        serde_json::from_value(replacement["input"].clone()).unwrap();
    assert_eq!(
        input.queue.unwrap().position.unwrap().slot_request_doc_id,
        original
    );
}

#[tokio::test]
async fn signed_pending_replacement_authenticates_delivery_and_each_position_field() {
    let db = test_db("queue-signature-tampering").await;
    let _did = db.node_identity.did();
    let access = ConfigAccess::local(db.node.clone());
    let original = enqueue(
        &db,
        "general",
        "signature-session",
        "signature-original",
        "original signed message",
        QueueDelivery::Steer,
    )
    .await;
    let receipt = submit_edit(
        &db,
        "signature-session",
        PendingQueueEdit {
            expected_request_doc_ids: vec![original.clone()],
            selected_request_doc_ids: vec![original.clone()],
            messages: vec![PendingMessageEdit {
                authored_input: None,
                request_doc_id: original.clone(),
                content: "replacement signed message".into(),
            }],
        },
    )
    .await
    .unwrap();
    assert_eq!(receipt.request_doc_ids.len(), 1);
    let response = access
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
            escape_graphql_string(&receipt.request_doc_ids[0]),
            gents::SIGNED_REQUEST_FIELDS,
        ))
        .await
        .unwrap();
    let rows: Vec<gents_protocol::row::AgentRequestRow> =
        serde_json::from_value(response["data"]["AgentRequest"].clone()).unwrap();
    assert_eq!(rows.len(), 1);
    let signed = &rows[0];
    gents::verify_request_receipt_signature(signed)
        .expect("the actual replacement signed by the node must authenticate");
    let queue = signed.input.as_ref().unwrap().queue.as_ref().unwrap();
    assert_eq!(queue.delivery, QueueDelivery::Steer);
    let position = queue.position.as_ref().unwrap();
    assert_eq!(position.slot_request_doc_id, original);
    assert_eq!(position.replaces_request_doc_id, original);

    let mut changed_delivery = signed.clone();
    changed_delivery
        .input
        .as_mut()
        .unwrap()
        .queue
        .as_mut()
        .unwrap()
        .delivery = QueueDelivery::Queue;
    let mut changed_slot = signed.clone();
    changed_slot
        .input
        .as_mut()
        .unwrap()
        .queue
        .as_mut()
        .unwrap()
        .position
        .as_mut()
        .unwrap()
        .slot_request_doc_id = "another-slot".into();
    let mut changed_predecessor = signed.clone();
    changed_predecessor
        .input
        .as_mut()
        .unwrap()
        .queue
        .as_mut()
        .unwrap()
        .position
        .as_mut()
        .unwrap()
        .replaces_request_doc_id = "another-predecessor".into();
    for (field, tampered) in [
        ("delivery", changed_delivery),
        ("slot_request_doc_id", changed_slot),
        ("replaces_request_doc_id", changed_predecessor),
    ] {
        let error = gents::verify_request_receipt_signature(&tampered).expect_err(field);
        assert!(
            format!("{error:#}").contains("signature"),
            "{field}: {error:#}"
        );
    }
    gents::verify_request_receipt_signature(signed)
        .expect("tampering clones must not change the persisted signed replacement");
}

#[tokio::test]
async fn explicit_steering_prioritizes_one_message_without_losing_other_queued_input() {
    let db = test_db("explicit-pending-steer").await;
    let did = db.node_identity.did();
    let session = "explicit-pending-steer-session";
    let mut originals = Vec::new();
    for (id, content) in [
        ("first", "Keep first queued"),
        ("second", "Keep second queued"),
        ("selected", "Steer this message"),
    ] {
        originals.push(enqueue(&db, "general", session, id, content, QueueDelivery::Queue).await);
    }
    let access = ConfigAccess::local(db.node.clone());
    let snapshot = pending_user_queue(&access, did, session, did)
        .await
        .unwrap();
    let target = originals[2].clone();
    let edit = pending_steer_edit(&snapshot, originals.clone(), &target).unwrap();
    let original_rows = futures::future::join_all(originals.iter().map(|id| row(&db, id))).await;
    let receipt = submit_edit_inner(&db, session, edit.clone(), Some(&target))
        .await
        .unwrap();
    let pending = pending_user_queue(&access, did, session, did)
        .await
        .unwrap();
    assert_eq!(
        pending
            .entries
            .iter()
            .map(|entry| entry.content.as_str())
            .collect::<Vec<_>>(),
        [
            "Steer this message",
            "Keep first queued",
            "Keep second queued"
        ]
    );
    assert_eq!(
        pending
            .entries
            .iter()
            .map(|entry| entry.steerable)
            .collect::<Vec<_>>(),
        [false, true, true]
    );
    assert_eq!(
        pending
            .entries
            .iter()
            .map(|entry| &entry.request_doc_id)
            .collect::<Vec<_>>(),
        receipt.request_doc_ids.iter().collect::<Vec<_>>()
    );
    for (id, original) in originals.iter().zip(&original_rows) {
        let current = row(&db, id).await;
        assert_eq!(current["lifecycle_state"], "superseded");
        for key in ["content", "input", "admission_signature"] {
            assert_eq!(current[key], original[key]);
        }
    }
    assert!(submit_edit_inner(&db, session, edit, Some(&target))
        .await
        .is_err());
    let after_retry = pending_user_queue(&access, did, session, did)
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(after_retry).unwrap(),
        serde_json::to_value(pending).unwrap()
    );
}
