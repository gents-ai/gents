use gents_session::test_support::{fixture, fixture_access, fixture_query};

#[tokio::test]
async fn post_commit_receipt_loss_replays_into_exact_committed_publication() {
    use gents_protocol::message::{AssistantContent, Message, ToolCall, ToolFunction};
    let (node, path, lifecycle, writer) = fixture("receipt-loss").await;
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
        id: Some("provider-message".into()),
        content: vec![AssistantContent::ToolCall(ToolCall {
            raw_arguments: None,
            id: "native-tool".into(),
            call_id: Some("provider-call".into()),
            function: ToolFunction::new("read".into(), serde_json::json!({"path":"文.txt"})),
            signature: Some("sig".into()),
            additional_params: None,
        })],
    };
    let (first, fault_fired) =
        gents_store::config_client::ConfigApplyTxn::with_post_commit_receipt_loss(
            writer.publish_native_turn(&lifecycle, 0, 0, &message),
        )
        .await;
    let first = first.expect("idempotent owner must reconstruct the lost receipt");
    assert!(fault_fired, "post-commit receipt-loss fault must fire");
    let request = gents_store::graphql::escape_graphql_string(&lifecycle.request().doc_id);
    let query = format!(
        r#"{{
            AgentMessage(filter: {{ request_doc_id: {{ _eq: "{request}" }} }}) {{ _docID sequence }}
            AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{request}" }} }}, order: {{ ordinal: ASC }}) {{ _docID ordinal close }}
            AgentToolCall(filter: {{ request_doc_id: {{ _eq: "{request}" }} }}) {{ _docID lifecycle_state }}
        }}"#
    );
    let before = fixture_query(&fixture_access(&node, &lifecycle), &query).await;
    let replay = writer
        .publish_native_turn(&lifecycle, 0, 0, &message)
        .await
        .expect("external replay resolves committed publication");
    let replayed = fixture_query(&fixture_access(&node, &lifecycle), &query).await;
    assert_eq!(
        replayed.data, before.data,
        "replay after a lost receipt must not add stored documents"
    );
    assert_eq!(first.message_doc_id, replay.message_doc_id);
    assert_eq!(first.sequence, replay.sequence);
    assert_eq!(first.accepted_tools.len(), replay.accepted_tools.len());
    assert_eq!(first.accepted_tools.len(), 1);
    let a = &first.accepted_tools[0];
    let b = &replay.accepted_tools[0];
    assert_eq!(a.tool_call_doc_id, b.tool_call_doc_id);
    assert_eq!(a.accepted_header_doc_id, b.accepted_header_doc_id);
    assert_eq!(a.arguments, b.arguments);
    assert_eq!(a.id, "native-tool");
    assert_eq!(a.call_id.as_deref(), Some("provider-call"));
    let rows = before.data.as_ref().unwrap();
    let message_rows = rows["AgentMessage"].as_array().unwrap();
    assert_eq!(message_rows.len(), 1, "exactly one committed header");
    assert_eq!(message_rows[0]["_docID"], first.message_doc_id);
    assert_eq!(message_rows[0]["sequence"], first.sequence);
    let segment_rows = rows["AgentOutputSegment"].as_array().unwrap();
    assert_eq!(segment_rows.len(), 1);
    assert_eq!(segment_rows[0]["_docID"], a.arguments.close_doc_id);
    assert!(
        !segment_rows[0]["close"].is_null(),
        "publication closes its delta"
    );
    let tool_rows = rows["AgentToolCall"].as_array().unwrap();
    assert_eq!(tool_rows.len(), 1);
    assert_eq!(tool_rows[0]["_docID"], a.tool_call_doc_id);
    assert_eq!(tool_rows[0]["lifecycle_state"], "pending");
    assert_eq!(a.accepted_header_doc_id, first.message_doc_id);

    let deadline = lifecycle.claimed_deadline_at().unwrap();
    let mut first_dispatch = crate::call::lifecycle::ToolCallLifecycle::from_accepted(
        node.clone(),
        lifecycle.request().node_did.clone(),
        lifecycle.request().requester_did.clone(),
        a.clone(),
        deadline,
        gents_protocol::tool_call::AwaitMode::Foreground,
    )
    .unwrap();
    let mut replay_dispatch = crate::call::lifecycle::ToolCallLifecycle::from_accepted(
        node.clone(),
        lifecycle.request().node_did.clone(),
        lifecycle.request().requester_did.clone(),
        b.clone(),
        deadline,
        gents_protocol::tool_call::AwaitMode::Foreground,
    )
    .unwrap();
    let (first_start, replay_start) = tokio::join!(
        first_dispatch.start_running(),
        replay_dispatch.start_running(),
    );
    assert_ne!(
        first_start.is_ok(),
        replay_start.is_ok(),
        "exactly one accepted handle must win pending-to-running dispatch: first={first_start:?}, replay={replay_start:?}"
    );
    let loser_error = match (&first_start, &replay_start) {
        (Ok(_), Err(error)) | (Err(error), _) => error,
        (Ok(_), Ok(_)) => unreachable!("exactly one handle wins, asserted above"),
    };
    assert!(
        format!("{loser_error:#}").contains("no longer pending"),
        "the losing handle must be rejected by the physical pending-row fence: {loser_error:#}"
    );
    let dispatched = fixture_query(&fixture_access(&node, &lifecycle), &query).await;
    let dispatched_tools = dispatched.data.as_ref().unwrap()["AgentToolCall"]
        .as_array()
        .unwrap();
    assert_eq!(dispatched_tools.len(), 1);
    assert_eq!(dispatched_tools[0]["_docID"], a.tool_call_doc_id);
    assert_eq!(dispatched_tools[0]["lifecycle_state"], "running");

    let winner = if first_start.is_ok() {
        &mut first_dispatch
    } else {
        &mut replay_dispatch
    };
    winner
        .complete("one receipt-loss dispatch result")
        .await
        .unwrap();
    let completed = fixture_query(&fixture_access(&node, &lifecycle), &query).await;
    let completed_tools = completed.data.as_ref().unwrap()["AgentToolCall"]
        .as_array()
        .unwrap();
    assert_eq!(completed_tools.len(), 1);
    assert_eq!(completed_tools[0]["_docID"], a.tool_call_doc_id);
    assert_eq!(completed_tools[0]["lifecycle_state"], "completed");
    let output =
        gents_session::session::tool_output_rows::observe_canonical_tool_output_with_access(
            &fixture_access(&node, &lifecycle),
            &a.tool_call_doc_id,
            &lifecycle.request().doc_id,
            &lifecycle.request().session_id,
            &lifecycle.request().node_did,
            lifecycle.request().requester_did.as_deref(),
        )
        .await
        .unwrap();
    assert_eq!(output.into_text(), "one receipt-loss dispatch result");
    node.shutdown().await.expect("shut down test node");
    drop(path);
}
