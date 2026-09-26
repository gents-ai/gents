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
            agent_did: "did:test:agent",
            request_doc_id: "request-sess-1",
            publication: {kind: "request_execution", execution_generation: "generation"},
            outcome: "complete",
            sequence: 1,
            role: "user",
            blocks: [],
            created_at: "2026-05-07T00:00:00Z"
        }) { _docID }
        second: create_AgentMessage(input: {
            message_key: "sess-1:2",
            session_id: "sess-1",
            agent_did: "did:test:agent",
            request_doc_id: "request-sess-1",
            publication: {kind: "request_execution", execution_generation: "generation"},
            outcome: "complete",
            sequence: 2,
            role: "assistant",
            blocks: [],
            created_at: "2026-05-07T00:00:01Z"
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
    assert_eq!(
        patch.store.transcript_messages.len(),
        1,
        "expected exactly one row"
    );
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
                    request_doc_id: "request-resident",
                    publication: {kind: "request_execution", execution_generation: "generation"},
                    outcome: "complete",
                    sequence: 1,
                    role: "user",
                    blocks: [],
                    created_at: "2026-08-26T00:00:00Z"
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
    assert!(observed.transcript_messages.is_empty());
    assert!(observed.tool_calls.is_empty());
    assert!(observed.compaction_entries.is_empty());

    let context = load_session_context_store(
        node.as_ref(),
        "resident",
        Some("did:test:selected"),
        Some("did:test:local"),
    )
    .await
    .expect("ephemeral context");
    assert_eq!(context.transcript_messages.len(), 1);
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
    assert_eq!(patch.store.mailbox_items.len(), 1);
    assert_eq!(patch.store.mailbox_items[0].title, "Live item");
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
            purpose: "normal",
            agent_did: "did:test:agent",
            behavior_id: "default",
            session_id: "sess-selected",
            content: "selected",
            lifecycle_state: "processing",
            execution_generation: "generation-selected",
            created_at: "2026-07-24T00:00:00Z"
        }) { _docID }
        second_request: create_AgentRequest(input: {
            request_id: "req-unrelated",
            purpose: "normal",
            agent_did: "did:test:agent",
            behavior_id: "default",
            session_id: "sess-unrelated",
            content: "unrelated",
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
    assert_eq!(
        patch.requests[0].execution_generation.as_deref(),
        Some("generation-selected")
    );
    assert!(
        patch
            .requests
            .iter()
            .all(|row| row.session_id.as_deref() == Some("sess-selected")),
        "unrelated session leaked into selected patch"
    );

    let terminal = gents_protocol::output::TerminalOutput::Message {
        message_doc_id: "bae-terminal-message".to_string(),
    };
    let terminal_literal = gents_protocol::graphql::graphql_input_literal(
        &serde_json::to_value(&terminal).expect("serialize terminal output"),
    )
    .expect("render terminal output");
    let response = node
        .execute(&format!(
            r#"mutation {{
                update_AgentRequest(
                    filter: {{request_id: {{_eq: "req-selected"}}}}
                    input: {{lifecycle_state: "completed", terminal_output: {terminal_literal}}}
                ) {{ _docID }}
            }}"#,
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let terminal_patch = load_chat_patch(node.as_ref(), "req-selected")
        .await
        .expect("terminal selected chat patch");
    assert_eq!(terminal_patch.requests[0].terminal_output, Some(terminal));
}

#[tokio::test]
async fn title_request_patch_preserves_existence_without_public_projection() {
    let node = Arc::new(NodeBuilder::default().build().await.expect("node"));
    ensure_runtime_schemas(node.as_ref())
        .await
        .expect("schemas");
    let access = gents::ConfigAccess::Local(node.clone());
    access
        .write(
            "test.title_request_patch",
            r#"mutation {
                normal: create_AgentRequest(input: {
                    request_id: "patch-normal",
                    purpose: "normal",
                    agent_did: "did:test:agent",
                    behavior_id: "default",
                    session_id: "patch-session",
                    content: "normal request",
                    lifecycle_state: "completed",
                    execution_origin: "interactive",
                    created_at: "2026-07-24T00:00:00Z"
                }) { _docID }
                title: create_AgentRequest(input: {
                    request_id: "patch-title",
                    purpose: "title-audit",
                    agent_did: "did:test:agent",
                    behavior_id: "default",
                    session_id: "patch-session",
                    content: "title audit",
                    lifecycle_state: "completed",
                    execution_origin: "interactive",
                    created_at: "2026-07-24T00:00:01Z"
                }) { _docID }
            }"#,
        )
        .await
        .expect("seed title and normal request rows");
    let rows = access
        .execute("{ AgentRequest(filter: {session_id: {_eq: \"patch-session\"}}) { _docID request_id purpose } }")
        .await
        .expect("read exact request identities");
    let rows = rows["data"]["AgentRequest"].as_array().unwrap();
    let id = |request_id: &str| {
        rows.iter()
            .find(|row| row["request_id"] == request_id)
            .and_then(|row| row["_docID"].as_str())
            .expect("created request physical ID")
    };
    let normal_id = id("patch-normal");
    let title_id = id("patch-title");
    let title_only = fetch_doc_patch(node.as_ref(), AGENT_REQUEST_NAME, &[title_id])
        .await
        .expect("title-only patch");
    assert_eq!(title_only.observed_documents, 1);
    assert!(title_only.store.requests.is_empty());

    let mixed = fetch_doc_patch(node.as_ref(), AGENT_REQUEST_NAME, &[normal_id, title_id])
        .await
        .expect("mixed patch");
    assert_eq!(mixed.observed_documents, 2);
    assert_eq!(mixed.store.requests.len(), 1);
    assert_eq!(mixed.store.requests[0].request_id, "patch-normal");

    let missing = fetch_doc_patch(
        node.as_ref(),
        AGENT_REQUEST_NAME,
        &[title_id, "physically-absent-request"],
    )
    .await
    .expect("mixed present/missing patch");
    assert_eq!(missing.observed_documents, 1);
    assert!(missing.store.requests.is_empty());
    assert!(
        missing.observed_documents < 2,
        "the observer's missing-document predicate must distinguish this batch"
    );
}
