use std::time::{Duration, Instant};

use gents::agent::p2p_reconcile::session_commands::run_session_command_reconciler;
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
    ConfigAccess::local(db.node.clone())
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
    let response = ConfigAccess::local(db.node.clone()).write("test.signed_remote_edit", &format!(r#"mutation {{ create_AgentSessionInputEdit(input: {{
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
    let cases = contract["session_input_edit_cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|case| {
            case["command"]["has_peer"] == true
                && case["authority"]["command_identity_unique"] == true
                && case["authority"]["route_applied"] == true
                && case["history"].as_array().unwrap().is_empty()
        })
        .collect::<Vec<_>>();
    assert_eq!(cases.len(), 2, "remote generated input-edit cases");
    let remote = RemoteEdit {
        runtime: &runtime,
        client: &client,
        admission,
        peer: peer.clone(),
    };
    let (fresh, revoked): (Vec<_>, Vec<_>) = cases
        .into_iter()
        .partition(|case| case["authority"]["enrollment_fresh"] == true);
    let mut prepared = Vec::new();
    for case in &fresh {
        prepared.push(remote.prepare(case).await);
    }
    let edit_cancel = cancel.child_token();
    let edit_task = tokio::spawn(run_session_command_reconciler(
        runtime.node.clone(),
        runtime.node_identity.clone(),
        edit_cancel.clone(),
    ));
    for (case, modeled) in fresh.iter().zip(&prepared) {
        remote.assert_modeled(case, modeled, &client).await;
    }
    edit_cancel.cancel();
    edit_task.await.unwrap().unwrap();
    let mut prepared = Vec::new();
    for case in &revoked {
        prepared.push(remote.prepare(case).await);
    }
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
    let revoked_task = tokio::spawn(run_session_command_reconciler(
        runtime.node.clone(),
        runtime.node_identity.clone(),
        cancel.clone(),
    ));
    for (case, modeled) in revoked.iter().zip(&prepared) {
        remote.assert_modeled(case, modeled, &runtime).await;
    }
    cancel.cancel();
    for task in [revoked_task, pairing_task, enrollment_task] {
        task.await.unwrap().unwrap();
    }
}

struct RemoteEdit<'a> {
    runtime: &'a TestDb,
    client: &'a TestDb,
    admission: AgentRequestAdmissionRecord,
    peer: String,
}

struct ModeledEdit {
    session_id: String,
    command: SessionInputEdit,
    request_ids: rapidhash::RapidHashMap<u64, String>,
    _active: gents::RequestLifecycle,
}

fn model_ids(values: &serde_json::Value) -> Vec<u64> {
    values
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_u64().unwrap())
        .collect()
}

type QueueImage = (
    Option<String>,
    Vec<String>,
    Vec<String>,
    std::collections::BTreeSet<String>,
);

impl ModeledEdit {
    fn queue(&self, state: &serde_json::Value) -> QueueImage {
        let entries = |key: &str| {
            state[key]
                .as_array()
                .unwrap()
                .iter()
                .map(|entry| self.request_ids[&entry["request_id"].as_u64().unwrap()].clone())
                .collect()
        };
        (
            state["active"]
                .as_u64()
                .map(|id| self.request_ids[&id].clone()),
            entries("folding"),
            entries("pending"),
            model_ids(&state["terminal"])
                .iter()
                .map(|id| self.request_ids[id].clone())
                .collect(),
        )
    }

    async fn native_queue(&self, runtime: &TestDb) -> QueueImage {
        let requests = rows(
            runtime,
            "AgentRequest",
            &format!(
                "{{ session_id: {{ _eq: \"{}\" }} }}",
                escape_graphql_string(&self.session_id)
            ),
            "request_id lifecycle_state",
        )
        .await;
        let in_state = |states: &[&str]| {
            requests
                .iter()
                .filter(|row| states.contains(&row["lifecycle_state"].as_str().unwrap()))
                .map(|row| row["request_id"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>()
        };
        let active = in_state(&["claimed", "processing"]);
        assert!(active.len() <= 1, "one active request: {active:?}");
        let pending = gents::lifecycle::pending_user_queue(
            &ConfigAccess::local(runtime.node.clone()),
            runtime.node_identity.did(),
            &self.session_id,
            &self.command.requester_did,
        )
        .await
        .unwrap()
        .entries
        .into_iter()
        .map(|entry| entry.request_id)
        .collect::<Vec<_>>();
        let terminal = requests
            .iter()
            .filter(|row| {
                gents_protocol::request_lifecycle::RequestLifecycleState::is_terminal_str(
                    row["lifecycle_state"].as_str(),
                )
            })
            .map(|row| row["request_id"].as_str().unwrap().to_owned())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            requests.len(),
            active.len() + pending.len() + terminal.len(),
            "every request is active, queued or terminal"
        );
        (active.into_iter().next(), Vec::new(), pending, terminal)
    }
}

impl RemoteEdit<'_> {
    async fn prepare(&self, case: &serde_json::Value) -> ModeledEdit {
        let did = self.runtime.node_identity.did();
        let requester = self.client.node_identity.did();
        let name = case["name"].as_str().unwrap();
        let session_id = format!("remote-{name}");
        let mut session = support::session_document_in_scope(
            did,
            &session_id,
            "general",
            &chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        );
        session.requester_did = Some(requester.into());
        support::create_session_document(&self.runtime.node, &session).await;
        let mut request_ids = rapidhash::RapidHashMap::default();
        let mut doc_ids = rapidhash::RapidHashMap::default();
        let initial = &case["initial"]["queue"];
        assert_eq!(initial["folding"], serde_json::json!([]), "{name}: folding");
        let active_id = initial["active"].as_u64().expect("modeled active request");
        let mut active = AgentRequestCreate::base(
            RequestPurpose::Normal,
            format!("{name}-{active_id}"),
            did,
            requester,
            "general",
            &session_id,
            format!("message {active_id}"),
            "interactive",
            (chrono::Utc::now() - chrono::Duration::minutes(1))
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            self.admission.clone(),
        );
        gents::sign_agent_request_create(self.client.node_identity.as_ref(), &mut active)
            .await
            .unwrap();
        let written = ConfigAccess::local(self.runtime.node.clone())
            .write("test.remote_active", &active.graphql_mutation().unwrap())
            .await
            .unwrap();
        let active_doc = gents::graphql::created_doc_id(&written, "AgentRequest").unwrap();
        request_ids.insert(active_id, active.request_id.clone());
        for entry in initial["pending"].as_array().unwrap() {
            let id = entry["request_id"].as_u64().unwrap();
            assert_eq!(
                entry["requester_id"], 1,
                "requester 1 is the enrolled client"
            );
            assert_eq!(entry["fresh"], true);
            let mut original = AgentRequestCreate::base(
                RequestPurpose::Normal,
                format!("{name}-{id}"),
                did,
                requester,
                "general",
                &session_id,
                format!("message {id}"),
                entry["execution_origin"].as_str().unwrap(),
                chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                self.admission.clone(),
            );
            original.input.queue = Some(RequestQueue {
                source: serde_json::from_value(entry["source"].clone()).unwrap(),
                policy: serde_json::from_value(entry["policy"].clone()).unwrap(),
                delivery: serde_json::from_value(entry["delivery"].clone()).unwrap(),
                position: None,
                key: None,
                queued_after_request_id: entry["queued_after"].as_u64().map(|id| id.to_string()),
                interrupted_request_id: None,
                background_completion_wake_version: None,
            });
            gents::sign_agent_request_create(self.client.node_identity.as_ref(), &mut original)
                .await
                .unwrap();
            let written = ConfigAccess::local(self.runtime.node.clone())
                .write("test.remote_pending", &original.graphql_mutation().unwrap())
                .await
                .unwrap();
            doc_ids.insert(
                id,
                gents::graphql::created_doc_id(&written, "AgentRequest").unwrap(),
            );
            request_ids.insert(id, original.request_id.clone());
            wait_row(
                self.client,
                "AgentRequest",
                "request_id",
                &original.request_id,
                "_docID",
                "",
            )
            .await;
        }
        let request = gents::DefraWatcher::new(self.runtime.node.clone(), did)
            .expect("request watcher")
            .try_fetch_request(&active_doc)
            .await
            .unwrap()
            .unwrap();
        let mut active = gents::RequestLifecycle::new_with_node_did(
            self.runtime.node.clone(),
            "general",
            did,
            request,
            600,
        );
        assert_eq!(
            active.claim().await.unwrap(),
            gents::lifecycle::ClaimOutcome::Claimed
        );
        wait_row(
            self.client,
            "AgentRequest",
            "_docID",
            &active_doc,
            "execution_generation",
            "execution_generation",
        )
        .await;
        let model = &case["command"];
        let authority = &case["authority"];
        let (offset, count) = (
            model["offset"].as_u64().unwrap() as usize,
            model["count"].as_u64().unwrap() as usize,
        );
        let pending = case["initial"]["queue"]["pending"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["request_id"].as_u64().unwrap())
            .collect::<Vec<_>>();
        let selected = pending.iter().skip(offset).take(count).collect::<Vec<_>>();
        let replacements = model["replacements"].as_array().unwrap();
        let mut edit = PendingQueueEdit {
            expected_request_doc_ids: pending.iter().map(|id| doc_ids[id].clone()).collect(),
            selected_request_doc_ids: selected.iter().map(|id| doc_ids[id].clone()).collect(),
            messages: replacements
                .iter()
                .zip(&selected)
                .map(|(replacement, id)| PendingMessageEdit {
                    authored_input: None,
                    request_doc_id: doc_ids[id].clone(),
                    content: format!("message {}", replacement["request_id"]),
                })
                .collect(),
        };
        let mut native = prepare_pending_user_edit(
            &ConfigAccess::local(self.client.node.clone()),
            self.client.node_identity.as_ref(),
            self.admission.clone(),
            did,
            &session_id,
            requester,
            &edit,
        )
        .await
        .unwrap();
        edit.expected_request_doc_ids = model_ids(&model["expected"])
            .iter()
            .map(|id| doc_ids[id].clone())
            .collect();
        if authority["replacements_valid"] == false {
            native[0].admission.signature[0] ^= 1;
        }
        for (replacement, create) in replacements.iter().zip(&native) {
            request_ids.insert(
                replacement["request_id"].as_u64().unwrap(),
                create.request_id.clone(),
            );
        }
        let now = case["now"].as_u64().unwrap();
        let instant = |model: &serde_json::Value| {
            let model = model.as_u64().unwrap();
            let offset = chrono::Duration::seconds(model as i64);
            if model <= now {
                chrono::Utc::now() - chrono::Duration::hours(1) + offset
            } else {
                chrono::Utc::now() + chrono::Duration::hours(1) + offset
            }
        };
        let mut command = SessionInputEdit {
            version: SESSION_INPUT_EDIT_VERSION,
            command_id: format!("edit-{name}"),
            requester_did: requester.into(),
            requester_peer_id: Some(self.peer.clone()),
            node_did: did.into(),
            session_id: if authority["session_owned"] == true {
                session_id.clone()
            } else {
                format!("{session_id}-foreign")
            },
            issued_at: instant(&model["issued_at"]).to_rfc3339(),
            expires_at: instant(&model["expires_at"]).to_rfc3339(),
            edit,
            replacements: native,
            signature: Vec::new(),
        };
        command.signature = self
            .client
            .node_identity
            .sign(&command.signing_payload())
            .await
            .unwrap();
        if authority["signature_valid"] == false {
            command.signature[0] ^= 1;
        }
        write_command(self.client, &command).await;
        wait_row(
            self.runtime,
            "AgentSessionInputEdit",
            "command_id",
            &command.command_id,
            "_docID",
            "",
        )
        .await;
        let modeled = ModeledEdit {
            session_id,
            command,
            request_ids,
            _active: active,
        };
        assert_eq!(
            modeled.native_queue(self.runtime).await,
            modeled.queue(initial),
            "{name}: seeded queue"
        );
        modeled
    }

    async fn assert_modeled(
        &self,
        case: &serde_json::Value,
        modeled: &ModeledEdit,
        observer: &TestDb,
    ) {
        let name = case["name"].as_str().unwrap();
        let command = &modeled.command;
        let observed = wait_row(
            observer,
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
        assert!(self
            .client
            .node_identity
            .verify(
                self.runtime.node_identity.did(),
                &receipt.signing_payload(),
                &receipt.signature
            )
            .await
            .unwrap());
        let expected = &case["receipt"];
        assert_eq!(
            serde_json::to_value(receipt.outcome).unwrap(),
            expected["outcome"],
            "{name}: outcome"
        );
        let mapped = |ids: Vec<u64>| {
            ids.iter()
                .map(|id| modeled.request_ids[id].clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            receipt.request_ids,
            mapped(model_ids(&expected["request_ids"])),
            "{name}: receipt request IDs"
        );
        assert_eq!(
            modeled.native_queue(self.runtime).await,
            modeled.queue(&case["after"]["queue"]),
            "{name}: queue state"
        );
        for id in &receipt.request_ids {
            let replacement =
                wait_row(self.client, "AgentRequest", "request_id", id, "content", "").await;
            assert!(replacement["content"]
                .as_str()
                .unwrap()
                .starts_with("message "));
        }
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
    ConfigAccess::local(db.node.clone()).write("test.edit_reopen.session", &format!(
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
    let response = ConfigAccess::local(db.node.clone())
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
            authored_input: None,
            request_doc_id: original_doc.clone(),
            content: "replacement durable message".into(),
        }],
    };
    let replacements = prepare_pending_user_edit(
        &ConfigAccess::local(db.node.clone()),
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
    let task = tokio::spawn(run_session_command_reconciler(
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
    stale.replacements = prepare_pending_user_edit(
        &ConfigAccess::local(db.node.clone()),
        db.node_identity.as_ref(),
        AgentRequestAdmissionRecord::local_self(&did),
        &did,
        session,
        &did,
        &PendingQueueEdit {
            expected_request_doc_ids: receipt.request_doc_ids.clone(),
            selected_request_doc_ids: receipt.request_doc_ids.clone(),
            messages: vec![PendingMessageEdit {
                authored_input: None,
                request_doc_id: receipt.request_doc_ids[0].clone(),
                content: "stale replacement".into(),
            }],
        },
    )
    .await
    .unwrap();
    stale.signature = db
        .node_identity
        .sign(&stale.signing_payload())
        .await
        .unwrap();
    write_command(&db, &stale).await;
    let task = tokio::spawn(run_session_command_reconciler(
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
    let response = ConfigAccess::local(db.node.clone())
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
            authored_input: None,
            request_doc_id: original_id.clone(),
            content: format!("edited {suffix}"),
        }],
    };
    let replacements = prepare_pending_user_edit(
        &ConfigAccess::local(db.node.clone()),
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
    let worker = tokio::spawn(run_session_command_reconciler(
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
    let worker = tokio::spawn(run_session_command_reconciler(
        runtime.node.clone(),
        runtime.node_identity.clone(),
        cancel.clone(),
    ));
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
        let worker = tokio::spawn(run_session_command_reconciler(
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
    let case = model["session_input_edit_cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| {
            let history = case["history"].as_array().unwrap();
            case["authority"]["command_identity_unique"] == false
                && if late_twin {
                    history.iter().any(|step| step["kind"] == "other_session")
                } else {
                    history.is_empty()
                }
        })
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
    let worker = tokio::spawn(run_session_command_reconciler(
        owner.node.clone(),
        identity.clone(),
        cancel.clone(),
    ));
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
