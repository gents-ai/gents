use std::time::{Duration, Instant};

use gents::agent::p2p_reconcile::session_input_edit::run_session_input_edit_reconciler;
use gents::agent::p2p_reconcile::{
    enrollment_authority_channel, resolve_template, resolve_template_filters,
    run_enrollment_reconciler, run_pairing_reconciler, EmbeddedRemoteP2pAdmin, PairingDirection,
    RemoteP2pAdmin, CLIENT_TEMPLATE, CLIENT_TO_RUNTIME_COLLECTIONS,
};
use gents::config_client::ConfigAccess;
use gents::graphql::escape_graphql_string;
use gents::lifecycle::{prepare_pending_user_edit, PendingMessageEdit, PendingQueueEdit};
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use gents_protocol::request_input::{QueueDelivery, QueuePolicy, QueueSource, RequestQueue};
use gents_protocol::session_input_edit::{
    SessionInputEdit, SessionInputEditOutcome, SessionInputEditReceipt, SESSION_INPUT_EDIT_VERSION,
};
use tokio_util::sync::CancellationToken;

use crate::support::{self, TestDb};

struct Stop(CancellationToken);
impl Drop for Stop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

async fn rows(db: &TestDb, collection: &str, filter: &str, fields: &str) -> Vec<serde_json::Value> {
    ConfigAccess::Local(db.node.clone())
        .execute(&format!(
            "{{ {collection}(filter: {filter}) {{ {fields} }} }}"
        ))
        .await
        .unwrap()["data"][collection]
        .as_array()
        .unwrap()
        .clone()
}

async fn wait_row(
    db: &TestDb,
    collection: &str,
    field: &str,
    value: &str,
    fields: &str,
    required: &str,
) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let found = rows(
            db,
            collection,
            &format!(
                "{{ {field}: {{ _eq: \"{}\" }} }}",
                escape_graphql_string(value)
            ),
            fields,
        )
        .await;
        if let Some(row) = found
            .into_iter()
            .find(|row| required.is_empty() || !row[required].is_null())
        {
            return row;
        }
        assert!(
            Instant::now() < deadline,
            "{collection}.{field}={value} did not replicate with {required}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn write_command(db: &TestDb, command: &SessionInputEdit) -> String {
    let peer = command
        .requester_peer_id
        .as_ref()
        .map(|p| format!("\"{}\"", escape_graphql_string(p)))
        .unwrap_or("null".into());
    let response = ConfigAccess::Local(db.node.clone()).write("test.signed_remote_edit", &format!(r#"mutation {{ create_AgentSessionInputEdit(input: {{
        command_id: "{}", requester_did: "{}", requester_peer_id: {}, node_did: "{}", session_id: "{}", intent_json: "{}"
    }}) {{ _docID }} }}"#, escape_graphql_string(&command.command_id), escape_graphql_string(&command.requester_did), peer,
        escape_graphql_string(&command.node_did), escape_graphql_string(&command.session_id),
        escape_graphql_string(&serde_json::to_string(command).unwrap()))).await.unwrap();
    gents::graphql::created_doc_id(&response, "AgentSessionInputEdit").unwrap()
}

#[tokio::test]
async fn generated_remote_edits_replicate_signed_effect_and_receipt() {
    let runtime = support::test_p2p_db("input-edit-runtime").await;
    let client = support::test_p2p_db("input-edit-client").await;
    let did = runtime.node_identity.did();
    let requester = client.node_identity.did();
    gents::document_config::ensure_node(&runtime.node, did)
        .await
        .unwrap();
    gents::document_config::ensure_node(&client.node, requester)
        .await
        .unwrap();
    let (peer, address) = support::enrollment::wait_for_peer_identity(&client.node).await;
    let cancel = CancellationToken::new();
    let _stop = Stop(cancel.clone());
    let (owner, authority) = enrollment_authority_channel();
    let enrollment_task = tokio::spawn(run_enrollment_reconciler(
        runtime.node.clone(),
        runtime.node_identity.clone(),
        owner,
        cancel.clone(),
    ));
    let pairing_task = tokio::spawn(run_pairing_reconciler(
        runtime.node.clone(),
        runtime.node_identity.clone(),
        authority.clone(),
        cancel.clone(),
    ));
    let enrollment = support::enrollment::authorize_enrollment_peer(
        runtime.node.clone(),
        "edit-network",
        "Edit network",
        runtime.node_identity.clone(),
        client.node_identity.clone(),
        &peer,
        &address,
    )
    .await;
    let fence = authority
        .fresh_authorization(requester, &peer)
        .await
        .unwrap()
        .unwrap();
    let admission = AgentRequestAdmissionRecord::enrollment(
        requester,
        &fence.request_id,
        &fence.request_digest,
        &fence.admin_did,
        fence.authorization_sequence,
        &fence.authorization_expires_at,
    );
    // The applied route is produced by the enrollment and pairing owners, never a test-authored grant.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let applied = rows(
            &runtime,
            "PeerPairingApplied",
            &format!(
                "{{ peer_id: {{ _eq: \"{}\" }} }}",
                escape_graphql_string(&peer)
            ),
            "replicator_filter",
        )
        .await;
        if applied.iter().any(|row| {
            row["replicator_filter"]
                .as_str()
                .is_some_and(|v| v.contains("AgentSessionInputEdit"))
        }) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "signed enrollment did not apply the edit replication route"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let (_, runtime_address) = support::enrollment::wait_for_peer_identity(&runtime.node).await;
    let client_admin = EmbeddedRemoteP2pAdmin::new(client.node.clone());
    let runtime_admin = EmbeddedRemoteP2pAdmin::new(runtime.node.clone());
    runtime_admin
        .add_p2p_collections(&["AgentSessionInputEdit".into()])
        .await
        .unwrap();
    client_admin
        .connect(std::slice::from_ref(&runtime_address))
        .await
        .unwrap();
    let outbound = resolve_template_filters(
        resolve_template(CLIENT_TEMPLATE).unwrap(),
        PairingDirection::ClientToRuntime,
        requester,
        did,
    );
    client_admin
        .add_replicator(
            &[runtime_address],
            &CLIENT_TO_RUNTIME_COLLECTIONS
                .iter()
                .map(|name| (*name).to_owned())
                .collect::<Vec<_>>(),
            &outbound,
        )
        .await
        .unwrap();
    let contract: serde_json::Value = gents_lean_contract::load_contract_snapshot().unwrap();
    let cases = contract["session_input_edit_cases"].as_array().unwrap();
    let mut prepared = Vec::new();
    for name in [
        "remote_applied",
        "expired_rejected",
        "stale_queue_rejected",
        "signature_rejected",
        "replacement_signature_rejected",
    ] {
        let case = cases.iter().find(|case| case["name"] == name).unwrap();
        let session_id = format!("remote-{name}");
        let mut session = support::session_document_in_scope(
            did,
            &session_id,
            "general",
            &chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        );
        session.requester_did = Some(requester.into());
        support::create_session_document(&runtime.node, &session).await;
        let mut original = AgentRequestCreate::base(
            RequestPurpose::Normal,
            format!("original-{name}"),
            did,
            requester,
            "general",
            &session_id,
            "original pending message",
            "interactive",
            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            admission.clone(),
        );
        original.input.queue = Some(RequestQueue {
            source: QueueSource::User,
            policy: QueuePolicy::Append,
            delivery: QueueDelivery::Steer,
            position: None,
            key: None,
            queued_after_request_id: None,
            interrupted_request_id: None,
            background_completion_wake_version: None,
        });
        gents::sign_agent_request_create(client.node_identity.as_ref(), &mut original)
            .await
            .unwrap();
        let written = ConfigAccess::Local(runtime.node.clone())
            .write("test.remote_pending", &original.graphql_mutation().unwrap())
            .await
            .unwrap();
        let original_id = gents::graphql::created_doc_id(&written, "AgentRequest").unwrap();
        wait_row(
            &client,
            "AgentRequest",
            "request_id",
            &original.request_id,
            "_docID",
            "",
        )
        .await;
        let edit = PendingQueueEdit {
            expected_request_doc_ids: vec![original_id.clone()],
            selected_request_doc_ids: vec![original_id.clone()],
            messages: vec![PendingMessageEdit {
                request_doc_id: original_id.clone(),
                content: "edited remote message".into(),
            }],
        };
        let replacements = prepare_pending_user_edit(
            &ConfigAccess::Local(client.node.clone()),
            client.node_identity.as_ref(),
            admission.clone(),
            did,
            &session_id,
            requester,
            &edit,
        )
        .await
        .unwrap();
        let now = chrono::Utc::now();
        let mut command = SessionInputEdit {
            version: SESSION_INPUT_EDIT_VERSION,
            command_id: format!("edit-{name}"),
            requester_did: requester.into(),
            requester_peer_id: Some(peer.clone()),
            node_did: did.into(),
            session_id,
            issued_at: (now - chrono::Duration::seconds(1)).to_rfc3339(),
            expires_at: (now + chrono::Duration::minutes(5)).to_rfc3339(),
            edit,
            replacements,
            signature: Vec::new(),
        };
        match name {
            "expired_rejected" => {
                command.issued_at = (now - chrono::Duration::minutes(2)).to_rfc3339();
                command.expires_at = (now - chrono::Duration::minutes(1)).to_rfc3339();
            }
            "stale_queue_rejected" => command.edit.expected_request_doc_ids.clear(),
            "replacement_signature_rejected" => command.replacements[0].admission.signature[0] ^= 1,
            _ => {}
        }
        command.signature = client
            .node_identity
            .sign(&command.signing_payload())
            .await
            .unwrap();
        if name == "signature_rejected" {
            command.signature[0] ^= 1;
        }
        write_command(&client, &command).await;
        wait_row(
            &runtime,
            "AgentSessionInputEdit",
            "command_id",
            &command.command_id,
            "_docID",
            "",
        )
        .await;
        prepared.push((
            command,
            original_id,
            case["receipt"]["outcome"].as_str().unwrap().to_owned(),
        ));
    }
    let edit_cancel = cancel.child_token();
    let edit_task = tokio::spawn(run_session_input_edit_reconciler(
        runtime.node.clone(),
        runtime.node_identity.clone(),
        edit_cancel.clone(),
    ));
    for (command, original_id, expected) in &prepared {
        let observed = wait_row(
            &client,
            "AgentSessionInputEdit",
            "command_id",
            &command.command_id,
            "_docID receipt_json",
            "receipt_json",
        )
        .await;
        let receipt: SessionInputEditReceipt =
            serde_json::from_str(observed["receipt_json"].as_str().unwrap()).unwrap();
        receipt.validate_for(command).unwrap();
        assert!(client
            .node_identity
            .verify(did, &receipt.signing_payload(), &receipt.signature)
            .await
            .unwrap());
        assert_eq!(
            serde_json::to_value(receipt.outcome).unwrap(),
            expected.as_str()
        );
        let old = wait_row(
            &runtime,
            "AgentRequest",
            "_docID",
            original_id,
            "content lifecycle_state admission_signature superseded_by_request_doc_id",
            "",
        )
        .await;
        assert_eq!(old["content"], "original pending message");
        if receipt.outcome == SessionInputEditOutcome::Applied {
            assert_eq!(old["lifecycle_state"], "superseded");
            assert_eq!(
                old["superseded_by_request_doc_id"],
                receipt.request_doc_ids[0]
            );
            let replacement = wait_row(
                &client,
                "AgentRequest",
                "request_id",
                &receipt.request_ids[0],
                "content",
                "",
            )
            .await;
            assert_eq!(replacement["content"], "edited remote message");
        } else {
            assert_eq!(old["lifecycle_state"], "pending");
        }
    }
    edit_cancel.cancel();
    edit_task.await.unwrap().unwrap();
    // The command crosses the live route before revocation. Application must
    // re-read the current signed enrollment instead of trusting transport arrival.
    let (template, original_id, _) = prepared
        .iter()
        .find(|(command, _, _)| command.command_id == "edit-expired_rejected")
        .unwrap();
    let mut revoked = template.clone();
    revoked.command_id = "edit-after-revocation".into();
    revoked.issued_at = (chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339();
    revoked.expires_at = (chrono::Utc::now() + chrono::Duration::minutes(5)).to_rfc3339();
    revoked.signature = client
        .node_identity
        .sign(&revoked.signing_payload())
        .await
        .unwrap();
    write_command(&client, &revoked).await;
    wait_row(
        &runtime,
        "AgentSessionInputEdit",
        "command_id",
        &revoked.command_id,
        "_docID",
        "",
    )
    .await;
    gents::agent::p2p_reconcile::GraphqlEnrollmentStore::new(
        runtime.node.clone(),
        runtime.node_identity.clone(),
    )
    .revoke_request(&enrollment.request_id)
    .await
    .unwrap();
    assert!(authority
        .fresh_authorization(requester, &peer)
        .await
        .unwrap()
        .is_none());
    let revoked_task = tokio::spawn(run_session_input_edit_reconciler(
        runtime.node.clone(),
        runtime.node_identity.clone(),
        cancel.clone(),
    ));
    let observed = wait_row(
        &runtime,
        "AgentSessionInputEdit",
        "command_id",
        &revoked.command_id,
        "receipt_json",
        "receipt_json",
    )
    .await;
    let receipt: SessionInputEditReceipt =
        serde_json::from_str(observed["receipt_json"].as_str().unwrap()).unwrap();
    receipt.validate_for(&revoked).unwrap();
    assert!(client
        .node_identity
        .verify(did, &receipt.signing_payload(), &receipt.signature)
        .await
        .unwrap());
    let model = cases
        .iter()
        .find(|case| case["name"] == "revoked_enrollment_rejected")
        .unwrap();
    assert_eq!(
        serde_json::to_value(receipt.outcome).unwrap(),
        model["receipt"]["outcome"]
    );
    assert_eq!(
        wait_row(
            &runtime,
            "AgentRequest",
            "_docID",
            original_id,
            "lifecycle_state",
            ""
        )
        .await["lifecycle_state"],
        "pending"
    );
    cancel.cancel();
    for task in [revoked_task, pairing_task, enrollment_task] {
        task.await.unwrap().unwrap();
    }
}

#[tokio::test]
async fn persisted_edit_survives_database_reopen_and_receipt_replay_is_idempotent() {
    let mut db = support::test_db("input-edit-reopen").await;
    let did = db.node_identity.did().to_owned();
    gents::document_config::ensure_node(&db.node, &did)
        .await
        .unwrap();
    let session = "edit-reopen-session";
    ConfigAccess::Local(db.node.clone()).write("test.edit_reopen.session", &format!(
        r#"mutation {{ create_AgentSession(input: {{ session_id: "{}", node_did: "{}", requester_did: "{}", agent_id: "general", created_at: "2026-10-10T00:00:00Z" }}) {{ _docID }} }}"#,
        escape_graphql_string(session), escape_graphql_string(&did), escape_graphql_string(&did)
    )).await.unwrap();
    let mut original = AgentRequestCreate::base(
        RequestPurpose::Normal,
        "reopen-original",
        &did,
        &did,
        "general",
        session,
        "original durable message",
        "interactive",
        "2026-10-10T00:00:00Z",
        AgentRequestAdmissionRecord::local_self(&did),
    );
    original.input.queue = Some(RequestQueue {
        delivery: QueueDelivery::Steer,
        position: None,
        source: QueueSource::User,
        policy: QueuePolicy::Append,
        key: None,
        queued_after_request_id: None,
        interrupted_request_id: None,
        background_completion_wake_version: None,
    });
    gents::sign_agent_request_create(db.node_identity.as_ref(), &mut original)
        .await
        .unwrap();
    let response = ConfigAccess::Local(db.node.clone())
        .write(
            "test.edit_reopen.original",
            &original.graphql_mutation().unwrap(),
        )
        .await
        .unwrap();
    let original_doc = gents::graphql::created_doc_id(&response, "AgentRequest").unwrap();
    let edit = PendingQueueEdit {
        expected_request_doc_ids: vec![original_doc.clone()],
        selected_request_doc_ids: vec![original_doc.clone()],
        messages: vec![PendingMessageEdit {
            request_doc_id: original_doc.clone(),
            content: "replacement durable message".into(),
        }],
    };
    let replacements = prepare_pending_user_edit(
        &ConfigAccess::Local(db.node.clone()),
        db.node_identity.as_ref(),
        AgentRequestAdmissionRecord::local_self(&did),
        &did,
        session,
        &did,
        &edit,
    )
    .await
    .unwrap();
    let now = chrono::Utc::now();
    let mut command = SessionInputEdit {
        version: SESSION_INPUT_EDIT_VERSION,
        command_id: "edit-before-reopen".into(),
        requester_did: did.clone(),
        requester_peer_id: None,
        node_did: did.clone(),
        session_id: session.into(),
        issued_at: (now - chrono::Duration::seconds(1)).to_rfc3339(),
        expires_at: (now + chrono::Duration::minutes(5)).to_rfc3339(),
        edit,
        replacements,
        signature: vec![],
    };
    command.signature = db
        .node_identity
        .sign(&command.signing_payload())
        .await
        .unwrap();
    write_command(&db, &command).await;
    db.simulate_process_crash().await.unwrap();
    assert_eq!(db.node_identity.did(), did);
    let cancel = CancellationToken::new();
    let _stop = Stop(cancel.clone());
    let task = tokio::spawn(run_session_input_edit_reconciler(
        db.node.clone(),
        db.node_identity.clone(),
        cancel.clone(),
    ));
    let row = wait_row(
        &db,
        "AgentSessionInputEdit",
        "command_id",
        &command.command_id,
        "receipt_json",
        "receipt_json",
    )
    .await;
    let receipt_json = row["receipt_json"].as_str().unwrap().to_owned();
    let receipt: SessionInputEditReceipt = serde_json::from_str(&receipt_json).unwrap();
    receipt.validate_for(&command).unwrap();
    assert!(db
        .node_identity
        .verify(&did, &receipt.signing_payload(), &receipt.signature)
        .await
        .unwrap());
    assert_eq!(receipt.outcome, SessionInputEditOutcome::Applied);
    assert_eq!(
        receipt.request_ids,
        vec![command.replacements[0].request_id.clone()]
    );
    cancel.cancel();
    task.await.unwrap().unwrap();
    let before = rows(
        &db,
        "AgentRequest",
        "{}",
        "_docID request_id content lifecycle_state superseded_by_request_doc_id",
    )
    .await;
    assert_eq!(before.len(), 2);
    let old = before
        .iter()
        .find(|row| row["_docID"] == original_doc)
        .unwrap();
    assert_eq!(old["content"], "original durable message");
    assert_eq!(old["lifecycle_state"], "superseded");
    assert_eq!(
        old["superseded_by_request_doc_id"],
        receipt.request_doc_ids[0]
    );
    let replacement = before
        .iter()
        .find(|row| row["_docID"] == receipt.request_doc_ids[0])
        .unwrap();
    assert_eq!(replacement["content"], "replacement durable message");
    db.simulate_process_crash().await.unwrap();
    let cancel = CancellationToken::new();
    let _stop = Stop(cancel.clone());
    let mut stale = command.clone();
    stale.command_id = "edit-after-reopen-stale".into();
    stale.signature = db
        .node_identity
        .sign(&stale.signing_payload())
        .await
        .unwrap();
    write_command(&db, &stale).await;
    let task = tokio::spawn(run_session_input_edit_reconciler(
        db.node.clone(),
        db.node_identity.clone(),
        cancel.clone(),
    ));
    let rejected = wait_row(
        &db,
        "AgentSessionInputEdit",
        "command_id",
        &stale.command_id,
        "receipt_json",
        "receipt_json",
    )
    .await;
    let rejected: SessionInputEditReceipt =
        serde_json::from_str(rejected["receipt_json"].as_str().unwrap()).unwrap();
    rejected.validate_for(&stale).unwrap();
    assert!(db
        .node_identity
        .verify(&did, &rejected.signing_payload(), &rejected.signature)
        .await
        .unwrap());
    assert_eq!(rejected.outcome, SessionInputEditOutcome::Rejected);
    let after = rows(
        &db,
        "AgentRequest",
        "{}",
        "_docID request_id content lifecycle_state superseded_by_request_doc_id",
    )
    .await;
    assert_eq!(after, before);
    let replay = wait_row(
        &db,
        "AgentSessionInputEdit",
        "command_id",
        &command.command_id,
        "receipt_json",
        "receipt_json",
    )
    .await;
    assert_eq!(replay["receipt_json"], receipt_json);
    cancel.cancel();
    task.await.unwrap().unwrap();
    assert_eq!(
        rows(
            &db,
            "AgentRequest",
            "{}",
            "_docID request_id content lifecycle_state superseded_by_request_doc_id"
        )
        .await,
        before
    );
}

async fn local_edit_command(db: &TestDb, suffix: &str) -> (SessionInputEdit, String) {
    local_edit_command_as(db, db.node_identity.as_ref(), suffix).await
}

async fn local_edit_command_as(
    db: &TestDb,
    identity: &dyn gents::identity::NodeIdentity,
    suffix: &str,
) -> (SessionInputEdit, String) {
    let did = identity.did();
    let session_id = format!("replicated-edit-{suffix}");
    let mut session =
        support::session_document_in_scope(did, &session_id, "general", "2026-10-10T00:00:00Z");
    session.requester_did = Some(did.into());
    support::create_session_document(&db.node, &session).await;
    let mut original = AgentRequestCreate::base(
        RequestPurpose::Normal,
        format!("original-{suffix}"),
        did,
        did,
        "general",
        &session_id,
        format!("original {suffix}"),
        "interactive",
        "2026-10-10T00:00:00Z",
        AgentRequestAdmissionRecord::local_self(did),
    );
    original.input.queue = Some(RequestQueue {
        delivery: QueueDelivery::Steer,
        position: None,
        source: QueueSource::User,
        policy: QueuePolicy::Append,
        key: None,
        queued_after_request_id: None,
        interrupted_request_id: None,
        background_completion_wake_version: None,
    });
    gents::sign_agent_request_create(identity, &mut original)
        .await
        .unwrap();
    let response = ConfigAccess::Local(db.node.clone())
        .write(
            "test.duplicate_edit.original",
            &original.graphql_mutation().unwrap(),
        )
        .await
        .unwrap();
    let original_id = gents::graphql::created_doc_id(&response, "AgentRequest").unwrap();
    let edit = PendingQueueEdit {
        expected_request_doc_ids: vec![original_id.clone()],
        selected_request_doc_ids: vec![original_id.clone()],
        messages: vec![PendingMessageEdit {
            request_doc_id: original_id.clone(),
            content: format!("edited {suffix}"),
        }],
    };
    let replacements = prepare_pending_user_edit(
        &ConfigAccess::Local(db.node.clone()),
        identity,
        AgentRequestAdmissionRecord::local_self(did),
        did,
        &session_id,
        did,
        &edit,
    )
    .await
    .unwrap();
    let now = chrono::Utc::now();
    let mut command = SessionInputEdit {
        version: SESSION_INPUT_EDIT_VERSION,
        command_id: format!("command-{suffix}"),
        requester_did: did.into(),
        requester_peer_id: None,
        node_did: did.into(),
        session_id,
        issued_at: (now - chrono::Duration::seconds(1)).to_rfc3339(),
        expires_at: (now + chrono::Duration::minutes(5)).to_rfc3339(),
        edit,
        replacements,
        signature: Vec::new(),
    };
    command.signature = identity.sign(&command.signing_payload()).await.unwrap();
    (command, original_id)
}

async fn connect_edit_replicas(left: &TestDb, right: &TestDb) {
    let (_, left_address) = support::enrollment::wait_for_peer_identity(&left.node).await;
    let (_, right_address) = support::enrollment::wait_for_peer_identity(&right.node).await;
    let collections = ["AgentSessionInputEdit", "AgentSession", "AgentRequest"]
        .map(str::to_owned)
        .to_vec();
    let left_admin = EmbeddedRemoteP2pAdmin::new(left.node.clone());
    let right_admin = EmbeddedRemoteP2pAdmin::new(right.node.clone());
    for admin in [&left_admin, &right_admin] {
        admin.add_p2p_collections(&collections).await.unwrap();
    }
    left_admin
        .connect(std::slice::from_ref(&right_address))
        .await
        .unwrap();
    right_admin
        .connect(std::slice::from_ref(&left_address))
        .await
        .unwrap();
    left_admin
        .add_replicator(&[right_address], &collections, &Default::default())
        .await
        .unwrap();
    right_admin
        .add_replicator(&[left_address], &collections, &Default::default())
        .await
        .unwrap();
}

async fn all_command_rows(db: &TestDb) -> Vec<serde_json::Value> {
    rows(
        db,
        "AgentSessionInputEdit",
        "{}",
        "_docID command_id intent_json receipt_json",
    )
    .await
}

async fn wait_for_command_documents(db: &TestDb, expected_ids: &[String]) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let found = all_command_rows(db).await;
        if expected_ids
            .iter()
            .all(|id| found.iter().any(|row| row["_docID"] == *id))
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "independent command documents did not converge: {found:?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test]
async fn same_physical_edit_replicates_and_replays_once() {
    let runtime = support::test_p2p_db("same-edit-runtime").await;
    let replica = support::test_p2p_db("same-edit-replica").await;
    let (command, original_id) = local_edit_command(&runtime, "same-intent").await;
    let runtime_id = write_command(&runtime, &command).await;
    connect_edit_replicas(&runtime, &replica).await;
    wait_for_command_documents(&runtime, std::slice::from_ref(&runtime_id)).await;
    wait_for_command_documents(&replica, std::slice::from_ref(&runtime_id)).await;
    let cancel = CancellationToken::new();
    let _stop = Stop(cancel.clone());
    let worker = tokio::spawn(run_session_input_edit_reconciler(
        runtime.node.clone(),
        runtime.node_identity.clone(),
        cancel.clone(),
    ));
    let observed = wait_row(
        &replica,
        "AgentSessionInputEdit",
        "_docID",
        &runtime_id,
        "receipt_json",
        "receipt_json",
    )
    .await;
    let receipt: SessionInputEditReceipt =
        serde_json::from_str(observed["receipt_json"].as_str().unwrap()).unwrap();
    receipt.validate_for(&command).unwrap();
    assert!(runtime
        .node_identity
        .verify(
            runtime.node_identity.did(),
            &receipt.signing_payload(),
            &receipt.signature
        )
        .await
        .unwrap());
    assert_eq!(receipt.outcome, SessionInputEditOutcome::Applied);
    assert_eq!(all_command_rows(&runtime).await.len(), 1);
    assert_eq!(all_command_rows(&replica).await.len(), 1);
    let requests = rows(
        &runtime,
        "AgentRequest",
        "{}",
        "_docID request_id lifecycle_state superseded_by_request_doc_id",
    )
    .await;
    assert_eq!(requests.len(), 2);
    let original = requests
        .iter()
        .find(|row| row["_docID"] == original_id)
        .unwrap();
    assert_eq!(original["lifecycle_state"], "superseded");
    assert_eq!(
        original["superseded_by_request_doc_id"],
        receipt.request_doc_ids[0]
    );
    assert_eq!(
        requests
            .iter()
            .filter(|row| row["request_id"] == command.replacements[0].request_id)
            .count(),
        1
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let peer_requests = rows(
            &replica,
            "AgentRequest",
            "{}",
            "_docID request_id lifecycle_state superseded_by_request_doc_id",
        )
        .await;
        if peer_requests.len() == requests.len()
            && requests.iter().all(|row| peer_requests.contains(row))
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "same-intent peer effects did not converge: {peer_requests:?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    cancel.cancel();
    worker.await.unwrap().unwrap();

    let cancel = CancellationToken::new();
    let _stop = Stop(cancel.clone());
    let worker = tokio::spawn(run_session_input_edit_reconciler(
        runtime.node.clone(),
        runtime.node_identity.clone(),
        cancel.clone(),
    ));
    // The second acknowledgement requires a later sweep of the persisted command.
    for suffix in ["replay-barrier-one", "replay-barrier-two"] {
        let (barrier, _) = local_edit_command(&runtime, suffix).await;
        let id = write_command(&runtime, &barrier).await;
        let row = wait_row(
            &runtime,
            "AgentSessionInputEdit",
            "_docID",
            &id,
            "receipt_json",
            "receipt_json",
        )
        .await;
        let receipt: SessionInputEditReceipt =
            serde_json::from_str(row["receipt_json"].as_str().unwrap()).unwrap();
        receipt.validate_for(&barrier).unwrap();
        assert_eq!(receipt.outcome, SessionInputEditOutcome::Applied);
    }
    cancel.cancel();
    worker.await.unwrap().unwrap();
    let replayed = all_command_rows(&runtime)
        .await
        .into_iter()
        .find(|row| row["_docID"] == runtime_id)
        .unwrap();
    assert_eq!(replayed["receipt_json"], observed["receipt_json"]);
    let filter = format!(
        "{{ session_id: {{ _eq: \"{}\" }} }}",
        escape_graphql_string(&command.session_id)
    );
    let after = rows(
        &runtime,
        "AgentRequest",
        &filter,
        "_docID request_id lifecycle_state superseded_by_request_doc_id",
    )
    .await;
    assert_eq!(after.len(), requests.len());
    assert!(requests.iter().all(|row| after.contains(row)));
}

async fn conflicting_offline_edits_preserve_queue_and_prior_receipt(
    late_twin: bool,
    same_intent: bool,
) {
    let left = support::test_p2p_db("conflicting-edit-left").await;
    let right = support::test_p2p_db("conflicting-edit-right").await;
    let identity = left.node_identity.clone();
    let (mut left_command, left_original) =
        local_edit_command_as(&left, identity.as_ref(), "collision-left").await;
    let (mut right_command, right_original) = if same_intent {
        (left_command.clone(), left_original.clone())
    } else {
        local_edit_command_as(&right, identity.as_ref(), "collision-right").await
    };
    for command in [&mut left_command, &mut right_command] {
        command.command_id = "replicated-command-collision".into();
        command.signature = identity.sign(&command.signing_payload()).await.unwrap();
    }
    let (left_id, right_id) = tokio::join!(
        write_command(&left, &left_command),
        write_command(&right, &right_command)
    );
    if same_intent {
        assert_eq!(
            serde_json::to_value(&left_command).unwrap(),
            serde_json::to_value(&right_command).unwrap()
        );
    }
    assert_ne!(
        left_id, right_id,
        "independent creates must remain distinct physical documents"
    );
    let (owner, peer, prior_command, twin_command, prior_id, twin_id) = if left_id > right_id {
        (
            &left,
            &right,
            &left_command,
            &right_command,
            &left_id,
            &right_id,
        )
    } else {
        (
            &right,
            &left,
            &right_command,
            &left_command,
            &right_id,
            &left_id,
        )
    };
    let prior_receipt = if late_twin {
        let cancel = CancellationToken::new();
        let _stop = Stop(cancel.clone());
        let worker = tokio::spawn(run_session_input_edit_reconciler(
            owner.node.clone(),
            identity.clone(),
            cancel.clone(),
        ));
        let observed = wait_row(
            owner,
            "AgentSessionInputEdit",
            "_docID",
            prior_id,
            "receipt_json",
            "receipt_json",
        )
        .await;
        cancel.cancel();
        worker.await.unwrap().unwrap();
        let receipt: SessionInputEditReceipt =
            serde_json::from_str(observed["receipt_json"].as_str().unwrap()).unwrap();
        receipt.validate_for(prior_command).unwrap();
        assert_eq!(receipt.outcome, SessionInputEditOutcome::Applied);
        Some(observed["receipt_json"].clone())
    } else {
        None
    };
    connect_edit_replicas(&left, &right).await;
    let physical_ids = vec![left_id.clone(), right_id.clone()];
    for db in [&left, &right] {
        wait_for_command_documents(db, &physical_ids).await;
        for command in [&left_command, &right_command] {
            let session = wait_row(
                db,
                "AgentSession",
                "session_id",
                &command.session_id,
                "node_did",
                "node_did",
            )
            .await;
            assert_eq!(session["node_did"], command.node_did);
        }
        for original in [&left_original, &right_original] {
            wait_row(db, "AgentRequest", "_docID", original, "_docID", "").await;
        }
    }
    let model: serde_json::Value = gents_lean_contract::load_contract_snapshot().unwrap();
    let case_name = if same_intent {
        "same_intent_physical_collision"
    } else if late_twin {
        "cross_session_collision_preserves_prior_receipt"
    } else {
        "simultaneous_command_identity_collision"
    };
    let case = model["session_input_edit_cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == case_name)
        .unwrap();
    assert_eq!(case["authority"]["command_identity_unique"], false);
    assert_eq!(case["before"], case["after"]);
    assert!(case["receipt"].is_null());
    let queue = |session: &str| {
        format!(
            "{{ session_id: {{ _eq: \"{}\" }} }}",
            escape_graphql_string(session)
        )
    };
    let fields = "_docID request_id content lifecycle_state superseded_by_request_doc_id";
    let before_prior = rows(
        owner,
        "AgentRequest",
        &queue(&prior_command.session_id),
        fields,
    )
    .await;
    let before_twin = rows(
        owner,
        "AgentRequest",
        &queue(&twin_command.session_id),
        fields,
    )
    .await;
    let cancel = CancellationToken::new();
    let _stop = Stop(cancel.clone());
    let worker = tokio::spawn(run_session_input_edit_reconciler(
        owner.node.clone(),
        identity.clone(),
        cancel.clone(),
    ));
    // A second acknowledged sentinel cannot belong to the sweep snapshot that
    // acknowledged the first: both colliding documents have been visited.
    for suffix in ["collision-barrier-one", "collision-barrier-two"] {
        let (barrier, _) = local_edit_command_as(owner, identity.as_ref(), suffix).await;
        let id = write_command(owner, &barrier).await;
        let row = wait_row(
            owner,
            "AgentSessionInputEdit",
            "_docID",
            &id,
            "receipt_json",
            "receipt_json",
        )
        .await;
        let receipt: SessionInputEditReceipt =
            serde_json::from_str(row["receipt_json"].as_str().unwrap()).unwrap();
        receipt.validate_for(&barrier).unwrap();
        assert!(identity
            .verify(
                identity.did(),
                &receipt.signing_payload(),
                &receipt.signature
            )
            .await
            .unwrap());
        assert_eq!(receipt.outcome, SessionInputEditOutcome::Applied);
    }
    cancel.cancel();
    worker.await.unwrap().unwrap();
    assert_eq!(
        rows(
            owner,
            "AgentRequest",
            &queue(&prior_command.session_id),
            fields
        )
        .await,
        before_prior
    );
    assert_eq!(
        rows(
            owner,
            "AgentRequest",
            &queue(&twin_command.session_id),
            fields
        )
        .await,
        before_twin
    );
    let commands = all_command_rows(owner).await;
    let prior = commands
        .iter()
        .find(|row| row["_docID"] == *prior_id)
        .unwrap();
    let twin = commands
        .iter()
        .find(|row| row["_docID"] == *twin_id)
        .unwrap();
    assert_eq!(
        prior["receipt_json"],
        prior_receipt.unwrap_or(serde_json::Value::Null)
    );
    assert!(
        twin["receipt_json"].is_null(),
        "colliding intent must not acquire a receipt or effect"
    );
    assert!(
        twin_id < prior_id,
        "late twin must replace the unique-index winner to exercise hidden prior history"
    );
    wait_for_command_documents(peer, &physical_ids).await;
    let expected_prior = rows(
        owner,
        "AgentRequest",
        &queue(&prior_command.session_id),
        fields,
    )
    .await;
    let expected_twin = rows(
        owner,
        "AgentRequest",
        &queue(&twin_command.session_id),
        fields,
    )
    .await;
    let expected_receipts = physical_ids
        .iter()
        .map(|id| {
            let row = commands.iter().find(|row| row["_docID"] == *id).unwrap();
            (id.clone(), row["receipt_json"].clone())
        })
        .collect::<Vec<_>>();
    let normalized = |mut values: Vec<serde_json::Value>| {
        values.sort_by(|left, right| left["_docID"].as_str().cmp(&right["_docID"].as_str()));
        values
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let peer_prior = rows(
            peer,
            "AgentRequest",
            &queue(&prior_command.session_id),
            fields,
        )
        .await;
        let peer_twin = rows(
            peer,
            "AgentRequest",
            &queue(&twin_command.session_id),
            fields,
        )
        .await;
        let peer_commands = all_command_rows(peer).await;
        let receipts_match = expected_receipts.iter().all(|(id, receipt)| {
            peer_commands
                .iter()
                .any(|row| row["_docID"] == *id && row["receipt_json"] == *receipt)
        });
        if normalized(peer_prior.clone()) == normalized(expected_prior.clone())
            && normalized(peer_twin.clone()) == normalized(expected_twin.clone())
            && receipts_match
        {
            break;
        }
        assert!(Instant::now() < deadline, "peer did not converge exact queue effects and receipts: prior={peer_prior:?}, twin={peer_twin:?}, commands={peer_commands:?}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test]
async fn different_offline_edit_intents_with_one_command_id_do_not_apply() {
    conflicting_offline_edits_preserve_queue_and_prior_receipt(false, false).await;
}

#[tokio::test]
async fn late_smaller_command_id_twin_cannot_apply_in_another_session() {
    conflicting_offline_edits_preserve_queue_and_prior_receipt(true, false).await;
}

#[tokio::test]
async fn identical_offline_edit_intents_with_distinct_physical_ids_do_not_apply() {
    conflicting_offline_edits_preserve_queue_and_prior_receipt(false, true).await;
}
