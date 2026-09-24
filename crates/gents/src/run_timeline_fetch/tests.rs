use super::validation::{validate_optional_request_binding, validate_required_request_binding};
use super::*;

fn bindings() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("doc-root".to_string(), "req-root".to_string()),
        ("doc-child".to_string(), "req-child".to_string()),
    ])
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
    use crate::lifecycle::{ClaimOutcome, RequestLifecycle};
    use crate::streaming::DefraStreamWriter;
    use gents_protocol::message::{AssistantContent, Message, ToolCall, ToolFunction};
    use std::{sync::Arc, time::Duration};

    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    crate::schema::ensure_runtime_schemas(node.as_ref())
        .await
        .unwrap();
    let response = node
        .execute(
            r#"mutation {
                create_AgentSession(input: {
                    session_id: "pending-tool-session"
                    agent_did: "did:test:timeline"
                    behavior_id: "general"
                    created_at: "2026-09-22T00:00:00Z"
                }) { _docID }
                create_AgentRequest(input: {
                    request_id: "pending-tool-request"
                    purpose: "normal"
                    agent_did: "did:test:timeline"
                    behavior_id: "general"
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
                    subagent_depth: 0
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
            crate::watcher::AGENT_REQUEST_FIELDS
        ))
        .await;
    let request: gents_protocol::row::AgentRequestRow =
        crate::graphql::first_row(&request_response, "AgentRequest")
            .unwrap()
            .expect("pending fixture request");
    let mut lifecycle = RequestLifecycle::new_with_agent_did(
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
    let access = ConfigAccess::Local(node.clone());
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

    // Publication between the tool snapshot and immutable dependency reads
    // must not introduce a tool whose admission was absent from that snapshot.
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

    let rows = load_run_timeline_rows(&ConfigAccess::Local(node.clone()), "pending-tool-request")
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
