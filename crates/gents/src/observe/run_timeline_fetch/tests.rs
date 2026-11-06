use super::validation::{validate_optional_request_binding, validate_required_request_binding};
use super::*;

fn bindings() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("doc-root".to_string(), "req-root".to_string()),
        ("doc-child".to_string(), "req-child".to_string()),
    ])
}

#[tokio::test]
async fn timeline_canonical_messages_share_one_request_payload_scan() {
    use crate::store::identity::NodeIdentity;
    let (db, user, access, binding) =
        crate::test_support::signed_test_node("timeline-shared-scan").await;
    let messages = (1..=3)
        .map(|sequence| format!("message {sequence}"))
        .collect::<Vec<_>>();
    crate::session::publish_shared_signed_history(
        &db,
        &user,
        &access,
        &binding,
        "session-timeline-shared",
        &messages,
    )
    .await;
    let (messages, scans) = crate::session::count_request_output_scans(
        super::event_loaders::resolve_timeline_messages_for_session(
            &access,
            db.node_identity.did(),
            "session-timeline-shared",
            Some(user.did()),
        ),
    )
    .await;
    assert_eq!(messages.unwrap().len(), 3);
    assert_eq!(
        scans, 1,
        "one timeline session pass scans a shared request once"
    );
    db.node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn timeline_resolves_fork_origin_payloads_without_cross_request_membership() {
    use crate::store::identity::NodeIdentity;
    let (db, user, access, binding) = crate::test_support::signed_test_node("timeline-fork").await;
    let node_did = db.node_identity.did();
    let messages = (1..=3)
        .map(|sequence| format!("message {sequence}"))
        .collect::<Vec<_>>();
    crate::session::publish_signed_history(
        &db,
        &user,
        &access,
        &binding,
        "timeline-fork-source",
        &messages,
    )
    .await;
    let outcome = crate::session::fork_on(
        &access,
        crate::session::ForkParams {
            source_session_id: "timeline-fork-source",
            boundary: crate::session::ForkBoundary::UserTurn(3),
            caller_node_did: node_did,
            caller_requester_did: Some(user.did()),
            target_agent_did: None,
        },
    )
    .await
    .unwrap();
    let (messages, scans) = crate::session::count_request_output_scans(
        super::event_loaders::resolve_timeline_messages_for_session(
            &access,
            node_did,
            &outcome.session_id,
            Some(user.did()),
        ),
    )
    .await;
    let messages = messages.unwrap();
    assert_eq!(messages.len(), 3);
    assert!(messages.iter().all(|row| row.request_doc_id.is_none()));
    assert_eq!(
        messages.iter().map(|row| row.sequence).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert_eq!(
        scans, 3,
        "each unbound fork header resolves its own origin request"
    );
    db.node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn timeline_batches_request_local_events_and_restores_request_sequence_order() {
    use crate::store::identity::NodeIdentity;
    let (db, user, access, binding) = crate::test_support::signed_test_node("timeline-batch").await;
    let agent = crate::store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding)
        .unwrap();
    let agent_access = ConfigAccess::local_managed_agent(
        db.node.clone(),
        identity::Did::new(agent.did()).unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    let session_id = "timeline-batch-session";
    crate::session::publish_signed_history(
        &db,
        &user,
        &access,
        &binding,
        session_id,
        &["a".into(), "b".into()],
    )
    .await;
    let session_response = agent_access.execute(r#"{AgentSession(filter: {session_id: {_eq: "timeline-batch-session"}}, limit: 2) {_docID}}"#).await.unwrap();
    let [session] = session_response["data"]["AgentSession"]
        .as_array()
        .unwrap()
        .as_slice()
    else {
        panic!("fixture session must be unique")
    };
    let session_doc = session["_docID"].as_str().unwrap().to_owned();
    let mut request_doc_ids = Vec::new();
    for index in 0..2 {
        let id = format!("{session_id}-request-{index}");
        let response = agent_access
            .execute(&format!(
                r#"{{AgentRequest(filter: {{request_id: {{_eq: "{}"}}}}, limit: 2) {{_docID}}}}"#,
                escape_graphql_string(&id)
            ))
            .await
            .unwrap();
        let [row] = response["data"]["AgentRequest"]
            .as_array()
            .unwrap()
            .as_slice()
        else {
            panic!("fixture request must be unique")
        };
        request_doc_ids.push(row["_docID"].as_str().unwrap().to_owned());
    }
    for (slot, seq, label) in [(1, 2, "b"), (1, 1, "b"), (0, 1, "a")] {
        let request_id = format!("{session_id}-request-{slot}");
        let call = serde_json::json!({
            "call_id": format!("call-{label}-{seq}"), "request_id": request_id,
            "request_doc_id": request_doc_ids[slot], "session_id": session_id,
            "node_did": db.node_identity.did(), "agent_did": binding.agent_did,
            "call_seq": seq, "attempt": 0, "call_state": "completed", "call_kind": "chat",
        });
        let reduction = serde_json::json!({
            "reduction_key": format!("reduction-{label}-{seq}"), "request_id": request_id,
            "request_doc_id": request_doc_ids[slot], "session_id": session_id,
            "session_doc_id": session_doc, "node_did": db.node_identity.did(),
            "requester_did": user.did(), "agent_did": binding.agent_did,
            "reduction_index": seq, "turn_index": 1,
            "messages_compacted": 0, "original_tokens": 0, "compacted_tokens": 0,
        });
        agent_access.transact_write("test.timeline.batch_rows", |txn| {
            let call = &call; let reduction = &reduction; let session_doc = &session_doc;
            Box::pin(async move {
                for (collection, mutation, input) in [
                    ("InferenceCall", "mutation($input: InferenceCallMutationInputArg!) { create_InferenceCall(input: $input) { _docID } }", call),
                    ("ProviderContextReduction", "mutation($input: ProviderContextReductionMutationInputArg!) { create_ProviderContextReduction(input: $input) { _docID } }", reduction),
                ] {
                    let response = txn.execute_with_variables(mutation, &serde_json::json!({"input": input})).await?;
                    let doc = crate::store::graphql::created_doc_id(&response, collection)?;
                    txn.add_resource_relationship(collection, &doc, "parent", "sessions", session_doc).await?;
                }
                Ok::<_, anyhow::Error>(())
            })
        }).await.unwrap();
    }
    let inference_calls =
        super::event_loaders::load_timeline_inference_calls_for_requests(&access, &request_doc_ids)
            .await
            .unwrap();
    assert_eq!(
        inference_calls
            .iter()
            .map(|row| row.call_id.as_str())
            .collect::<Vec<_>>(),
        vec!["call-a-1", "call-b-1", "call-b-2"]
    );
    let reductions = super::event_loaders::load_timeline_provider_context_reductions_for_requests(
        &access,
        &request_doc_ids,
    )
    .await
    .unwrap();
    assert_eq!(
        reductions
            .iter()
            .map(|row| row.reduction_key.as_str())
            .collect::<Vec<_>>(),
        vec!["reduction-a-1", "reduction-b-1", "reduction-b-2"]
    );
    db.node.shutdown().await.expect("node shutdown");
}

#[test]
fn physical_request_edge_rejects_forged_logical_join() {
    let error = validate_required_request_binding(
        &bindings(),
        "ProviderContextReduction",
        "reduction-1",
        "req-root",
        Some("doc-child"),
    )
    .expect_err("mismatched physical edge must fail closed");
    assert!(error.to_string().contains("belongs to req-child"));
}

#[test]
fn genuinely_unbound_context_row_is_permitted_but_half_binding_is_not() {
    validate_optional_request_binding(&bindings(), "AgentMessage", "message-context", None, None)
        .expect("unbound context message");
    let error = validate_optional_request_binding(
        &bindings(),
        "AgentMessage",
        "message-forged",
        Some("req-root"),
        None,
    )
    .expect_err("partial binding must fail closed");
    assert!(error.to_string().contains("incomplete request lineage"));
}

#[test]
fn rendered_request_requires_the_same_physical_binding_as_other_timeline_rows() {
    let rendered = TimelineRenderedRequestRow {
        doc_id: Some("rendered-1".to_string()),
        capture_key: "capture-1".to_string(),
        request_id: Some("req-root".to_string()),
        request_doc_id: Some("doc-child".to_string()),
        ..Default::default()
    };
    let error = validate_request_scoped_rows(&bindings(), &[], &[], &[], &[], &[], &[rendered])
        .expect_err("rendered request must not forge a logical/physical request pair");
    assert!(error.to_string().contains("RenderedRequest rendered-1"));
}

#[test]
fn session_rows_for_nested_requests_are_out_of_scope_without_hiding_forged_root_edges() {
    let bindings = bindings();
    assert!(!request_scoped_row_is_in_timeline(
        &bindings,
        Some("req-grandchild"),
        Some("doc-grandchild")
    ));
    assert!(request_scoped_row_is_in_timeline(
        &bindings,
        Some("req-root"),
        Some("doc-grandchild")
    ));
    assert!(!request_scoped_row_is_in_timeline(
        &bindings,
        Some("req-root"),
        None
    ));
    assert!(request_scoped_row_is_in_timeline(
        &bindings,
        None,
        Some("doc-root")
    ));
    assert!(!request_scoped_row_is_in_timeline(
        &bindings,
        Some("req-grandchild"),
        None
    ));
    assert!(request_scoped_row_is_in_timeline(&bindings, None, None));
}

#[test]
fn child_bridge_requires_the_exact_parent_tool_document() {
    let root = TimelineRequestRow {
        doc_id: Some("doc-root".to_string()),
        request_id: "req-root".to_string(),
        ..Default::default()
    };
    let child = TimelineRequestRow {
        doc_id: Some("doc-child".to_string()),
        request_id: "req-child".to_string(),
        caused_by_parent_request_id: Some("req-root".to_string()),
        caused_by_parent_request_doc_id: Some("doc-root".to_string()),
        caused_by_parent_tool_call_id: Some("call-parent".to_string()),
        caused_by_parent_tool_call_doc_id: Some("doc-forged-tool".to_string()),
        ..Default::default()
    };
    let tool = TimelineToolCallRow {
        doc_id: Some("doc-real-tool".to_string()),
        request_id: Some("req-root".to_string()),
        request_doc_id: Some("doc-root".to_string()),
        tool_call_id: "call-parent".to_string(),
        child_request_id: Some("req-child".to_string()),
        ..Default::default()
    };

    let error = validate_child_tool_bridges(&root, &[root.clone(), child], &[tool])
        .expect_err("forged tool document edge must fail closed");
    assert!(error.to_string().contains("missing AgentToolCall"));
}

#[tokio::test]
async fn accepted_pending_tool_uses_authoritative_lifecycle_state_when_status_is_null() {
    use crate::request::lifecycle::{ClaimOutcome, RequestLifecycle};
    use crate::request::streaming::DefraStreamWriter;
    use crate::store::identity::NodeIdentity;
    use gents_protocol::message::{AssistantContent, Message, ToolCall, ToolFunction};
    use std::time::Duration;

    let (db, user, access, binding) =
        crate::test_support::signed_test_node("timeline-pending-tool").await;
    let node = db.node.clone();
    let node_did = db.node_identity.did().to_owned();
    let agent = crate::store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding)
        .unwrap();
    let create = crate::request::lifecycle::build_signed_client_request(
        &access,
        crate::request::lifecycle::RequestSpec::new(
            gents_protocol::request_admission::RequestPurpose::Normal,
            crate::request::lifecycle::RequestIdentity {
                request_id: "pending-tool-request".into(),
                node_did: node_did.clone(),
                requester_did: Some(user.did().to_owned()),
                agent_did: binding.agent_did.clone(),
                session_id: "pending-tool-session".into(),
                content: "publish an accepted tool".into(),
                execution_origin: crate::request::lifecycle::ExecutionOrigin::Interactive,
                created_at: "2026-09-22T00:00:00Z".into(),
            },
            gents_protocol::request_admission::AgentRequestAdmissionRecord::client(user.did()),
        ),
        crate::request::lifecycle::RequestSigner::Identity(&user),
    )
    .await
    .unwrap();
    crate::request::lifecycle::submit_client_request(&access, &create)
        .await
        .unwrap();
    let agent_access = ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(agent.did()).unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    let request_response = agent_access
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "pending-tool-request" }} }}) {{ {} }} }}"#,
            crate::request::watcher::AGENT_REQUEST_FIELDS
        ))
        .await
        .unwrap();
    let request: gents_protocol::row::AgentRequestRow =
        serde_json::from_value(request_response["data"]["AgentRequest"][0].clone()).unwrap();
    let request = request.try_into().unwrap();
    let (_owner, authority) = crate::runtime::agent::p2p_reconcile::enrollment_authority_channel();
    let verifier = crate::request::admission::AgentRequestAdmissionVerifier::new(
        node.clone(),
        db.node_identity.clone(),
        authority,
    );
    let (request, admission) = verifier
        .verify_fresh_for_claim_with_identity(&request, agent.as_ref())
        .await
        .unwrap();
    assert!(
        admission.is_some(),
        "client claim must retain its verified admission"
    );
    let mut lifecycle =
        RequestLifecycle::new_with_node_did(node.clone(), agent.did(), &node_did, request, 60);
    lifecycle.bind_managed_agent(agent.as_ref()).unwrap();
    lifecycle.set_client_claim_admission(admission);
    assert_eq!(
        lifecycle.claim_with_identity().await.unwrap(),
        ClaimOutcome::Claimed
    );
    let writer =
        DefraStreamWriter::new_for_lifecycle(node.clone(), &lifecycle, Duration::ZERO).unwrap();
    lifecycle.begin_owned_execution(&writer).await.unwrap();
    writer
        .start_provider_attempt(
            &lifecycle.request().doc_id,
            0,
            0,
            "inference.1".parse().unwrap(),
        )
        .await;
    let message = Message::Assistant {
        request_id: None,
        id: Some("pending-tool-provider-message".to_owned()),
        content: vec![AssistantContent::ToolCall(ToolCall {
            raw_arguments: None,
            id: "pending-tool-native-id".to_owned(),
            call_id: Some("pending-tool-runtime-id".to_owned()),
            function: ToolFunction::new(
                "read_file".to_owned(),
                serde_json::json!({"path":"/tmp/pending-tool"}),
            ),
            signature: None,
            additional_params: None,
        })],
    };
    let earlier_tools = event_loaders::load_timeline_tool_observations_for_session(
        &access,
        &node_did,
        "pending-tool-session",
        Some(user.did()),
    )
    .await
    .unwrap();
    assert!(earlier_tools.is_empty());
    let published = writer
        .publish_native_turn(&lifecycle, 0, 0, &message)
        .await
        .expect("canonical accepted tool publication");
    assert_eq!(published.accepted_tools.len(), 1);

    let later_messages = resolve_timeline_messages_for_session(
        &access,
        &node_did,
        "pending-tool-session",
        Some(user.did()),
    )
    .await
    .unwrap();
    assert_eq!(later_messages.len(), 1);
    assert!(event_loaders::resolve_timeline_tool_observations(
        &access,
        &node_did,
        "pending-tool-session",
        Some(user.did()),
        earlier_tools,
        &later_messages,
    )
    .await
    .unwrap()
    .is_empty());

    let rows = load_run_timeline_rows(&access, "pending-tool-request")
        .await
        .expect("accepted pending call must remain timeline-readable");
    let tool = rows.tool_calls.first().expect("one accepted tool");
    assert_eq!(tool.tool_call_id, "pending-tool-native-id");
    assert_eq!(tool.lifecycle_state.as_deref(), Some("pending"));
    assert_eq!(tool.status, "pending");
    assert_eq!(tool.args, r#"{"path":"/tmp/pending-tool"}"#);
    assert!(tool.result.is_none());

    node.shutdown().await.expect("node shutdown");
}

#[test]
fn direct_child_without_tool_lineage_is_valid_but_half_bridge_is_rejected() {
    let root = TimelineRequestRow {
        doc_id: Some("doc-root".to_string()),
        request_id: "req-root".to_string(),
        ..Default::default()
    };
    let direct_child = TimelineRequestRow {
        doc_id: Some("doc-direct".to_string()),
        request_id: "req-direct".to_string(),
        caused_by_parent_request_id: Some("req-root".to_string()),
        caused_by_parent_request_doc_id: Some("doc-root".to_string()),
        ..Default::default()
    };
    validate_child_tool_bridges(&root, &[root.clone(), direct_child], &[])
        .expect("direct parent lineage does not fabricate a tool delegation");

    let half_bridge = TimelineRequestRow {
        doc_id: Some("doc-half".to_string()),
        request_id: "req-half".to_string(),
        caused_by_parent_request_id: Some("req-root".to_string()),
        caused_by_parent_request_doc_id: Some("doc-root".to_string()),
        caused_by_parent_tool_call_id: Some("call-only".to_string()),
        ..Default::default()
    };
    let error = validate_child_tool_bridges(&root, &[root.clone(), half_bridge], &[])
        .expect_err("half tool bridge must fail closed");
    assert!(error.to_string().contains("incomplete parent tool lineage"));
}

#[tokio::test]
async fn accepted_tool_arguments_read_only_the_accepted_message() {
    use crate::tools::call::lifecycle::admission_fixture::{
        publish_accepted_on_claimed_request, published_admission_with_owner,
        PublishedAdmissionOptions,
    };
    use crate::tools::call::lifecycle::AwaitMode;
    use gents_protocol::output::{
        MessageBlock, MessagePublication, MessageRole, OutputOutcome, PayloadRef, TranscriptMessage,
    };

    let (admitted, mut owner) = published_admission_with_owner(PublishedAdmissionOptions {
        name: "accepted-arguments-exact".into(),
        real_identity: true,
        await_mode: AwaitMode::Background,
        tool_name: Some(crate::tools::native::AGENT_NEW_TOOL_NAME.into()),
        ..Default::default()
    })
    .await
    .expect("publish first accepted spawn");
    let node = admitted.node.clone();
    let second_arguments = serde_json::json!({"agent": "child", "prompt": "second"});
    let second = publish_accepted_on_claimed_request(
        node.clone(),
        &mut owner,
        &admitted.node_did,
        1,
        crate::tools::native::AGENT_NEW_TOOL_NAME,
        "bridge-second",
        second_arguments.clone(),
        AwaitMode::Background,
        true,
    )
    .await
    .expect("publish second accepted spawn");

    let first_doc = admitted.tool.doc_id().expect("first bridge").to_owned();
    let second_doc = second.doc_id().expect("second bridge").to_owned();
    let bridge = admitted.client
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ node_did session_id requester_did request_doc_id }} }}"#,
            escape_graphql_string(&second_doc)
        ))
        .await
        .unwrap();
    let bridge = &bridge["data"]["AgentToolCall"][0];
    let text = |field: &str| bridge[field].as_str().map(str::to_owned);
    let (node_did, session_id) = (text("node_did").unwrap(), text("session_id").unwrap());
    let requester_did = text("requester_did");

    let unresolvable = TranscriptMessage {
        native_request_id: None,
        message_key: "unrelated-unresolvable".into(),
        session_id: session_id.clone(),
        node_did: node_did.clone(),
        requester_did: requester_did.clone(),
        request_doc_id: text("request_doc_id"),
        publication: MessagePublication::RequestExecution {
            execution_generation: "unrelated".into(),
        },
        outcome: OutputOutcome::Complete,
        sequence: 10_000,
        role: MessageRole::Assistant,
        native_id: None,
        created_at: "2026-09-25T00:00:00Z".into(),
        blocks: vec![MessageBlock::ToolCall {
            tool_call_doc_id: "unrelated-tool".into(),
            id: "unrelated-call".into(),
            call_id: None,
            name: "bash".into(),
            arguments: PayloadRef {
                close_doc_id: "missing-close".into(),
                stream: 0,
            },
            signature: None,
            additional_params: None,
        }],
    };
    let signer = owner.managed_agent_signer().expect("claimed Agent signer");
    let agent_access = ConfigAccess::local_with_agent_signer(
        node.clone(),
        identity::Did::new(&owner.request().agent_did).unwrap(),
        signer,
    )
    .unwrap();
    agent_access
        .transact_write("test.unrelated_message", |txn| {
            let unresolvable = &unresolvable;
            Box::pin(async move {
                let variables = crate::session::canonical_rows::transcript_message_bound_variables(
                    txn,
                    unresolvable,
                )
                .await?;
                let session_doc = variables["input"]["session_doc_id"].as_str().unwrap();
                let response = txn
                    .execute_with_variables(
                        crate::session::canonical_rows::CREATE_AGENT_MESSAGE_MUTATION,
                        &variables,
                    )
                    .await?;
                let doc = crate::store::graphql::created_doc_id(&response, "AgentMessage")?;
                txn.add_resource_relationship(
                    "AgentMessage",
                    &doc,
                    "parent",
                    "sessions",
                    session_doc,
                )
                .await?;
                Ok::<_, anyhow::Error>(())
            })
        })
        .await
        .expect("persist unrelated message");

    let access = &admitted.client;
    assert!(
        load_session_tool_calls(access, &node_did, &session_id, requester_did.as_deref())
            .await
            .is_err(),
        "premise: the session-wide reader resolves every message in the session"
    );
    let accepted = load_accepted_tool_calls(
        access,
        &node_did,
        &session_id,
        requester_did.as_deref(),
        &[first_doc.clone(), second_doc.clone()],
    )
    .await
    .expect("selected bindings read only their accepted messages");
    assert_eq!(accepted.len(), 2);
    let accepted = accepted
        .into_iter()
        .map(|call| (call.doc_id.unwrap(), call.args))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        serde_json::from_str::<Value>(&accepted[&first_doc]).unwrap(),
        serde_json::json!({"agent": owner.request().agent_did, "prompt": "work"})
    );
    assert_eq!(
        serde_json::from_str::<Value>(&accepted[&second_doc]).unwrap(),
        second_arguments
    );
    assert!(load_accepted_tool_arguments(
        access,
        &node_did,
        "another-session",
        requester_did.as_deref(),
        &second_doc,
    )
    .await
    .expect("an out-of-scope document is absent, not an error")
    .is_empty());

    drop(owner);
    node.shutdown().await.expect("node shutdown");
    std::fs::remove_dir_all(admitted.path).unwrap();
}
