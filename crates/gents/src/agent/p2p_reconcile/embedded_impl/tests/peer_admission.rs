use super::fixtures::{
    authorize_enrollment_peer, collection_values, exec, runtime_test_node_with_identity,
    wait_for_active_peer, wait_for_peer_info, wait_for_value,
};
use super::peer_daemon::{
    behavior_daemon, load_admission_request_by_id, seed_peer_session_message, test_agent,
};
use super::*;

#[tokio::test]
async fn peer_request_final_admission_uses_the_requester_signature_and_fresh_authority() {
    let identity_temp = tempfile::tempdir().expect("identity tempdir");
    let coordinator_identity: Arc<dyn NodeIdentity> = Arc::new(
        KeyIdentity::load_or_create(identity_temp.path().join("coordinator.key"), None)
            .expect("coordinator identity"),
    );
    let host_identity: Arc<dyn NodeIdentity> = Arc::new(
        KeyIdentity::load_or_create(identity_temp.path().join("host.key"), None)
            .expect("host identity"),
    );
    let coordinator_test = runtime_test_node_with_identity(coordinator_identity.as_ref()).await;
    let host_test = runtime_test_node_with_identity(host_identity.as_ref()).await;
    let coordinator = Arc::clone(&coordinator_test.node);
    let host = Arc::clone(&host_test.node);
    let coordinator_admin = EmbeddedRemoteP2pAdmin::new(Arc::clone(&coordinator));
    let host_admin = EmbeddedRemoteP2pAdmin::new(Arc::clone(&host));
    let coordinator_addresses = wait_for_peer_info(&coordinator_admin).await;
    let host_addresses = wait_for_peer_info(&host_admin).await;

    let host_did = host_identity.did().to_string();
    let coordinator_did = coordinator_identity.did().to_string();
    let parent_request_id = "remote-parent-1";

    let parent_response = coordinator
        .execute(&format!(
            r#"mutation {{ create_AgentRequest(input: {{
                request_id: "{parent_request_id}", node_did: "{coordinator_did}",
                purpose: "normal",
                requester_did: "{coordinator_did}", agent_id: "parent-behavior",
                session_id: "remote-session", retry_root_request: "{parent_request_id}",
                content: "remote parent", lifecycle_state: "pending",
                execution_origin: "interactive", created_at: "2026-08-30T00:00:00Z",
                retry_count: 0, max_retries: 3, request_hop: 0
            }}) {{ _docID }} }}"#,
        ))
        .await;
    assert!(
        !parent_response.has_errors(),
        "seed parent: {:?}",
        parent_response.errors
    );
    let _parent_doc_id = parent_response
        .data
        .as_ref()
        .and_then(|data| {
            data.get("create_AgentRequest")
                .or_else(|| data.get("add_AgentRequest"))
        })
        .and_then(|value| {
            value.get("_docID").or_else(|| {
                value
                    .as_array()
                    .and_then(|rows| rows.first())
                    .and_then(|row| row.get("_docID"))
            })
        })
        .and_then(serde_json::Value::as_str)
        .expect("parent doc id")
        .to_string();
    let parent_request =
        crate::request::binding::load_agent_request(&coordinator, parent_request_id)
            .await
            .unwrap()
            .unwrap();
    let mut parent_lifecycle = crate::request::lifecycle::RequestLifecycle::new_with_node_did(
        coordinator.clone(),
        "parent-behavior",
        &coordinator_did,
        parent_request,
        120,
    );
    assert_eq!(
        parent_lifecycle.claim().await.unwrap(),
        crate::request::lifecycle::ClaimOutcome::Claimed
    );
    let parent_writer = crate::request::streaming::DefraStreamWriter::new(
        coordinator.clone(),
        &coordinator_did,
        Duration::ZERO,
    );
    parent_lifecycle
        .begin_owned_execution(&parent_writer)
        .await
        .unwrap();
    let fresh_request_id = seed_peer_session_message(
        &coordinator,
        &parent_lifecycle,
        &parent_writer,
        &host_did,
        "message-1",
        "delegated work",
        0,
    )
    .await;
    let revoked_request_id = seed_peer_session_message(
        &coordinator,
        &parent_lifecycle,
        &parent_writer,
        &host_did,
        "message-2",
        "delegated work after revocation",
        1,
    )
    .await;

    // The receiving node's own agent configures the request; the Peer
    // branch reads only the requester's ACP authority.
    let tools: crate::documents::Tools = serde_json::from_value(serde_json::json!({
        "tools_id":"host-tools","node_did":host_did
    }))
    .unwrap();
    let context: crate::documents::AgentContext = serde_json::from_value(serde_json::json!({
        "context_id":"host-context","node_did":host_did,"tools_id":"host-tools"
    }))
    .unwrap();
    let behavior: crate::documents::Agent = serde_json::from_value(serde_json::json!({
        "agent_id":"behavior-1","node_did":host_did,"context_id":"host-context",
        "inference_profile_id":"host-profile"
    }))
    .unwrap();
    for (collection, document) in [
        ("Tools", serde_json::to_value(tools).unwrap()),
        ("AgentContext", serde_json::to_value(context).unwrap()),
        ("Agent", serde_json::to_value(behavior).unwrap()),
    ] {
        let input = gents_protocol::graphql::graphql_input_literal(&document).unwrap();
        exec(
            &host,
            &format!("mutation {{create_{collection}(input:{input}){{_docID}}}}"),
            "seed target context policy",
        )
        .await;
    }

    let template =
        resolve_template(AGENT_TARGET_CALLER_TEMPLATE).expect("agent-target-caller template");
    let collections = template
        .collections
        .iter()
        .map(|collection| (*collection).to_string())
        .collect::<Vec<_>>();
    coordinator_admin
        .connect(&host_addresses)
        .await
        .expect("connect host");
    wait_for_active_peer(&coordinator_admin).await;
    wait_for_active_peer(&host_admin).await;
    let coordinator_p2p = coordinator.p2p().expect("coordinator P2P enabled");
    let coordinator_peer = coordinator_p2p
        .local_peer_id()
        .await
        .expect("coordinator peer id");
    let coordinator_ticket = coordinator_p2p
        .shareable_address()
        .await
        .expect("coordinator ticket lookup")
        .expect("coordinator shareable ticket");
    let enrollment_request_id = authorize_enrollment_peer(
        host.clone(),
        host_identity.clone(),
        coordinator_identity.clone(),
        &coordinator_peer,
        &coordinator_ticket,
    )
    .await;
    let (authority_owner, authority) = enrollment_authority_channel();
    let authority_cancel = CancellationToken::new();
    let authority_task = tokio::spawn(run_enrollment_reconciler(
        host.clone(),
        host_identity.clone(),
        authority_owner,
        authority_cancel.clone(),
    ));
    host_admin
        .add_replicator(
            &coordinator_addresses,
            &collections,
            &PairingFilters::default(),
        )
        .await
        .expect("authorize coordinator route");
    let filters = scope_filter(
        &template.scope,
        template.collections,
        &host_did,
        &coordinator_did,
    );
    coordinator_admin
        .add_replicator(&host_addresses, &collections, &filters)
        .await
        .expect("install addressed-request coordinator route");
    wait_for_value(&host, "AgentRequest", "request_id", &fresh_request_id).await;
    wait_for_value(&host, "AgentRequest", "request_id", &revoked_request_id).await;
    assert_eq!(
        collection_values(&host, "AgentRequest", "request_id").await,
        BTreeSet::from([fresh_request_id.clone(), revoked_request_id.clone()]),
        "only requests addressed to the host replicate; the caller's own request stays home"
    );
    assert!(
        collection_values(&host, "AgentToolCall", "tool_call_id")
            .await
            .is_empty(),
        "the caller's tool rows never replicate to the host"
    );

    // Adapted from Ivan's #3264: a remote interrupting message and its
    // scoped interrupt commit together on the caller's replica, and the
    // interrupt reaches the owning host.
    let active = load_admission_request_by_id(&host, &fresh_request_id).await;
    exec(
        &host,
        &format!(
            r#"mutation {{ update_AgentRequest(docID: "{}", input: {{ lifecycle_state: "processing" }}) {{ _docID }} }}"#,
            escape_graphql_string(&active.doc_id)
        ),
        "start remote interrupt fixture",
    )
    .await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while crate::request::interrupt::active_session_request(
        &coordinator,
        &active.session_id,
        &host_did,
        Some(&coordinator_did),
    )
    .await
    .unwrap()
    .and_then(|row| row.doc_id)
        != Some(active.doc_id.clone())
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the active request never replicated to the caller"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let mut followup =
        crate::tool_call_lifecycle::admission_fixture::publish_accepted_on_claimed_request(
            coordinator.clone(),
            &mut parent_lifecycle,
            &coordinator_did,
            2,
            crate::toolset::AGENT_MESSAGE_TOOL_NAME,
            "peer-interrupt-message",
            serde_json::json!({"session_id": active.session_id, "message": "change direction", "delivery": "interrupt"}),
            crate::tool_call_lifecycle::AwaitMode::Background,
            false,
        )
        .await
        .unwrap();
    let caller = parent_lifecycle.request();
    let cause = crate::request::lifecycle::SessionMessageCause {
        caller_node_did: caller.node_did.clone(),
        caller_agent_id: caller.agent_id.clone(),
        caller_session_id: caller.session_id.clone(),
        caller_requester_did: caller.requester_did.clone(),
        caller_request_id: caller.request_id.clone(),
        caller_request_doc_id: caller.doc_id.clone(),
        caller_hop: caller.request_hop,
        tool_call_id: followup.tool_call_id().into(),
        tool_call_doc_id: followup.doc_id().unwrap().into(),
        correlation: None,
    };
    let target = crate::request::lifecycle::SessionMessageTarget {
        node_did: host_did.clone(),
        agent_id: active.agent_id.clone(),
        session_id: active.session_id.clone(),
    };
    let plan = crate::session::message::plan_with_delivery(
        &coordinator,
        &cause,
        &target,
        crate::session::message::RenderedBody {
            content: "change direction".into(),
            goal: None,
        },
        None,
        crate::session::message::AgentMessageDelivery::Interrupt,
    )
    .await
    .unwrap()
    .unwrap();
    let requests_before = collection_values(&coordinator, "AgentRequest", "request_id").await;
    let aborted: anyhow::Result<()> = crate::config_client::ConfigAccess::transact_local_write(
        &coordinator,
        None,
        "test.abort_remote_message_interrupt",
        |txn| {
            let (cause, plan) = (&cause, &plan);
            Box::pin(async move {
                crate::session::message::stage_planned_write_in_txn(txn, cause, plan)
                    .await
                    .map_err(anyhow::Error::from)?;
                anyhow::bail!("abort staged message and interrupt")
            })
        },
    )
    .await;
    assert!(aborted.is_err());
    assert_eq!(
        collection_values(&coordinator, "AgentRequest", "request_id").await,
        requests_before,
        "an aborted interrupting message publishes no request"
    );
    assert!(
        crate::request::interrupt::fetch_interrupt_requested_at_by_doc_id(
            &coordinator,
            &active.doc_id
        )
        .await
        .unwrap()
        .is_none(),
        "an aborted interrupting message interrupts nothing"
    );
    let receipt = crate::session::message::commit(&coordinator, &cause, &mut followup, plan, true)
        .await
        .unwrap();
    assert!(
        crate::request::interrupt::fetch_interrupt_requested_at_by_doc_id(
            &coordinator,
            &active.doc_id
        )
        .await
        .unwrap()
        .is_some()
    );
    wait_for_value(&host, "AgentRequest", "request_id", &receipt.request_id).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while crate::request::interrupt::fetch_interrupt_requested_at_by_doc_id(&host, &active.doc_id)
        .await
        .unwrap()
        .is_none()
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the interrupt never replicated to the host"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let followup_on_host = load_admission_request_by_id(&host, &receipt.request_id).await;
    for (doc_id, state) in [
        (followup_on_host.doc_id.as_str(), "completed"),
        (active.doc_id.as_str(), "pending"),
    ] {
        exec(
            &host,
            &format!(
                r#"mutation {{ update_AgentRequest(docID: "{}", input: {{ lifecycle_state: "{state}", interrupt_requested_at: null }}) {{ _docID }} }}"#,
                escape_graphql_string(doc_id)
            ),
            "restore remote interrupt fixture",
        )
        .await;
    }

    let behavior = test_agent(host_identity.clone());
    let fresh_calls = Arc::new(AtomicUsize::new(0));
    let mut fresh_daemon = behavior_daemon(
        host.clone(),
        behavior.clone(),
        authority.clone(),
        fresh_calls.clone(),
    );
    let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let fresh = load_admission_request_by_id(&host, &fresh_request_id).await;
    fresh_daemon
        .process_request(fresh, shutdown_rx.clone())
        .await
        .unwrap();
    let fresh_state = host
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}) {{ lifecycle_state failure_reason error_message terminal_output }} }}"#,
            escape_graphql_string(&fresh_request_id),
        ))
        .await;
    assert!(
        fresh_calls.load(Ordering::SeqCst) > 0,
        "fresh Peer request did not reach the provider: {fresh_state:?}"
    );

    GraphqlEnrollmentStore::new(host.clone(), host_identity.clone())
        .revoke_request(&enrollment_request_id)
        .await
        .expect("durably revoke signed enrollment generation");
    let revoked_calls = Arc::new(AtomicUsize::new(0));
    let mut revoked_daemon =
        behavior_daemon(host.clone(), behavior, authority, revoked_calls.clone());
    let revoked = load_admission_request_by_id(&host, &revoked_request_id).await;
    revoked_daemon
        .process_request(revoked, shutdown_rx)
        .await
        .unwrap();
    assert_eq!(
        revoked_calls.load(Ordering::SeqCst),
        0,
        "revoked pending Peer request reached the provider"
    );
    let rejected = host
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}, limit: 1) {{
                lifecycle_state claimed_at
            }} }}"#,
            escape_graphql_string(&revoked_request_id),
        ))
        .await;
    let rejected = rejected
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(|rows| rows.as_array())
        .and_then(|rows| rows.first())
        .expect("terminal denied child");
    assert_eq!(
        rejected
            .get("lifecycle_state")
            .and_then(serde_json::Value::as_str),
        Some("failed")
    );
    assert!(rejected
        .get("claimed_at")
        .is_none_or(serde_json::Value::is_null));

    authority_cancel.cancel();
    authority_task
        .await
        .expect("join enrollment authority")
        .expect("enrollment authority shutdown");
    coordinator.shutdown().await;
    host.shutdown().await;
}
