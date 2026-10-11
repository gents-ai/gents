use gents::config_client::ConfigAccess;
use gents::graphql::escape_graphql_string;
use gents::lifecycle::{
    pending_user_queue, replace_pending_user_messages, PendingMessageEdit, PendingQueueEdit,
};
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use gents_protocol::request_input::QueueDelivery;

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
        &ConfigAccess::Local(db.node.clone()),
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
    let response = ConfigAccess::Local(db.node.clone())
        .write(
            "test.queue_signed_input",
            &request.graphql_mutation().unwrap(),
        )
        .await
        .unwrap();
    gents::graphql::created_doc_id(&response, "AgentRequest").unwrap()
}

pub(super) async fn row(db: &TestDb, doc: &str) -> serde_json::Value {
    let response = ConfigAccess::Local(db.node.clone()).execute(&format!(r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ _docID request_id content input lifecycle_state failure_reason admission_signature superseded_by_request_doc_id terminal_output }} }}"#, escape_graphql_string(doc))).await.unwrap();
    let rows = response["data"]["AgentRequest"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    rows[0].clone()
}

#[tokio::test]
async fn signed_queue_edit_reorders_preserves_slots_and_cancels_atomically() {
    let db = test_db("signed-queue-management").await;
    let did = db.node_identity.did();
    let access = ConfigAccess::Local(db.node.clone());
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
    let receipt = replace_pending_user_messages(
        &access,
        db.node_identity.as_ref(),
        AgentRequestAdmissionRecord::local_self(did),
        did,
        "edit-session",
        did,
        PendingQueueEdit {
            expected_request_doc_ids: vec![first.clone(), second.clone()],
            selected_request_doc_ids: vec![first.clone(), second.clone()],
            messages: vec![
                PendingMessageEdit {
                    request_doc_id: second.clone(),
                    content: "second edited".into(),
                },
                PendingMessageEdit {
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
    let cancelled = replace_pending_user_messages(
        &access,
        db.node_identity.as_ref(),
        AgentRequestAdmissionRecord::local_self(did),
        did,
        "edit-session",
        did,
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
    let access = ConfigAccess::Local(db.node.clone());
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
    let error = replace_pending_user_messages(
        &access,
        db.node_identity.as_ref(),
        AgentRequestAdmissionRecord::local_self(did),
        did,
        "stale-session",
        did,
        PendingQueueEdit {
            expected_request_doc_ids: vec![first.clone()],
            selected_request_doc_ids: vec![first.clone()],
            messages: vec![PendingMessageEdit {
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
async fn signed_pending_replacement_authenticates_delivery_and_each_position_field() {
    let db = test_db("queue-signature-tampering").await;
    let did = db.node_identity.did();
    let access = ConfigAccess::Local(db.node.clone());
    let original = enqueue(
        &db,
        "general",
        "signature-session",
        "signature-original",
        "original signed message",
        QueueDelivery::Steer,
    )
    .await;
    let receipt = replace_pending_user_messages(
        &access,
        db.node_identity.as_ref(),
        AgentRequestAdmissionRecord::local_self(did),
        did,
        "signature-session",
        did,
        PendingQueueEdit {
            expected_request_doc_ids: vec![original.clone()],
            selected_request_doc_ids: vec![original.clone()],
            messages: vec![PendingMessageEdit {
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
