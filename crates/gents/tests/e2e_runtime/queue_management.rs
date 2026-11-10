use gents::request::lifecycle::{
    pending_steer_edit, pending_user_queue, submit_pending_user_edit, submit_pending_user_steer,
    PendingMessageEdit, PendingQueueEdit,
};
use gents::runtime::agent::p2p_reconcile::session_commands::run_session_command_reconciler;
use gents::store::graphql::escape_graphql_string;
use gents::NodeIdentity;
use gents_protocol::request_admission::{AgentRequestAdmissionRecord, AgentRequestCreate};
use gents_protocol::request_input::QueueDelivery;
use gents_protocol::session_input_edit::SessionInputEditReceipt;

use crate::support::{self, test_db, TestDb};

pub(super) async fn insert_signed_as(
    fixture: &support::ManagedAgentFixture,
    mut request: AgentRequestCreate,
) -> String {
    request.session_doc_id = Some(
        gents::request::lifecycle::reserve_goal_session(&fixture.access, &request)
            .await
            .unwrap(),
    );
    request.agent_doc_id = Some(
        fixture
            .access
            .transact_readonly("test.queue.selected_agent", |txn| {
                Box::pin(async {
                    Ok::<_, anyhow::Error>(
                        gents::store::config_client::read_desired_state_record_in_txn(
                            txn,
                            gents::Collection::Agent,
                            &request.node_did,
                            &request.agent_did,
                        )
                        .await?
                        .expect("selected Agent")
                        .0,
                    )
                })
            })
            .await
            .unwrap(),
    );
    gents::sign_agent_request_create(&fixture.user, &mut request)
        .await
        .unwrap();
    gents::request::lifecycle::submit_client_request(&fixture.access, &request)
        .await
        .unwrap();
    let response = fixture.access.execute(&format!(
        r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}, limit: 1) {{ _docID }} }}"#,
        escape_graphql_string(&request.request_id),
    )).await.unwrap();
    response["data"]["AgentRequest"][0]["_docID"]
        .as_str()
        .unwrap()
        .into()
}

pub(super) async fn enqueue_owned(
    db: &TestDb,
    fixture: &support::ManagedAgentFixture,
    session: &str,
    id: &str,
    text: &str,
    delivery: QueueDelivery,
) -> String {
    support::agent_requests::submit_user_request(
        db.node_identity.did(),
        fixture,
        session,
        id,
        text,
        delivery,
    )
    .await
}

async fn submit_edit(
    db: &TestDb,
    fixture: &support::ManagedAgentFixture,
    session_id: &str,
    edit: PendingQueueEdit,
) -> anyhow::Result<SessionInputEditReceipt> {
    submit_edit_inner(db, fixture, session_id, edit, None).await
}

async fn submit_edit_inner(
    db: &TestDb,
    fixture: &support::ManagedAgentFixture,
    session_id: &str,
    edit: PendingQueueEdit,
    steer: Option<&str>,
) -> anyhow::Result<SessionInputEditReceipt> {
    let did = db.node_identity.did();
    let existing = fixture
        .access
        .execute(&format!(
            r#"{{ AgentSession(filter: {{ session_id: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
            escape_graphql_string(session_id)
        ))
        .await?;
    anyhow::ensure!(
        existing["data"]["AgentSession"]
            .as_array()
            .is_some_and(|rows| rows.len() == 1),
        "queue edit requires its existing physical session"
    );
    let cancel = tokio_util::sync::CancellationToken::new();
    let worker = tokio::spawn(run_session_command_reconciler(
        db.node.clone(),
        db.node_identity.clone(),
        support::fixtures::session_command_agent_snapshot(db).await,
        cancel.clone(),
    ));
    let access = &fixture.access;
    let result = match steer {
        Some(selected) => {
            submit_pending_user_steer(
                access,
                &fixture.user,
                AgentRequestAdmissionRecord::client(fixture.user.did()),
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
                access,
                &fixture.user,
                AgentRequestAdmissionRecord::client(fixture.user.did()),
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

pub(super) async fn row_as(fixture: &support::ManagedAgentFixture, doc: &str) -> serde_json::Value {
    let response = fixture.access.execute(&format!(
        r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ _docID request_id content input lifecycle_state failure_reason admission_signature superseded_by_request_doc_id terminal_output }} }}"#,
        escape_graphql_string(doc),
    )).await.unwrap();
    let rows = response["data"]["AgentRequest"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    rows[0].clone()
}

async fn install_review_skill(db: &TestDb, fixture: &support::ManagedAgentFixture) {
    use gents::documents::apply_desired_state_plan;
    use gents::store::config_client::{
        read_desired_state_record_in_txn, DesiredStateApplyDocument, DesiredStateApplyPlan,
    };
    let owner = db.node_identity.did();
    let selected = fixture.binding.agent_did.as_str();
    fixture.access.transact_write("test.queue_review_skill", |txn| {
        Box::pin(async move {
            let (_, mut agent) = read_desired_state_record_in_txn(
                txn, gents::Collection::Agent, owner, selected,
            ).await?.expect("selected Agent");
            agent["context_id"] = "test-context".into();
            let documents = [
                (gents::Collection::Skill, serde_json::json!({
                    "node_did": owner, "skill_id": "review", "name": "review",
                    "description": "test", "instructions": "review", "enabled": true,
                })),
                (gents::Collection::AgentContext, serde_json::json!({
                    "node_did": owner, "context_id": "test-context", "skill_ids": ["review"],
                })),
                (gents::Collection::Agent, agent),
            ];
            let plan = DesiredStateApplyPlan::new(documents.into_iter().map(|(collection, value)| {
                DesiredStateApplyDocument { collection, add: value.clone(), update: value }
            }).collect())?;
            apply_desired_state_plan(txn, &plan).await
        })
    }).await.expect("install User-authored review skill");
}

#[tokio::test]
async fn signed_queue_edit_reorders_preserves_slots_and_cancels_atomically() {
    let db = test_db("signed-queue-management").await;
    let fixture = support::managed_agent_fixture(&db).await;
    let did = db.node_identity.did();
    let access = fixture.access.clone();
    let first = enqueue_owned(
        &db,
        &fixture,
        "edit-session",
        "edit-first",
        "first original",
        QueueDelivery::Steer,
    )
    .await;
    let second = enqueue_owned(
        &db,
        &fixture,
        "edit-session",
        "edit-second",
        "second original",
        QueueDelivery::Steer,
    )
    .await;
    let original_first = row_as(&fixture, &first).await;
    let original_second = row_as(&fixture, &second).await;
    let snapshot = pending_user_queue(&access, did, "edit-session", fixture.user.did())
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
        &fixture,
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
        let old_after = row_as(&fixture, old_id).await;
        assert_eq!(old_after["content"], old["content"]);
        assert_eq!(old_after["admission_signature"], old["admission_signature"]);
        assert_eq!(old_after["lifecycle_state"], "superseded");
        assert_eq!(
            old_after["superseded_by_request_doc_id"],
            receipt.request_doc_ids[index]
        );
        let replacement = row_as(&fixture, &receipt.request_doc_ids[index]).await;
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
    let snapshot = pending_user_queue(&access, did, "edit-session", fixture.user.did())
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
        &fixture,
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
    assert!(
        pending_user_queue(&access, did, "edit-session", fixture.user.did())
            .await
            .unwrap()
            .entries
            .is_empty()
    );
    for id in ids {
        let cancelled = row_as(&fixture, &id).await;
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
    let fixture = support::managed_agent_fixture(&db).await;
    let did = db.node_identity.did();
    let access = fixture.access.clone();
    let first = enqueue_owned(
        &db,
        &fixture,
        "stale-session",
        "stale-first",
        "unchanged first",
        QueueDelivery::Queue,
    )
    .await;
    let before = row_as(&fixture, &first).await;
    let second = enqueue_owned(
        &db,
        &fixture,
        "stale-session",
        "stale-second",
        "concurrent append",
        QueueDelivery::Queue,
    )
    .await;
    let error = submit_edit(
        &db,
        &fixture,
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
    assert_eq!(row_as(&fixture, &first).await, before);
    let pending = pending_user_queue(&access, did, "stale-session", fixture.user.did())
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
    let fixture = support::managed_agent_fixture(&db).await;
    let original = enqueue_owned(
        &db,
        &fixture,
        "invalid-edit-session",
        "invalid-edit-original",
        "original message",
        QueueDelivery::Queue,
    )
    .await;
    let before = row_as(&fixture, &original).await;
    let result = submit_edit(
        &db,
        &fixture,
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
    assert_eq!(row_as(&fixture, &original).await, before);
}

#[tokio::test]
async fn edited_skill_outside_context_does_not_supersede_the_original() {
    let db = test_db("invalid-edited-skill").await;
    let fixture = support::managed_agent_fixture(&db).await;
    let original = enqueue_owned(
        &db,
        &fixture,
        "invalid-skill-session",
        "original",
        "original message",
        QueueDelivery::Queue,
    )
    .await;
    let before = row_as(&fixture, &original).await;
    let error = submit_edit(
        &db,
        &fixture,
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
    assert_eq!(row_as(&fixture, &original).await, before);
}

#[tokio::test]
async fn authored_edit_keeps_queue_slot_and_retains_new_attachment_bytes() {
    let db = test_db("authored-queue-edit").await;
    let fixture = support::managed_agent_fixture(&db).await;
    let did = db.node_identity.did();
    let original = enqueue_owned(
        &db,
        &fixture,
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
        &fixture,
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
    assert_eq!(
        row_as(&fixture, &original).await["lifecycle_state"],
        "superseded"
    );
    let replacement = row_as(&fixture, &receipt.request_doc_ids[0]).await;
    let input: gents_protocol::request_input::RequestInput =
        serde_json::from_value(replacement["input"].clone()).unwrap();
    assert_eq!(input.attachments, vec![attachment]);
    assert_eq!(
        input.queue.unwrap().position.unwrap().slot_request_doc_id,
        original
    );
    let pending = pending_user_queue(
        &fixture.access,
        did,
        "authored-edit-session",
        fixture.user.did(),
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
    let fixture = support::managed_agent_fixture(&db).await;
    let did = db.node_identity.did();
    let access = fixture.access.clone();
    install_review_skill(&db, &fixture).await;
    let original = enqueue_owned(
        &db,
        &fixture,
        "allowed-skill-session",
        "original",
        "original message",
        QueueDelivery::Queue,
    )
    .await;
    let receipt = submit_edit(
        &db,
        &fixture,
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
    let pending = pending_user_queue(&access, did, "allowed-skill-session", fixture.user.did())
        .await
        .unwrap();
    assert_eq!(pending.entries.len(), 1);
    assert_eq!(
        pending.entries[0].request_doc_id,
        receipt.request_doc_ids[0]
    );
    assert_eq!(pending.entries[0].selected_skill_ids, ["review"]);
    let replacement = row_as(&fixture, &receipt.request_doc_ids[0]).await;
    let input: gents_protocol::request_input::RequestInput =
        serde_json::from_value(replacement["input"].clone()).unwrap();
    assert_eq!(
        input.queue.unwrap().position.unwrap().slot_request_doc_id,
        original
    );
}

#[tokio::test]
async fn queued_edit_accepts_skill_only_but_rejects_truly_empty_input() {
    let db = test_db("skill-only-queue-edit").await;
    let fixture = support::managed_agent_fixture(&db).await;
    let did = db.node_identity.did();
    let access = fixture.access.clone();
    install_review_skill(&db, &fixture).await;
    let original = enqueue_owned(
        &db,
        &fixture,
        "skill-only-session",
        "original",
        "original message",
        QueueDelivery::Queue,
    )
    .await;
    let edit = |skills: Vec<String>| PendingQueueEdit {
        expected_request_doc_ids: vec![original.clone()],
        selected_request_doc_ids: vec![original.clone()],
        messages: vec![PendingMessageEdit {
            request_doc_id: original.clone(),
            content: String::new(),
            authored_input: Some(gents_protocol::request_input::AuthoredRequestInput {
                selected_skill_ids: skills,
                ..Default::default()
            }),
        }],
    };
    let error = submit_edit(&db, &fixture, "skill-only-session", edit(vec![]))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("A message cannot be empty"));
    let receipt = submit_edit(
        &db,
        &fixture,
        "skill-only-session",
        edit(vec!["review".into()]),
    )
    .await
    .unwrap();
    let pending = pending_user_queue(&access, did, "skill-only-session", fixture.user.did())
        .await
        .unwrap();
    assert_eq!(pending.entries.len(), 1);
    assert_eq!(
        pending.entries[0].request_doc_id,
        receipt.request_doc_ids[0]
    );
    assert_eq!(pending.entries[0].content, "");
    assert_eq!(pending.entries[0].selected_skill_ids, ["review"]);
    let replacement = row_as(&fixture, &receipt.request_doc_ids[0]).await;
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
    let fixture = support::managed_agent_fixture(&db).await;
    let _did = db.node_identity.did();
    let access = fixture.access.clone();
    let original = enqueue_owned(
        &db,
        &fixture,
        "signature-session",
        "signature-original",
        "original signed message",
        QueueDelivery::Steer,
    )
    .await;
    let receipt = submit_edit(
        &db,
        &fixture,
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
        .expect("the actual replacement signed by the user must authenticate");
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
        let chain = gents_protocol::error_chain::ErrorChain(&error).to_string();
        assert!(chain.contains("signature"), "{field}: {chain}");
    }
    gents::verify_request_receipt_signature(signed)
        .expect("tampering clones must not change the persisted signed replacement");
}

#[tokio::test]
async fn explicit_steering_prioritizes_one_message_without_losing_other_queued_input() {
    let db = test_db("explicit-pending-steer").await;
    let fixture = support::managed_agent_fixture(&db).await;
    let did = db.node_identity.did();
    let session = "explicit-pending-steer-session";
    let mut originals = Vec::new();
    for (id, content) in [
        ("first", "Keep first queued"),
        ("second", "Keep second queued"),
        ("selected", "Steer this message"),
    ] {
        originals
            .push(enqueue_owned(&db, &fixture, session, id, content, QueueDelivery::Queue).await);
    }
    let access = fixture.access.clone();
    let snapshot = pending_user_queue(&access, did, session, fixture.user.did())
        .await
        .unwrap();
    let target = originals[2].clone();
    let edit = pending_steer_edit(&snapshot, originals.clone(), &target).unwrap();
    let original_rows =
        futures::future::join_all(originals.iter().map(|id| row_as(&fixture, id))).await;
    let receipt = submit_edit_inner(&db, &fixture, session, edit.clone(), Some(&target))
        .await
        .unwrap();
    let pending = pending_user_queue(&access, did, session, fixture.user.did())
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
        let current = row_as(&fixture, id).await;
        assert_eq!(current["lifecycle_state"], "superseded");
        for key in ["content", "input", "admission_signature"] {
            assert_eq!(current[key], original[key]);
        }
    }
    assert!(
        submit_edit_inner(&db, &fixture, session, edit, Some(&target))
            .await
            .is_err()
    );
    let after_retry = pending_user_queue(&access, did, session, fixture.user.did())
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(after_retry).unwrap(),
        serde_json::to_value(pending).unwrap()
    );
}
