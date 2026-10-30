use super::*;

#[tokio::test]
async fn sessions_get_reports_timeline_tools_usage_and_compactions() {
    let node = seeded_node().await;
    let tool = SessionHistoryTool::new(
        node,
        "did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7",
    );

    let parsed = load_session_investigation(&tool.node, &tool.node_did, "session-a")
        .await
        .unwrap();

    assert_eq!(parsed.session_id, "session-a");
    assert_eq!(parsed.requests.len(), 2);
    assert_eq!(parsed.tool_calls.total, 1);
    assert_eq!(parsed.tool_calls.by_tool.get("read_file"), Some(&1));
    assert_eq!(parsed.token_usage.model_calls, 1);
    assert_eq!(parsed.token_usage.input_tokens, Some(100));
    assert_eq!(parsed.token_usage.cached_input_tokens, Some(40));
    assert_eq!(parsed.token_usage.fresh_input_tokens, Some(60));
    assert_eq!(parsed.token_usage.output_tokens, Some(20));
    assert!(!parsed.token_usage.incomplete);
    assert_eq!(parsed.compactions.len(), 1);
    assert_eq!(parsed.compactions[0].scope, "session_prefix");
    assert_eq!(
        parsed.compaction_strategy,
        crate::compaction::CompactionStrategy::StripThenSummarize
    );
    assert_eq!(parsed.context_window, Some(10_000));
    assert_eq!(parsed.compaction_threshold, Some(0.57));
    assert_eq!(
        parsed
            .latest_context
            .as_ref()
            .map(|context| context.accounting.estimated_input_tokens),
        Some(650)
    );
}

#[tokio::test]
async fn sessions_get_fails_usage_totals_closed_for_partial_rows() {
    let node = seeded_node().await;
    let response = node
        .execute(r#"{ AgentRequest(filter: {request_id: {_eq: "request-a-new"}}) { _docID } }"#)
        .await;
    let request_doc_id = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("_docID"))
        .and_then(Value::as_str)
        .unwrap();
    let mutation = format!(
        r#"mutation {{ create_InferenceCall(input: {{
                call_id: "session-a:partial"
                request_id: "request-a-new"
                request_doc_id: "{}"
                node_did: "did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7"
                call_kind: "inference"
                backend_id: "backend-a"
                call_state: "completed"
                call_seq: 2
                queued_at: "2026-06-03T10:05:31Z"
                prompt_tokens: 999
            }}) {{ _docID }} }}"#,
        escape_graphql_string(request_doc_id)
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "partial usage seed: {:?}",
        response.errors
    );
    let mutation = format!(
        r#"mutation {{ create_InferenceCall(input: {{
                call_id: "session-a:invalid-cache"
                request_id: "request-a-new"
                request_doc_id: "{}"
                node_did: "did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7"
                call_kind: "inference"
                backend_id: "backend-a"
                call_state: "completed"
                call_seq: 3
                queued_at: "2026-06-03T10:05:32Z"
                prompt_tokens: 100
                completion_tokens: 10
                cached_input_tokens: 101
            }}) {{ _docID }} }}"#,
        escape_graphql_string(request_doc_id)
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "invalid cache seed: {:?}",
        response.errors
    );

    let snapshot = load_session_investigation(
        &node,
        "did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7",
        "session-a",
    )
    .await
    .unwrap();
    assert_eq!(snapshot.token_usage.model_calls, 3);
    assert_eq!(snapshot.token_usage.calls_with_usage, 1);
    assert_eq!(snapshot.token_usage.input_tokens, None);
    assert_eq!(snapshot.token_usage.output_tokens, None);
    assert_eq!(snapshot.token_usage.charged_tokens, None);
    assert!(snapshot.token_usage.incomplete);
    assert!(snapshot
        .parent_inclusive_audit_usage
        .iter()
        .any(|observation| {
            matches!(observation, super::super::ParentAuditUsageObservation::Unavailable {
                parent_request_id, error, ..
            } if parent_request_id == "request-a-new"
                && error.contains("cache read exceeds"))
        }));
}

#[tokio::test]
async fn sessions_get_does_not_join_duplicate_logical_request_ids() {
    let node = seeded_node().await;
    let identities = tempfile::tempdir().unwrap();
    let identity = pin_fixed_signing_identity(identities.path());
    let mut duplicate = gents_protocol::request_admission::AgentRequestCreate::base(
        gents_protocol::request_admission::RequestPurpose::Normal,
        "request-a-new",
        PIN_FIXED_DID,
        PIN_FIXED_DID,
        "agent-b",
        "session-b",
        "duplicate-label request fixture",
        "interactive",
        "2026-06-03T11:05:00Z",
        gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(PIN_FIXED_DID),
    );
    crate::sign_agent_request_create(&identity, &mut duplicate)
        .await
        .unwrap();
    let response = crate::config_client::ConfigAccess::write_local_response(
        &node,
        "test.session_history.duplicate_request",
        &duplicate.graphql_mutation().unwrap(),
    )
    .await
    .unwrap();
    crate::graphql::ensure_no_errors(&response, "duplicate-label request").unwrap();
    let response = node
        .execute(
            r#"{
                    AgentRequest(filter: {
                        request_id: {_eq: "request-a-new"},
                        session_id: {_eq: "session-b"}
                    }) { _docID }
                }"#,
        )
        .await;
    assert!(
        !response.has_errors(),
        "duplicate-label request lookup: {:?}",
        response.errors
    );
    let outsider_doc_id = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("_docID"))
        .and_then(Value::as_str)
        .expect("outsider request document ID");
    let mutation = format!(
        r#"mutation {{ create_InferenceCall(input: {{
                call_id: "session-b:duplicate-label-call"
                request_id: "request-a-new"
                request_doc_id: "{}"
                node_did: "did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7"
                call_kind: "inference"
                backend_id: "backend-a"
                call_state: "completed"
                call_seq: 1
                queued_at: "2026-06-03T11:05:01Z"
                prompt_tokens: 900
                completion_tokens: 90
                usage_completeness: "complete"
            }}) {{ _docID }} }}"#,
        escape_graphql_string(outsider_doc_id)
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "outsider inference: {:?}",
        response.errors
    );

    let snapshot = load_session_investigation(
        &node,
        "did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7",
        "session-a",
    )
    .await
    .unwrap();
    assert_eq!(snapshot.token_usage.model_calls, 1);
    assert_eq!(snapshot.token_usage.input_tokens, Some(100));
    assert_eq!(snapshot.token_usage.charged_tokens, Some(120));
}
