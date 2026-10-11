use super::*;
use crate::identity::KeyIdentity;
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use gents_protocol::request_input::{
    QueueDelivery, QueuePolicy, QueueSource, RequestInput, RequestQueue,
};
use gents_protocol::session_input_edit::{PendingMessageEdit, PendingQueueEdit};

struct Fixture {
    node: Arc<EmbeddedNode>,
    identity: Arc<dyn NodeIdentity>,
    _dir: tempfile::TempDir,
    original: String,
}

impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let identity: Arc<dyn NodeIdentity> =
            Arc::new(KeyIdentity::load_or_create(dir.path().join("node.key"), None).unwrap());
        let node = Arc::new(
            EmbeddedNode::builder()
                .data_path(dir.path())
                .with_node_identity_did(identity.did())
                .build()
                .await
                .unwrap(),
        );
        crate::ensure_runtime_schemas(&node).await.unwrap();
        crate::test_support::install_test_agent(&node, identity.did(), "general").await;
        let access = ConfigAccess::Local(node.clone());
        access.write("test.edit.session", &format!(r#"mutation {{ create_AgentSession(input: {{ session_id: "edit-session", node_did: "{}", requester_did: "{}", agent_id: "general", created_at: "2026-10-10T00:00:00Z" }}) {{ _docID }} }}"#,
            escape_graphql_string(identity.did()), escape_graphql_string(identity.did()))).await.unwrap();
        let mut create = AgentRequestCreate::base(
            RequestPurpose::Normal,
            "original",
            identity.did(),
            identity.did(),
            "general",
            "edit-session",
            "original content",
            "interactive",
            "2026-10-10T00:00:00Z",
            AgentRequestAdmissionRecord::local_self(identity.did()),
        );
        create.input = RequestInput {
            queue: Some(RequestQueue {
                delivery: QueueDelivery::Steer,
                position: None,
                source: QueueSource::User,
                policy: QueuePolicy::Append,
                key: None,
                queued_after_request_id: None,
                interrupted_request_id: None,
                background_completion_wake_version: None,
            }),
            ..Default::default()
        };
        crate::sign_agent_request_create(identity.as_ref(), &mut create)
            .await
            .unwrap();
        let response = access
            .write("test.edit.request", &create.graphql_mutation().unwrap())
            .await
            .unwrap();
        let original = crate::graphql::created_doc_id(&response, "AgentRequest").unwrap();
        Self {
            node,
            identity,
            _dir: dir,
            original,
        }
    }

    async fn command(&self) -> SessionInputEdit {
        let edit = PendingQueueEdit {
            expected_request_doc_ids: vec![self.original.clone()],
            selected_request_doc_ids: vec![self.original.clone()],
            messages: vec![PendingMessageEdit {
                request_doc_id: self.original.clone(),
                content: "corrected content".into(),
            }],
        };
        let replacements = crate::lifecycle::queue::prepare_pending_user_edit(
            &ConfigAccess::Local(self.node.clone()),
            self.identity.as_ref(),
            AgentRequestAdmissionRecord::local_self(self.identity.did()),
            self.identity.did(),
            "edit-session",
            self.identity.did(),
            &edit,
        )
        .await
        .unwrap();
        let mut command = SessionInputEdit {
            version: SESSION_INPUT_EDIT_VERSION,
            command_id: uuid::Uuid::new_v4().to_string(),
            requester_did: self.identity.did().to_owned(),
            requester_peer_id: None,
            node_did: self.identity.did().to_owned(),
            session_id: "edit-session".into(),
            issued_at: (Utc::now() - chrono::Duration::seconds(1)).to_rfc3339(),
            expires_at: (Utc::now() + chrono::Duration::minutes(5)).to_rfc3339(),
            edit,
            replacements,
            signature: Vec::new(),
        };
        command.signature = self
            .identity
            .sign(&command.signing_payload())
            .await
            .unwrap();
        command
    }

    async fn submit(&self, command: &SessionInputEdit) -> String {
        let response = ConfigAccess::Local(self.node.clone()).write("test.edit.intent", &format!(r#"mutation {{ create_AgentSessionInputEdit(input: {{ command_id: "{}", requester_did: "{}", node_did: "{}", session_id: "{}", intent_json: "{}" }}) {{ _docID }} }}"#,
            escape_graphql_string(&command.command_id), escape_graphql_string(&command.requester_did), escape_graphql_string(&command.node_did), escape_graphql_string(&command.session_id), escape_graphql_string(&serde_json::to_string(command).unwrap()))).await.unwrap();
        crate::graphql::created_doc_id(&response, "AgentSessionInputEdit").unwrap()
    }

    async fn receipt(&self, id: &str) -> SessionInputEditReceipt {
        let value = ConfigAccess::Local(self.node.clone()).execute(&format!(r#"{{ AgentSessionInputEdit(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ receipt_json }} }}"#, escape_graphql_string(id))).await.unwrap();
        serde_json::from_str(
            value["data"]["AgentSessionInputEdit"][0]["receipt_json"]
                .as_str()
                .unwrap(),
        )
        .unwrap()
    }

    async fn requests(&self) -> serde_json::Value {
        ConfigAccess::Local(self.node.clone()).execute("{ AgentRequest { request_id content lifecycle_state superseded_by_request_doc_id } }").await.unwrap()["data"]["AgentRequest"].clone()
    }
}

#[tokio::test]
async fn generated_commands_bind_to_atomic_queue_owner() {
    let case = crate::lean_vocab_test::lean_session_input_edit_cases()
        .iter()
        .find(|case| case.name == "local_self_applied")
        .unwrap();
    assert!(
        case.authority.local_self && case.authority.requester_is_node && !case.command.has_peer
    );
    let fixture = Fixture::new().await;
    let command = fixture.command().await;
    let id = fixture.submit(&command).await;
    process_command(&fixture.node, &fixture.identity, &id)
        .await
        .unwrap();
    let receipt = fixture.receipt(&id).await;
    receipt.validate_for(&command).unwrap();
    assert!(fixture
        .identity
        .verify(
            &receipt.signer_did,
            &receipt.signing_payload(),
            &receipt.signature
        )
        .await
        .unwrap());
    assert_eq!(
        serde_json::to_value(receipt.outcome).unwrap(),
        case.receipt.as_ref().unwrap().outcome
    );
    let rows = fixture.requests().await;
    assert_eq!(rows.as_array().unwrap().len(), 2);
    assert_eq!(
        rows.as_array()
            .unwrap()
            .iter()
            .find(|row| row["request_id"] == "original")
            .unwrap()["lifecycle_state"],
        "superseded"
    );
    assert_eq!(
        rows.as_array()
            .unwrap()
            .iter()
            .find(|row| row["request_id"] == command.replacements[0].request_id)
            .unwrap()["content"],
        "corrected content"
    );
    process_command(&fixture.node, &fixture.identity, &id)
        .await
        .unwrap();
    assert_eq!(
        fixture.requests().await,
        rows,
        "verified replay must not apply twice"
    );
    assert_eq!(fixture.receipt(&id).await, receipt);
}

#[tokio::test]
async fn invalid_receipt_cannot_suppress_a_valid_edit_and_stale_edit_never_writes_replacement() {
    let fixture = Fixture::new().await;
    let first = fixture.command().await;
    let stale = fixture.command().await;
    let first_id = fixture.submit(&first).await;
    let stale_id = fixture.submit(&stale).await;
    ConfigAccess::Local(fixture.node.clone()).write("test.forged.receipt", &format!(r#"mutation {{ update_AgentSessionInputEdit(docID: "{}", input: {{ receipt_json: "{{}}" }}) {{ _docID }} }}"#, escape_graphql_string(&first_id))).await.unwrap();
    process_command(&fixture.node, &fixture.identity, &first_id)
        .await
        .unwrap();
    assert_eq!(
        fixture.receipt(&first_id).await.outcome,
        SessionInputEditOutcome::Applied
    );
    let before = fixture.requests().await;
    process_command(&fixture.node, &fixture.identity, &stale_id)
        .await
        .unwrap();
    let rejected = fixture.receipt(&stale_id).await;
    rejected.validate_for(&stale).unwrap();
    assert_eq!(rejected.outcome, SessionInputEditOutcome::Rejected);
    assert_eq!(
        fixture.requests().await,
        before,
        "losing replacement must not be created"
    );
}

#[tokio::test]
async fn valid_author_signature_cannot_install_replacement_with_wrong_admission_branch() {
    let fixture = Fixture::new().await;
    let mut command = fixture.command().await;
    command.replacements[0].admission = AgentRequestAdmissionRecord::peer(fixture.identity.did());
    crate::sign_agent_request_create(fixture.identity.as_ref(), &mut command.replacements[0])
        .await
        .unwrap();
    command.signature = fixture
        .identity
        .sign(&command.signing_payload())
        .await
        .unwrap();
    let id = fixture.submit(&command).await;
    let before = fixture.requests().await;
    process_command(&fixture.node, &fixture.identity, &id)
        .await
        .unwrap();
    let receipt = fixture.receipt(&id).await;
    receipt.validate_for(&command).unwrap();
    assert_eq!(receipt.outcome, SessionInputEditOutcome::Rejected);
    assert!(receipt.reason.contains("replacement admission"));
    assert_eq!(fixture.requests().await, before);
}

#[tokio::test]
async fn generated_scope_and_validity_rejections_leave_queue_unchanged() {
    for name in [
        "future_issuance_rejected",
        "foreign_session_rejected",
        "local_foreign_rejected",
    ] {
        let case = crate::lean_vocab_test::lean_session_input_edit_cases()
            .iter()
            .find(|case| case.name == name)
            .unwrap();
        let fixture = Fixture::new().await;
        let mut command = fixture.command().await;
        let foreign_key;
        let signer: &dyn NodeIdentity = if name == "local_foreign_rejected" {
            foreign_key =
                KeyIdentity::load_or_create(fixture._dir.path().join("foreign.key"), None).unwrap();
            command.requester_did = foreign_key.did().to_owned();
            &foreign_key
        } else {
            fixture.identity.as_ref()
        };
        if name == "future_issuance_rejected" {
            command.issued_at = (Utc::now() + chrono::Duration::minutes(5)).to_rfc3339();
            command.expires_at = (Utc::now() + chrono::Duration::minutes(10)).to_rfc3339();
        }
        if name == "foreign_session_rejected" {
            command.session_id = "another-session".into();
        }
        command.signature = signer.sign(&command.signing_payload()).await.unwrap();
        let id = fixture.submit(&command).await;
        let before = fixture.requests().await;
        process_command(&fixture.node, &fixture.identity, &id)
            .await
            .unwrap();
        let receipt = fixture.receipt(&id).await;
        receipt.validate_for(&command).unwrap();
        assert_eq!(
            serde_json::to_value(receipt.outcome).unwrap(),
            case.receipt.as_ref().unwrap().outcome,
            "{name}"
        );
        assert_eq!(fixture.requests().await, before, "{name}");
    }
}

#[tokio::test]
async fn edit_effect_and_receipt_rollback_together_after_a_write_failure() {
    let fixture = Fixture::new().await;
    let command = fixture.command().await;
    let id = fixture.submit(&command).await;
    let before = fixture.requests().await;
    let (result, mutations) = ConfigApplyTxn::with_successful_mutation_failure_at(
        Some(1),
        process_command(&fixture.node, &fixture.identity, &id),
    )
    .await;
    assert!(result.is_err());
    assert!(mutations > 0, "fault must interrupt an actual mutation");
    assert_eq!(fixture.requests().await, before);
    let row = ConfigAccess::Local(fixture.node.clone()).execute(&format!(r#"{{ AgentSessionInputEdit(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ receipt_json }} }}"#, escape_graphql_string(&id))).await.unwrap();
    assert!(row["data"]["AgentSessionInputEdit"][0]["receipt_json"].is_null());
    process_command(&fixture.node, &fixture.identity, &id)
        .await
        .unwrap();
    assert_eq!(
        fixture.receipt(&id).await.outcome,
        SessionInputEditOutcome::Applied
    );
}

#[tokio::test]
async fn generated_exact_replay_preserves_signed_prior_receipt_after_expiry() {
    let case = crate::lean_vocab_test::lean_session_input_edit_cases()
        .iter()
        .find(|case| case.name == "exact_replay_after_expiry")
        .unwrap();
    assert!(case.now >= case.command.expires_at);
    let fixture = Fixture::new().await;
    let mut command = fixture.command().await;
    command.issued_at = (Utc::now() - chrono::Duration::hours(2)).to_rfc3339();
    command.expires_at = (Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
    command.signature = fixture
        .identity
        .sign(&command.signing_payload())
        .await
        .unwrap();
    let id = fixture.submit(&command).await;
    let applied = ConfigAccess::Local(fixture.node.clone())
        .transact("test.prior.edit", |txn| {
            let command = &command;
            Box::pin(async move {
                let validated = crate::lifecycle::queue::validate_pending_user_edit_in_txn(
                    txn,
                    &command.node_did,
                    &command.session_id,
                    &command.requester_did,
                    &command.edit,
                    &command.replacements,
                )
                .await?
                .map_err(anyhow::Error::msg)?;
                crate::lifecycle::queue::apply_pending_user_edit_in_txn(txn, validated).await
            })
        })
        .await
        .unwrap();
    let mut receipt = SessionInputEditReceipt {
        version: SESSION_INPUT_EDIT_VERSION,
        command_id: command.command_id.clone(),
        command_digest: command.computed_digest(),
        requester_did: command.requester_did.clone(),
        node_did: command.node_did.clone(),
        session_id: command.session_id.clone(),
        outcome: SessionInputEditOutcome::Applied,
        reason: String::new(),
        request_doc_ids: applied.request_doc_ids,
        request_ids: applied.request_ids,
        processed_at: (Utc::now() - chrono::Duration::minutes(90)).to_rfc3339(),
        signer_did: fixture.identity.did().to_owned(),
        signature: Vec::new(),
    };
    receipt.signature = fixture
        .identity
        .sign(&receipt.signing_payload())
        .await
        .unwrap();
    receipt.validate_for(&command).unwrap();
    ConfigAccess::Local(fixture.node.clone()).write("test.prior.receipt", &format!(r#"mutation {{ update_AgentSessionInputEdit(docID: "{}", input: {{ receipt_json: "{}" }}) {{ _docID }} }}"#, escape_graphql_string(&id), escape_graphql_string(&serde_json::to_string(&receipt).unwrap()))).await.unwrap();
    let before = fixture.requests().await;
    process_command(&fixture.node, &fixture.identity, &id)
        .await
        .unwrap();
    assert_eq!(fixture.requests().await, before);
    assert_eq!(fixture.receipt(&id).await, receipt);
    assert_eq!(
        serde_json::to_value(receipt.outcome).unwrap(),
        case.receipt.as_ref().unwrap().outcome
    );
}

#[tokio::test]
async fn generated_command_identity_collision_cannot_replace_original_intent_or_effect() {
    let case = crate::lean_vocab_test::lean_session_input_edit_cases()
        .iter()
        .find(|case| case.name == "command_identity_collision")
        .unwrap();
    assert!(case.receipt.is_none());
    let fixture = Fixture::new().await;
    let command = fixture.command().await;
    let id = fixture.submit(&command).await;
    process_command(&fixture.node, &fixture.identity, &id)
        .await
        .unwrap();
    let receipt = fixture.receipt(&id).await;
    let before = fixture.requests().await;
    let mut colliding = command.clone();
    colliding.edit.messages[0].content = "conflicting replacement".into();
    colliding.signature = fixture
        .identity
        .sign(&colliding.signing_payload())
        .await
        .unwrap();
    assert_ne!(colliding.computed_digest(), command.computed_digest());
    let result = ConfigAccess::Local(fixture.node.clone()).write("test.command.collision", &format!(r#"mutation {{ create_AgentSessionInputEdit(input: {{ command_id: "{}", requester_did: "{}", node_did: "{}", session_id: "{}", intent_json: "{}" }}) {{ _docID }} }}"#, escape_graphql_string(&colliding.command_id), escape_graphql_string(&colliding.requester_did), escape_graphql_string(&colliding.node_did), escape_graphql_string(&colliding.session_id), escape_graphql_string(&serde_json::to_string(&colliding).unwrap()))).await;
    assert!(
        result.is_err(),
        "unique immutable command identity must reject conflicting intent"
    );
    process_command(&fixture.node, &fixture.identity, &id)
        .await
        .unwrap();
    assert_eq!(fixture.requests().await, before);
    assert_eq!(fixture.receipt(&id).await, receipt);
}

#[tokio::test]
async fn cryptographically_invalid_receipt_cannot_suppress_valid_edit() {
    let fixture = Fixture::new().await;
    let command = fixture.command().await;
    let id = fixture.submit(&command).await;
    let mut forged = SessionInputEditReceipt {
        version: SESSION_INPUT_EDIT_VERSION,
        command_id: command.command_id.clone(),
        command_digest: command.computed_digest(),
        requester_did: command.requester_did.clone(),
        node_did: command.node_did.clone(),
        session_id: command.session_id.clone(),
        outcome: SessionInputEditOutcome::Rejected,
        reason: "forged rejection".into(),
        request_doc_ids: vec![],
        request_ids: vec![],
        processed_at: Utc::now().to_rfc3339(),
        signer_did: fixture.identity.did().to_owned(),
        signature: vec![],
    };
    forged.signature = fixture
        .identity
        .sign(&forged.signing_payload())
        .await
        .unwrap();
    forged.signature[0] ^= 1;
    forged.validate_for(&command).unwrap();
    ConfigAccess::Local(fixture.node.clone()).write("test.invalid_signature_receipt", &format!(
        r#"mutation {{ update_AgentSessionInputEdit(docID: "{}", input: {{ receipt_json: "{}" }}) {{ _docID }} }}"#,
        escape_graphql_string(&id), escape_graphql_string(&serde_json::to_string(&forged).unwrap())
    )).await.unwrap();
    process_command(&fixture.node, &fixture.identity, &id)
        .await
        .unwrap();
    let actual = fixture.receipt(&id).await;
    actual.validate_for(&command).unwrap();
    assert_eq!(actual.outcome, SessionInputEditOutcome::Applied);
    assert!(fixture
        .identity
        .verify(
            &actual.signer_did,
            &actual.signing_payload(),
            &actual.signature
        )
        .await
        .unwrap());
    assert_eq!(fixture.requests().await.as_array().unwrap().len(), 2);
}
