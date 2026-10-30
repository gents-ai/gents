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
    let node = std::sync::Arc::new(crate::test_support::node_builder().build().await.unwrap());
    crate::ensure_runtime_schemas(&node).await.unwrap();
    for sequence in 1..=3 {
        let key = format!("timeline-message-{sequence}");
        crate::session::import_history_observation(
            &node,
            "doc-timeline-shared",
            "session-timeline-shared",
            "did:test:timeline-shared",
            None,
            &format!("message {sequence}"),
            &crate::session::canonical_rows::authored_message_key("doc-timeline-shared", &key),
            sequence,
            None,
        )
        .await;
    }

    let access = ConfigAccess::local(node.clone());
    let (messages, scans) = crate::session::count_request_output_scans(
        super::event_loaders::resolve_timeline_messages_for_session(
            &access,
            "did:test:timeline-shared",
            "session-timeline-shared",
            None,
        ),
    )
    .await;
    assert_eq!(messages.unwrap().len(), 3);
    assert_eq!(
        scans, 1,
        "one timeline session pass scans a shared request once"
    );
    node.shutdown().await;
}

#[tokio::test]
async fn timeline_resolves_fork_origin_payloads_without_cross_request_membership() {
    let node = std::sync::Arc::new(crate::test_support::node_builder().build().await.unwrap());
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let owner = "did:test:timeline-fork";
    crate::test_support::install_test_agent(&node, owner, "timeline-fork").await;
    crate::session::create_session_with_agent_id(
        &node,
        "timeline-fork-source",
        owner,
        "timeline-fork",
    )
    .await
    .unwrap();
    for sequence in 1..=3u32 {
        crate::session::import_history_observation(
            &node,
            "doc-timeline-fork-shared",
            "timeline-fork-source",
            owner,
            None,
            &format!("message {sequence}"),
            &format!("timeline-fork:{sequence}"),
            sequence,
            None,
        )
        .await;
    }
    let outcome = crate::session::fork(
        &node,
        crate::session::ForkParams {
            source_session_id: "timeline-fork-source",
            boundary: crate::session::ForkBoundary::UserTurn(3),
            caller_node_did: owner,
            caller_requester_did: None,
            target_agent_id: None,
        },
    )
    .await
    .unwrap();

    let access = ConfigAccess::local(node.clone());
    let (messages, scans) = crate::session::count_request_output_scans(
        super::event_loaders::resolve_timeline_messages_for_session(
            &access,
            owner,
            &outcome.session_id,
            None,
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
    node.shutdown().await;
}

#[tokio::test]
async fn timeline_batches_request_local_events_and_restores_request_sequence_order() {
    let node = std::sync::Arc::new(crate::test_support::node_builder().build().await.unwrap());
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let response = node
        .execute(
            r#"mutation {
                seed_InferenceCall_1: create_InferenceCall(input: { call_id: "call-b-2", request_id: "req-b", request_doc_id: "doc-b", call_seq: 2, attempt: 0, call_state: "completed", call_kind: "chat" }) { _docID }
                seed_InferenceCall_2: create_InferenceCall(input: { call_id: "call-b-1", request_id: "req-b", request_doc_id: "doc-b", call_seq: 1, attempt: 0, call_state: "completed", call_kind: "chat" }) { _docID }
                seed_InferenceCall_3: create_InferenceCall(input: { call_id: "call-a-1", request_id: "req-a", request_doc_id: "doc-a", call_seq: 1, attempt: 0, call_state: "completed", call_kind: "chat" }) { _docID }
                seed_ProviderContextReduction_1: create_ProviderContextReduction(input: { reduction_key: "reduction-b-2", request_id: "req-b", request_doc_id: "doc-b", session_id: "s-1", reduction_index: 2, turn_index: 1, messages_compacted: 0, original_tokens: 0, compacted_tokens: 0 }) { _docID }
                seed_ProviderContextReduction_2: create_ProviderContextReduction(input: { reduction_key: "reduction-b-1", request_id: "req-b", request_doc_id: "doc-b", session_id: "s-1", reduction_index: 1, turn_index: 1, messages_compacted: 0, original_tokens: 0, compacted_tokens: 0 }) { _docID }
                seed_ProviderContextReduction_3: create_ProviderContextReduction(input: { reduction_key: "reduction-a-1", request_id: "req-a", request_doc_id: "doc-a", session_id: "s-1", reduction_index: 1, turn_index: 1, messages_compacted: 0, original_tokens: 0, compacted_tokens: 0 }) { _docID }
            }"#,
        )
        .await;
    assert!(
        !response.has_errors(),
        "seed request-local timeline rows: {:?}",
        response.errors
    );

    let access = ConfigAccess::local(node.clone());
    let request_doc_ids = vec!["doc-a".to_owned(), "doc-b".to_owned()];
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
    node.shutdown().await;
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
    use gents_protocol::message::{AssistantContent, Message, ToolCall, ToolFunction};
    use std::{sync::Arc, time::Duration};

    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    crate::store::schema::ensure_runtime_schemas(node.as_ref())
        .await
        .unwrap();
    let response = node
        .execute(
            r#"mutation {
                create_AgentSession(input: {
                    session_id: "pending-tool-session"
                    node_did: "did:test:timeline"
                    agent_id: "general"
                    created_at: "2026-09-22T00:00:00Z"
                }) { _docID }
                create_AgentRequest(input: {
                    request_id: "pending-tool-request"
                    purpose: "normal"
                    node_did: "did:test:timeline"
                    agent_id: "general"
                    session_id: "pending-tool-session"
                    retry_parent_request: ""
                    retry_root_request: "pending-tool-request"
                    superseded_by_request: ""
                    content: "publish an accepted tool"
                    lifecycle_state: "pending"
                    backend_id: ""
                    execution_origin: "interactive"
                    failure_reason: ""
                    created_at: "2026-09-22T00:00:00Z"
                    retry_count: 0
                    max_retries: 3
                    request_hop: 0
                }) { _docID }
            }"#,
        )
        .await;
    assert!(
        !response.has_errors(),
        "seed accepted pending fixture: {:?}",
        response.errors
    );
    let request_response = node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "pending-tool-request" }} }}) {{ {} }} }}"#,
            crate::request::watcher::AGENT_REQUEST_FIELDS
        ))
        .await;
    let request: gents_protocol::row::AgentRequestRow =
        crate::graphql::first_row(&request_response, "AgentRequest")
            .unwrap()
            .expect("pending fixture request");
    let mut lifecycle = RequestLifecycle::new_with_node_did(
        node.clone(),
        "general",
        "did:test:timeline",
        request.try_into().unwrap(),
        60,
    );
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    let writer = DefraStreamWriter::new(node.clone(), "did:test:timeline", Duration::ZERO);
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
        id: Some("pending-tool-provider-message".to_owned()),
        content: vec![AssistantContent::ToolCall(ToolCall {
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
    let access = ConfigAccess::local(node.clone());
    let earlier_tools = event_loaders::load_timeline_tool_observations_for_session(
        &access,
        "did:test:timeline",
        "pending-tool-session",
        None,
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
        "did:test:timeline",
        "pending-tool-session",
        None,
    )
    .await
    .unwrap();
    assert_eq!(later_messages.len(), 1);
    assert!(event_loaders::resolve_timeline_tool_observations(
        &access,
        "did:test:timeline",
        "pending-tool-session",
        None,
        earlier_tools,
        &later_messages,
    )
    .await
    .unwrap()
    .is_empty());

    let rows = load_run_timeline_rows(&ConfigAccess::local(node.clone()), "pending-tool-request")
        .await
        .expect("accepted pending call must remain timeline-readable");
    let tool = rows.tool_calls.first().expect("one accepted tool");
    assert_eq!(tool.tool_call_id, "pending-tool-native-id");
    assert_eq!(tool.lifecycle_state.as_deref(), Some("pending"));
    assert_eq!(tool.status, "pending");
    assert_eq!(tool.args, r#"{"path":"/tmp/pending-tool"}"#);
    assert!(tool.result.is_none());

    node.shutdown().await;
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
        tool_name: Some(crate::toolset::AGENT_NEW_TOOL_NAME.into()),
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
        crate::toolset::AGENT_NEW_TOOL_NAME,
        "bridge-second",
        second_arguments.clone(),
        AwaitMode::Background,
        true,
    )
    .await
    .expect("publish second accepted spawn");

    let first_doc = admitted.tool.doc_id().expect("first bridge").to_owned();
    let second_doc = second.doc_id().expect("second bridge").to_owned();
    let bridge = node
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ node_did session_id requester_did request_doc_id }} }}"#,
            escape_graphql_string(&second_doc)
        ))
        .await;
    let bridge = &bridge.data.expect("bridge scope")["AgentToolCall"][0];
    let text = |field: &str| bridge[field].as_str().map(str::to_owned);
    let (node_did, session_id) = (text("node_did").unwrap(), text("session_id").unwrap());
    let requester_did = text("requester_did");

    let unresolvable = TranscriptMessage {
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
    let variables =
        crate::session::canonical_rows::transcript_message_create_variables(&unresolvable).unwrap();
    ConfigAccess::transact_local_write(&node, None, "test.unrelated_message", |txn| {
        let variables = variables.clone();
        Box::pin(async move {
            txn.execute_with_variables(
                crate::session::canonical_rows::CREATE_AGENT_MESSAGE_MUTATION,
                &variables,
            )
            .await
            .map(|_| ())
        })
    })
    .await
    .expect("persist unrelated message");

    let access = ConfigAccess::local(node.clone());
    assert!(
        load_session_tool_calls(&access, &node_did, &session_id, requester_did.as_deref())
            .await
            .is_err(),
        "premise: the session-wide reader resolves every message in the session"
    );
    let accepted = load_accepted_tool_calls(
        &access,
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
        serde_json::json!({"agent": "general", "prompt": "work"})
    );
    assert_eq!(
        serde_json::from_str::<Value>(&accepted[&second_doc]).unwrap(),
        second_arguments
    );
    assert!(load_accepted_tool_arguments(
        &access,
        &node_did,
        "another-session",
        requester_did.as_deref(),
        &second_doc,
    )
    .await
    .expect("an out-of-scope document is absent, not an error")
    .is_empty());

    drop(owner);
    node.shutdown().await;
    std::fs::remove_dir_all(admitted.path).unwrap();
}
