use super::*;
use crate::eval::runner::embedded::home::EmbeddedHome;
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use gents_protocol::request_input::{QueueDelivery, QueuePolicy, QueueSource, RequestQueue};
use gents_protocol::session_input_edit::{PendingMessageEdit, PendingQueueEdit};
use tokio_util::sync::CancellationToken;

async fn temp_home() -> EmbeddedHome {
    let home = EmbeddedHome::create_temp("input-edit-recovery")
        .await
        .unwrap();
    let binding = crate::store::identity::provision_agent_identity(home.identity.as_ref()).unwrap();
    let access = client_access(&home, home.identity.as_ref());
    crate::test_support::bootstrap_signed_test_node(&access, home.did()).await;
    crate::test_support::install_test_agent_binding_with_access(
        home.did(),
        "General",
        &binding,
        &access,
    )
    .await;
    crate::test_support::set_test_default_agent_as(&access, home.did(), &binding.agent_did).await;
    home
}

async fn agent_for(home: &EmbeddedHome) -> Arc<dyn NodeIdentity> {
    let binding =
        crate::test_support::ensure_test_default_agent(&home.node, home.identity.did()).await;
    crate::store::identity::resolve_agent_identity(home.identity.as_ref(), &binding).unwrap()
}

async fn active_agents(
    home: &EmbeddedHome,
) -> tokio::sync::watch::Receiver<Arc<crate::runtime::snapshot::ActiveRuntimeSnapshot>> {
    let agent_identity = agent_for(home).await;
    let node = Arc::new(crate::store::identity::RuntimeNode {
        node_did: home.identity.did().into(),
        identity: home.identity.clone(),
        default_agent_did: agent_identity.did().into(),
        display_name: None,
        enabled: true,
    });
    let agent = Arc::new(crate::inference::resolved::ResolvedAgent {
        configuration: crate::inference::resolved::ResolvedConfiguration::Static,
        agent_did: agent_identity.did().into(),
        node: node.clone(),
        agent_identity,
        backend_id: None,
        backend_provider_kind:
            crate::inference::backends::provider::BackendProviderKind::OpenAiCompatible,
        openai_wire_api: crate::OpenAiWireApi::ChatCompletions,
        backend_endpoint: String::new(),
        backend_auth: crate::documents::BackendAuth::Unauthenticated,
        claude_platform: None,
        model_name: String::new(),
        profile_id: None,
        service_tier: None,
        resolved_reasoning_efforts: None,
        context_window: 8192,
        max_output_tokens: 1024,
        max_turns: 2,
        max_turns_provenance: crate::inference::resolved::MaxTurnsProvenance::Default,
        system_prompt: String::new(),
        tools: crate::tools::surface::AgentToolSurfaceConfig::meta_only(),
        compaction: None,
        compaction_inference: None,
        max_total_tokens: None,
        stream_batch_ms: 0,
        stream_liveness_timeout: std::time::Duration::from_secs(5),
        provider_idle_timeout: std::time::Duration::from_secs(5),
        deadline_duration: std::time::Duration::from_secs(30),
        completion_retry:
            crate::runtime::agent::completion_retry::CompletionRetryProfileFields::default(),
        sampling: crate::inference::resolved::SamplingConfig::default(),
        skills: Vec::new(),
        oauth_pool: Vec::new(),
    });
    let snapshot = crate::runtime::snapshot::ResolvedRuntimeSnapshot::from_parts(
        agent.agent_did.clone(),
        vec![agent],
        Default::default(),
        Default::default(),
    )
    .with_node(node)
    .with_local_did(home.identity.did().into())
    .activate(1, Default::default());
    let (_sender, receiver) = tokio::sync::watch::channel(Arc::new(snapshot));
    receiver
}

fn client_access(home: &EmbeddedHome, signer: &dyn NodeIdentity) -> ConfigAccess {
    ConfigAccess::Graphql(
        crate::store::config_client::GraphqlEndpoint::as_principal(
            home.graphql_endpoint().unwrap(),
            signer.did(),
        )
        .with_client_signing_capability(signer.client_signing_capability().unwrap())
        .unwrap()
        .with_client_signed_agent_creates(),
    )
}

async fn edit_command(home: &EmbeddedHome, issued_at: DateTime<Utc>) -> SessionInputEdit {
    let did = home.identity.did();
    let binding = crate::test_support::ensure_test_default_agent(&home.node, did).await;
    let access = client_access(home, home.identity.as_ref());
    let mut create = AgentRequestCreate::base(
        RequestPurpose::Normal,
        "original",
        did,
        did,
        &binding.agent_did,
        "edit-session",
        "original content",
        "interactive",
        "2026-10-10T00:00:00Z",
        AgentRequestAdmissionRecord::client(did),
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
    create.session_doc_id = Some(
        crate::request::lifecycle::reserve_goal_session(&access, &create)
            .await
            .unwrap(),
    );
    let agent_doc = access
        .transact_readonly("test.edit.agent", |txn| {
            let agent_did = &binding.agent_did;
            Box::pin(async move {
                let (doc, _) = crate::store::config_client::read_desired_state_record_in_txn(
                    txn,
                    crate::Collection::Agent,
                    did,
                    agent_did,
                )
                .await?
                .context("Agent missing")?;
                Ok::<_, anyhow::Error>(doc)
            })
        })
        .await
        .unwrap();
    create.agent_doc_id = Some(agent_doc);
    crate::sign_agent_request_create(home.identity.as_ref(), &mut create)
        .await
        .unwrap();
    crate::request::lifecycle::submit_client_request(&access, &create)
        .await
        .unwrap();
    let response = access
        .execute("{ AgentRequest { _docID request_id } }")
        .await
        .unwrap();
    let original = response["data"]["AgentRequest"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["request_id"] == "original")
        .unwrap()["_docID"]
        .as_str()
        .unwrap()
        .to_owned();
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
        AgentRequestAdmissionRecord::client(did),
        did,
        "edit-session",
        did,
        &edit,
    )
    .await
    .unwrap();
    let (session_doc_id, agent_did, agent_doc_id, replacement_evidence) =
        test_signed_replacement_fields(
            &access,
            home.identity.as_ref(),
            did,
            "edit-session",
            &replacements,
        )
        .await;
    let mut command = SessionInputEdit {
        version: SESSION_INPUT_EDIT_VERSION,
        command_id: uuid::Uuid::new_v4().to_string(),
        requester_did: did.to_owned(),
        requester_peer_id: None,
        node_did: did.to_owned(),
        session_id: "edit-session".into(),
        session_doc_id,
        agent_did,
        agent_doc_id,
        issued_at: issued_at.to_rfc3339(),
        expires_at: (issued_at + chrono::Duration::minutes(5)).to_rfc3339(),
        edit,
        replacements,
        replacement_evidence,
        signature: Vec::new(),
    };
    command.signature = home
        .identity
        .sign(&command.signing_payload())
        .await
        .unwrap();
    command
}

async fn submit(
    home: &EmbeddedHome,
    signer: &dyn NodeIdentity,
    command: &SessionInputEdit,
) -> String {
    let access = client_access(home, signer);
    let input = serde_json::json!({"command_id":command.command_id, "requester_did":command.requester_did,
        "requester_peer_id":command.requester_peer_id, "node_did":command.node_did,
        "session_id":command.session_id, "intent_json":serde_json::to_string(command).unwrap()});
    access.transact_write("test.edit.intent", |txn| {
        let input = &input;
        Box::pin(async move {
            let response = txn.execute_client_signed_create(
                "mutation($input: AgentSessionInputEditMutationInputArg!) { create_AgentSessionInputEdit(input: $input) { _docID } }",
                &serde_json::json!({"input":input}), gents_protocol::schemas::AGENT_SESSION_INPUT_EDIT,
            ).await?;
            let doc = crate::store::graphql::created_doc_id(&response, "AgentSessionInputEdit")?;
            txn.add_resource_relationship("AgentSessionInputEdit", &doc, "parent", "sessions", &command.session_doc_id).await?;
            txn.add_actor_relationship("AgentSessionInputEdit", &doc, "working_agent", &::identity::Did::new(&command.agent_did)?).await?;
            Ok::<_, anyhow::Error>(doc)
        })
    }).await.unwrap()
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
    let home = temp_home().await;
    let issued_at = Utc::now() + chrono::Duration::seconds(2);
    let command = edit_command(&home, issued_at).await;
    let id = submit(&home, home.identity.as_ref(), &command).await;
    let before = requests(&home.node).await;
    let deferred = process_command(
        &home.node,
        &home.identity,
        &id,
        agent_for(&home).await.as_ref(),
    )
    .await
    .unwrap();
    assert_eq!(
        deferred.map(|at| at.timestamp_millis()),
        Some(issued_at.timestamp_millis())
    );
    assert!(raw_receipt(&home.node, &id).await.is_none());
    assert_eq!(requests(&home.node).await, before);

    let cancel = CancellationToken::new();
    let worker = tokio::spawn(run_session_command_reconciler_with_agents(
        home.node.clone(),
        home.identity.clone(),
        Some(active_agents(&home).await),
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
    let home = temp_home().await;
    let command = edit_command(&home, Utc::now() - chrono::Duration::seconds(1)).await;
    let id = submit(&home, home.identity.as_ref(), &command).await;
    process_command(
        &home.node,
        &home.identity,
        &id,
        agent_for(&home).await.as_ref(),
    )
    .await
    .unwrap();
    let applied: SessionInputEditReceipt =
        serde_json::from_str(&raw_receipt(&home.node, &id).await.unwrap()).unwrap();
    assert_eq!(applied.outcome, SessionInputEditOutcome::Applied);
    let effects = requests(&home.node).await;
    for overwrite in ["{}".to_owned(), String::new()] {
        ConfigAccess::local(home.node.clone()).write("test.overwrite.receipt", &format!(r#"mutation {{ update_AgentSessionInputEdit(docID: "{}", input: {{ receipt_json: "{}" }}) {{ _docID }} }}"#,
            escape_graphql_string(&id), escape_graphql_string(&overwrite))).await.unwrap();
        process_command(
            &home.node,
            &home.identity,
            &id,
            agent_for(&home).await.as_ref(),
        )
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
async fn duplicate_command_id_after_apply_preserves_receipt_and_effects() {
    let home = temp_home().await;
    let command = edit_command(&home, Utc::now() - chrono::Duration::seconds(1)).await;
    let prior = submit(&home, home.identity.as_ref(), &command).await;
    process_command(
        &home.node,
        &home.identity,
        &prior,
        agent_for(&home).await.as_ref(),
    )
    .await
    .unwrap();
    let receipt = raw_receipt(&home.node, &prior).await.unwrap();
    let effects = requests(&home.node).await;
    let mut twin = command.clone();
    twin.issued_at = (DateTime::parse_from_rfc3339(&command.issued_at).unwrap()
        - chrono::Duration::seconds(1))
    .to_rfc3339();
    twin.signature = home.identity.sign(&twin.signing_payload()).await.unwrap();
    let access = client_access(&home, home.identity.as_ref());
    assert!(
        crate::request::lifecycle::queue::publish_edit_command(&access, &twin)
            .await
            .is_err()
    );
    assert_eq!(raw_receipt(&home.node, &prior).await.unwrap(), receipt);
    assert_eq!(requests(&home.node).await, effects);
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
    let worker = tokio::spawn(run_session_command_reconciler_with_agents(
        home.node.clone(),
        home.identity.clone(),
        Some(active_agents(home).await),
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
    let home = temp_home().await;
    let command = edit_command(&home, Utc::now() - chrono::Duration::seconds(1)).await;
    let id = submit(&home, home.identity.as_ref(), &command).await;
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
    let home = temp_home().await;
    let issued_at = Utc::now() + chrono::Duration::seconds(2);
    let command = edit_command(&home, issued_at).await;
    let id = submit(&home, home.identity.as_ref(), &command).await;
    overwrite_receipt(&home.node, &id, "{}").await;

    let receipt = reconcile_until_verified(&home, &command, &id).await;
    assert!(Utc::now() >= issued_at);
    assert_eq!(receipt.outcome, SessionInputEditOutcome::Applied);
}
