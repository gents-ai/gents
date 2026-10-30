use super::*;
use crate::eval::runner::embedded::home::EmbeddedHome;
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use gents_protocol::request_input::{QueueDelivery, QueuePolicy, QueueSource, RequestQueue};
use gents_protocol::session_input_edit::{PendingMessageEdit, PendingQueueEdit};
use tokio_util::sync::CancellationToken;

async fn temp_home(p2p: bool) -> EmbeddedHome {
    let tempdir = tempfile::tempdir().unwrap();
    let config: Option<crate::eval::runner::embedded::home::P2PConfigForPath> = p2p.then(|| {
        Arc::new(|data_path: &std::path::Path| defra_node::P2PConfig {
            port: 0,
            bind_addr: Some(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)),
            relay_mode: p2p::iroh::IrohRelayModeConfig::Disabled,
            discovery: p2p::iroh::IrohDiscoveryConfig::Disabled,
            allowlist: p2p::iroh::IrohAllowlistConfig::AcceptAll,
            max_concurrent_multipath_paths: None,
            secret_key_path: Some(data_path.join("p2p.key")),
            load_persisted_collections: true,
            max_concurrent_dag_fetches: p2p::sync::DEFAULT_MAX_CONCURRENT_DAG_FETCHES,
            max_concurrent_push_tasks: p2p::sync::DEFAULT_MAX_CONCURRENT_PUSH_TASKS,
            rate_limit_burst: p2p::sync::DEFAULT_RATE_LIMIT_BURST,
            rate_limit_rate: p2p::sync::DEFAULT_RATE_LIMIT_RATE,
            max_doc_sync_request_doc_ids: p2p::sync::DEFAULT_MAX_DOC_SYNC_REQUEST_DOC_IDS,
            max_pending_dags: p2p::sync::DEFAULT_MAX_PENDING_DAGS,
            rebroadcast_on_merge: false,
        }) as _
    });
    let home = EmbeddedHome::in_tempdir(tempdir, config).await.unwrap();
    crate::test_support::install_test_agent(&home.node, home.identity.did(), "general").await;
    home
}

async fn edit_command(home: &EmbeddedHome, issued_at: DateTime<Utc>) -> SessionInputEdit {
    let did = home.identity.did();
    let access = ConfigAccess::local(home.node.clone());
    access.write("test.edit.session", &format!(r#"mutation {{ create_AgentSession(input: {{ session_id: "edit-session", node_did: "{did}", requester_did: "{did}", agent_id: "general", created_at: "2026-10-10T00:00:00Z" }}) {{ _docID }} }}"#,
        did = escape_graphql_string(did))).await.unwrap();
    let mut create = AgentRequestCreate::base(
        RequestPurpose::Normal,
        "original",
        did,
        did,
        "general",
        "edit-session",
        "original content",
        "interactive",
        "2026-10-10T00:00:00Z",
        AgentRequestAdmissionRecord::local_self(did),
    );
    create.input.queue = Some(RequestQueue {
        delivery: QueueDelivery::Steer,
        position: None,
        source: QueueSource::User,
        policy: QueuePolicy::Append,
        key: None,
        queued_after_request_id: None,
        interrupted_request_id: None,
        background_completion_wake_version: None,
    });
    crate::sign_agent_request_create(home.identity.as_ref(), &mut create)
        .await
        .unwrap();
    let response = access
        .write("test.edit.request", &create.graphql_mutation().unwrap())
        .await
        .unwrap();
    let original = crate::graphql::created_doc_id(&response, "AgentRequest").unwrap();
    let edit = PendingQueueEdit {
        expected_request_doc_ids: vec![original.clone()],
        selected_request_doc_ids: vec![original.clone()],
        messages: vec![PendingMessageEdit {
            authored_input: None,
            request_doc_id: original,
            content: "corrected content".into(),
        }],
    };
    let replacements = crate::request::lifecycle::queue::prepare_pending_user_edit(
        &access,
        home.identity.as_ref(),
        AgentRequestAdmissionRecord::local_self(did),
        did,
        "edit-session",
        did,
        &edit,
    )
    .await
    .unwrap();
    let mut command = SessionInputEdit {
        version: SESSION_INPUT_EDIT_VERSION,
        command_id: uuid::Uuid::new_v4().to_string(),
        requester_did: did.to_owned(),
        requester_peer_id: None,
        node_did: did.to_owned(),
        session_id: "edit-session".into(),
        issued_at: issued_at.to_rfc3339(),
        expires_at: (issued_at + chrono::Duration::minutes(5)).to_rfc3339(),
        edit,
        replacements,
        signature: Vec::new(),
    };
    command.signature = home
        .identity
        .sign(&command.signing_payload())
        .await
        .unwrap();
    command
}

async fn submit(node: &Arc<EmbeddedNode>, command: &SessionInputEdit) -> String {
    let response = ConfigAccess::local(node.clone()).write("test.edit.intent", &format!(r#"mutation {{ create_AgentSessionInputEdit(input: {{ command_id: "{}", requester_did: "{}", node_did: "{}", session_id: "{}", intent_json: "{}" }}) {{ _docID }} }}"#,
        escape_graphql_string(&command.command_id), escape_graphql_string(&command.requester_did), escape_graphql_string(&command.node_did), escape_graphql_string(&command.session_id), escape_graphql_string(&serde_json::to_string(command).unwrap()))).await.unwrap();
    crate::graphql::created_doc_id(&response, "AgentSessionInputEdit").unwrap()
}

async fn raw_receipt(node: &Arc<EmbeddedNode>, id: &str) -> Option<String> {
    let value = ConfigAccess::local(node.clone()).execute(&format!(r#"{{ AgentSessionInputEdit(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ receipt_json }} }}"#, escape_graphql_string(id))).await.unwrap();
    value["data"]["AgentSessionInputEdit"][0]["receipt_json"]
        .as_str()
        .map(str::to_owned)
}

async fn requests(node: &Arc<EmbeddedNode>) -> serde_json::Value {
    ConfigAccess::local(node.clone()).execute("{ AgentRequest { _docID request_id content lifecycle_state superseded_by_request_doc_id } }").await.unwrap()["data"]["AgentRequest"].clone()
}

#[tokio::test]
async fn future_issued_command_defers_without_receipt_then_applies() {
    let home = temp_home(false).await;
    let issued_at = Utc::now() + chrono::Duration::seconds(2);
    let command = edit_command(&home, issued_at).await;
    let id = submit(&home.node, &command).await;
    let before = requests(&home.node).await;
    let deferred = process_command(&home.node, &home.identity, &id)
        .await
        .unwrap();
    assert_eq!(
        deferred.map(|at| at.timestamp_millis()),
        Some(issued_at.timestamp_millis())
    );
    assert!(raw_receipt(&home.node, &id).await.is_none());
    assert_eq!(requests(&home.node).await, before);

    let cancel = CancellationToken::new();
    let worker = tokio::spawn(run_session_command_reconciler(
        home.node.clone(),
        home.identity.clone(),
        cancel.clone(),
    ));
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
    let raw = loop {
        if let Some(raw) = raw_receipt(&home.node, &id).await {
            break raw;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "deferred edit was not retried once its issuance passed"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    cancel.cancel();
    worker.await.unwrap().unwrap();
    let receipt: SessionInputEditReceipt = serde_json::from_str(&raw).unwrap();
    receipt.validate_for(&command).unwrap();
    assert_eq!(receipt.outcome, SessionInputEditOutcome::Applied);
}

#[tokio::test]
async fn overwritten_receipt_after_apply_is_reissued_as_applied() {
    let home = temp_home(false).await;
    let command = edit_command(&home, Utc::now() - chrono::Duration::seconds(1)).await;
    let id = submit(&home.node, &command).await;
    process_command(&home.node, &home.identity, &id)
        .await
        .unwrap();
    let applied: SessionInputEditReceipt =
        serde_json::from_str(&raw_receipt(&home.node, &id).await.unwrap()).unwrap();
    assert_eq!(applied.outcome, SessionInputEditOutcome::Applied);
    let effects = requests(&home.node).await;
    for overwrite in ["{}".to_owned(), String::new()] {
        ConfigAccess::local(home.node.clone()).write("test.overwrite.receipt", &format!(r#"mutation {{ update_AgentSessionInputEdit(docID: "{}", input: {{ receipt_json: "{}" }}) {{ _docID }} }}"#,
            escape_graphql_string(&id), escape_graphql_string(&overwrite))).await.unwrap();
        process_command(&home.node, &home.identity, &id)
            .await
            .unwrap();
        let reissued: SessionInputEditReceipt =
            serde_json::from_str(&raw_receipt(&home.node, &id).await.unwrap()).unwrap();
        reissued.validate_for(&command).unwrap();
        assert_eq!(reissued.outcome, SessionInputEditOutcome::Applied);
        assert_eq!(reissued.request_doc_ids, applied.request_doc_ids);
        assert_eq!(reissued.request_ids, applied.request_ids);
        assert_eq!(requests(&home.node).await, effects);
    }
}

#[tokio::test]
async fn late_twin_of_an_applied_command_is_left_unanswered_without_error() {
    let owner = temp_home(true).await;
    let peer = temp_home(true).await;
    let command = edit_command(&owner, Utc::now() - chrono::Duration::seconds(1)).await;
    let prior = submit(&owner.node, &command).await;
    process_command(&owner.node, &owner.identity, &prior)
        .await
        .unwrap();
    let receipt = raw_receipt(&owner.node, &prior).await.unwrap();
    let effects = requests(&owner.node).await;
    let twin = submit(&peer.node, &command).await;
    assert_ne!(twin, prior);

    use super::super::RemoteP2pAdmin;
    let collections = vec!["AgentSessionInputEdit".to_owned()];
    let mut addresses = Vec::new();
    for home in [&owner, &peer] {
        let p2p = home.node.p2p().unwrap();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
        let address = loop {
            if let Some(address) = p2p.shareable_address().await.ok().flatten() {
                break address;
            }
            assert!(tokio::time::Instant::now() < deadline, "no P2P address");
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        };
        addresses.push(address);
    }
    let owner_admin = super::super::EmbeddedRemoteP2pAdmin::new(owner.node.clone());
    let peer_admin = super::super::EmbeddedRemoteP2pAdmin::new(peer.node.clone());
    for admin in [&owner_admin, &peer_admin] {
        admin.add_p2p_collections(&collections).await.unwrap();
    }
    owner_admin.connect(&addresses[1..]).await.unwrap();
    peer_admin
        .add_replicator(&addresses[..1], &collections, &Default::default())
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let rows = ConfigAccess::local(owner.node.clone())
            .execute("{ AgentSessionInputEdit { _docID } }")
            .await
            .unwrap();
        if rows["data"]["AgentSessionInputEdit"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["_docID"] == twin.as_str())
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "twin did not replicate"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    for id in [&twin, &prior] {
        assert_eq!(
            process_command(&owner.node, &owner.identity, id)
                .await
                .unwrap(),
            None
        );
    }
    assert!(raw_receipt(&owner.node, &twin).await.is_none());
    assert_eq!(raw_receipt(&owner.node, &prior).await.unwrap(), receipt);
    assert_eq!(requests(&owner.node).await, effects);
}

async fn overwrite_receipt(node: &Arc<EmbeddedNode>, id: &str, raw: &str) {
    ConfigAccess::local(node.clone()).write("test.overwrite.receipt", &format!(r#"mutation {{ update_AgentSessionInputEdit(docID: "{}", input: {{ receipt_json: "{}" }}) {{ _docID }} }}"#,
        escape_graphql_string(id), escape_graphql_string(raw))).await.unwrap();
}

async fn reconcile_until_verified(
    home: &EmbeddedHome,
    command: &SessionInputEdit,
    id: &str,
) -> SessionInputEditReceipt {
    let cancel = CancellationToken::new();
    let worker = tokio::spawn(run_session_command_reconciler(
        home.node.clone(),
        home.identity.clone(),
        cancel.clone(),
    ));
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
    let receipt = loop {
        let raw = raw_receipt(&home.node, id).await;
        if let Some(receipt) = verified_receipt(home.identity.as_ref(), command, raw.as_deref())
            .await
            .unwrap()
        {
            break receipt;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "reconciler left the command without a verified receipt"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    cancel.cancel();
    worker.await.unwrap().unwrap();
    receipt
}

#[tokio::test]
async fn receipt_overwritten_while_stopped_is_reissued_at_restart() {
    let home = temp_home(false).await;
    let command = edit_command(&home, Utc::now() - chrono::Duration::seconds(1)).await;
    let id = submit(&home.node, &command).await;
    let applied = reconcile_until_verified(&home, &command, &id).await;
    assert_eq!(applied.outcome, SessionInputEditOutcome::Applied);
    let effects = requests(&home.node).await;
    overwrite_receipt(&home.node, &id, "{}").await;

    let reissued = reconcile_until_verified(&home, &command, &id).await;
    assert_eq!(reissued.outcome, SessionInputEditOutcome::Applied);
    assert_eq!(reissued.request_doc_ids, applied.request_doc_ids);
    assert_eq!(reissued.request_ids, applied.request_ids);
    assert_eq!(requests(&home.node).await, effects);
}

#[tokio::test]
async fn future_issued_command_with_invalid_receipt_is_processed_at_its_deadline() {
    let home = temp_home(false).await;
    let issued_at = Utc::now() + chrono::Duration::seconds(2);
    let command = edit_command(&home, issued_at).await;
    let id = submit(&home.node, &command).await;
    overwrite_receipt(&home.node, &id, "{}").await;

    let receipt = reconcile_until_verified(&home, &command, &id).await;
    assert!(Utc::now() >= issued_at);
    assert_eq!(receipt.outcome, SessionInputEditOutcome::Applied);
}
