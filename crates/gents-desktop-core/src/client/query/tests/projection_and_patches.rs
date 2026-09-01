use super::super::*;
use crate::client::schema::ensure_runtime_schemas;
use defra_node::NodeBuilder;
use gents_protocol::schemas::AGENT_MESSAGE_NAME;
use std::sync::Arc;

#[tokio::test]
async fn fetch_doc_patch_returns_only_matching_rows() {
    let node = Arc::new(NodeBuilder::default().build().await.expect("node"));
    ensure_runtime_schemas(node.as_ref())
        .await
        .expect("schemas");

    let mutation = r#"mutation {
        create_AgentMessage(input: {
            message_key: "sess-1:1",
            session_id: "sess-1",
            sequence: 1,
            role: "user",
            content: "hello",
            timestamp: "2026-05-07T00:00:00Z"
        }) { _docID }
        second: create_AgentMessage(input: {
            message_key: "sess-1:2",
            session_id: "sess-1",
            sequence: 2,
            role: "assistant",
            content: "hi",
            timestamp: "2026-05-07T00:00:01Z"
        }) { _docID }
    }"#;
    let response = node.execute(mutation).await;
    assert!(!response.has_errors(), "{:?}", response.errors);

    // DefraDB's create_* mutations return an array, so each value is
    // [{_docID: "..."}] rather than {_docID: "..."}.
    let doc_ids: Vec<String> = response
        .data
        .as_ref()
        .and_then(|d| d.as_object())
        .map(|o| {
            o.values()
                .filter_map(|v| {
                    v.as_array()
                        .and_then(|a| a.first())
                        .and_then(|x| x.get("_docID"))
                        .and_then(|x| x.as_str())
                        .map(String::from)
                })
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(doc_ids.len(), 2);

    let target_id = doc_ids[0].clone();
    let patch = fetch_doc_patch(node.as_ref(), AGENT_MESSAGE_NAME, &[&target_id])
        .await
        .expect("fetch_doc_patch");
    assert_eq!(patch.messages.len(), 1, "expected exactly one row");
}

#[tokio::test]
async fn observer_snapshot_excludes_transcript_while_context_read_stays_authoritative() {
    let node = Arc::new(NodeBuilder::default().build().await.expect("node"));
    ensure_runtime_schemas(node.as_ref())
        .await
        .expect("schemas");
    let response = node
        .execute(
            r#"mutation {
                message: create_AgentMessage(input: {
                    message_key: "resident:1",
                    session_id: "resident",
                    agent_did: "did:test:selected",
                    requester_did: "did:test:local",
                    sequence: 1,
                    role: "user",
                    content: "durable only",
                    timestamp: "2026-08-26T00:00:00Z"
                }) { _docID }
                tool: create_AgentToolCall(input: {
                    tool_call_key: "resident:tool:1",
                    session_id: "resident",
                    message_sequence: 1,
                    tool_name: "read_file"
                }) { _docID }
                compaction: create_CompactionEntry(input: {
                    compaction_key: "resident:compaction:1",
                    session_id: "resident",
                    agent_did: "did:test:selected",
                    sequence: 1,
                    summary: "summary",
                    messages_compacted: 1,
                    compacted_through_sequence: 1,
                    original_tokens: 10,
                    compacted_tokens: 2,
                    created_at: "2026-08-26T00:00:01Z"
                }) { _docID }
            }"#,
        )
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);

    let observed = load_full_snapshot(node.as_ref())
        .await
        .expect("observer snapshot");
    assert!(observed.messages.is_empty());
    assert!(observed.tool_calls.is_empty());
    assert!(observed.tool_results.is_empty());
    assert!(observed.compaction_entries.is_empty());

    let context = load_session_context_store(
        node.as_ref(),
        "resident",
        Some("did:test:selected"),
        Some("did:test:local"),
    )
    .await
    .expect("ephemeral context");
    assert_eq!(context.messages.len(), 1);
    assert_eq!(context.messages[0].content.as_deref(), Some("durable only"));
    assert_eq!(context.compaction_entries.len(), 1);
    assert_eq!(
        context.compaction_entries[0].summary.as_deref(),
        Some("summary")
    );
}

#[tokio::test]
async fn fetch_doc_patch_hydrates_mailbox_live_updates() {
    let node = Arc::new(NodeBuilder::default().build().await.expect("node"));
    ensure_runtime_schemas(node.as_ref())
        .await
        .expect("schemas");

    let response = node
        .execute(
            r#"mutation {
                create_MailboxItem(input: {
                    item_key: "graph:wait-live:ask:1",
                    requester_did: "did:test:owner",
                    agent_did: "did:test:agent",
                    status: "open",
                    kind: "ask",
                    action: "start_request",
                    title: "Live item",
                    source_kind: "graph",
                    source_id: "wait-live",
                    target_agent_did: "did:test:agent",
                    target_behavior_id: "operator",
                    created_at: "2026-08-25T00:00:00Z",
                    updated_at: "2026-08-25T00:00:00Z"
                }) { _docID }
            }"#,
        )
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let lookup = node
        .execute(
            r#"query {
                MailboxItem(filter: { item_key: { _eq: "graph:wait-live:ask:1" } }) {
                    _docID
                }
            }"#,
        )
        .await;
    assert!(!lookup.has_errors(), "{:?}", lookup.errors);
    let doc_id = lookup
        .data
        .as_ref()
        .and_then(|data| data.get("MailboxItem"))
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("_docID"))
        .and_then(Value::as_str)
        .expect("mailbox doc id");

    let patch = fetch_doc_patch(node.as_ref(), MAILBOX_ITEM_NAME, &[doc_id])
        .await
        .expect("mailbox patch");
    assert_eq!(patch.mailbox_items.len(), 1);
    assert_eq!(patch.mailbox_items[0].title, "Live item");
    assert!(supports_doc_patch_collection(MAILBOX_ITEM_NAME));
}

#[tokio::test]
async fn load_chat_patch_reads_only_the_selected_local_session() {
    let node = Arc::new(NodeBuilder::default().build().await.expect("node"));
    ensure_runtime_schemas(node.as_ref())
        .await
        .expect("schemas");

    let mutation = r#"mutation {
        first_request: create_AgentRequest(input: {
            request_id: "req-selected",
            agent_did: "did:test:agent",
            behavior_id: "default",
            session_id: "sess-selected",
            content: "selected",
            status: "processing",
            lifecycle_state: "processing",
            created_at: "2026-07-24T00:00:00Z"
        }) { _docID }
        first_response: create_AgentResponse(input: {
            response_key: "req-selected",
            request_id: "req-selected",
            agent_did: "did:test:agent",
            behavior_id: "default",
            session_id: "sess-selected",
            content: "partial",
            reasoning: "",
            status: "streaming",
            error_message: "",
            token_count: 1,
            progress_seq: 1,
            created_at: "2026-07-24T00:00:00Z"
        }) { _docID }
        second_request: create_AgentRequest(input: {
            request_id: "req-unrelated",
            agent_did: "did:test:agent",
            behavior_id: "default",
            session_id: "sess-unrelated",
            content: "unrelated",
            status: "completed",
            lifecycle_state: "completed",
            created_at: "2026-07-24T00:00:00Z"
        }) { _docID }
    }"#;
    let response = node.execute(mutation).await;
    assert!(!response.has_errors(), "{:?}", response.errors);

    let patch = load_chat_patch(node.as_ref(), "req-selected")
        .await
        .expect("selected local chat patch");
    assert_eq!(patch.requests.len(), 1);
    assert_eq!(patch.requests[0].request_id, "req-selected");
    assert_eq!(patch.responses.len(), 1);
    assert_eq!(patch.responses[0].content.as_deref(), Some("partial"));
    assert!(
        patch
            .requests
            .iter()
            .all(|row| row.session_id.as_deref() == Some("sess-selected")),
        "unrelated session leaked into selected patch"
    );
}
