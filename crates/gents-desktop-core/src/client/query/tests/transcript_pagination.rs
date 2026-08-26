use super::super::*;
use crate::client::schema::ensure_runtime_schemas;
use defra_node::NodeBuilder;
use std::sync::Arc;

#[tokio::test]
async fn transcript_pages_bound_defradb_rows_and_use_stable_sequence_cursors() {
    let node = Arc::new(NodeBuilder::default().build().await.expect("node"));
    ensure_runtime_schemas(node.as_ref())
        .await
        .expect("schemas");

    let fields = (1..=600)
        .map(|sequence| {
            format!(
                r#"m{sequence}: create_AgentMessage(input: {{
                    message_key: "paged:{sequence}",
                    session_id: "paged",
                    sequence: {sequence},
                    role: "assistant",
                    content: "row {sequence}",
                    timestamp: "2026-08-25T00:00:00Z"
                }}) {{ _docID }}"#
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let response = node.execute(&format!("mutation {{ {fields} }}")).await;
    assert!(!response.has_errors(), "{:?}", response.errors);

    let tip = load_session_transcript_page(node.as_ref(), "paged", None, None, None, Some(10))
        .await
        .expect("tip page");
    assert_eq!(tip.query_count, 1);
    assert_eq!(tip.message_query_limit, 11);
    assert_eq!(tip.tool_call_query_limit, 321);
    assert_eq!(tip.queried_rows, 11);
    assert!(!tip.source_exhausted);
    let tip_sequences = tip
        .store
        .messages
        .iter()
        .filter_map(|row| row.sequence)
        .collect::<Vec<_>>();
    assert_eq!(tip_sequences.iter().copied().min(), Some(591));
    assert_eq!(tip_sequences.iter().copied().max(), Some(600));

    let older = load_session_transcript_page(
        node.as_ref(),
        "paged",
        None,
        None,
        Some("paged:591"),
        Some(10),
    )
    .await
    .expect("older page");
    assert_eq!(older.query_count, 2);
    assert_eq!(older.queried_rows, 11);
    assert!(older.has_newer);
    assert!(older
        .store
        .messages
        .iter()
        .all(|row| row.sequence.is_some_and(|sequence| sequence < 591)));
    assert!(tip_sequences.iter().all(|sequence| older
        .store
        .messages
        .iter()
        .all(|row| row.sequence != Some(*sequence))));

    let tool_cursor = tool_group_cursor_sequence("tools-42");
    assert_eq!(tool_cursor, Some(42));

    let tool_fields = (1..=321)
        .map(|sequence| {
            format!(
                r#"t{sequence}: create_AgentToolCall(input: {{
                    tool_call_key: "tool-heavy:{sequence}",
                    session_id: "tool-heavy",
                    message_sequence: {sequence},
                    tool_name: "bounded_tool",
                    tool_call_id: "call-{sequence}",
                    status: "completed",
                    lifecycle_state: "completed"
                }}) {{ _docID }}"#
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let response = node.execute(&format!("mutation {{ {tool_fields} }}")).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let tool_page =
        load_session_transcript_page(node.as_ref(), "tool-heavy", None, None, None, Some(40))
            .await
            .expect("many bounded tool groups remain pageable");
    assert_eq!(tool_page.store.tool_calls.len(), 320);
    assert_eq!(tool_page.queried_rows, 321);
    assert!(!tool_page.source_exhausted);

    let message_fields = (1..=10)
        .map(|sequence| {
            format!(
                r#"wm{sequence}: create_AgentMessage(input: {{
                    message_key: "tool-window:{sequence}",
                    session_id: "tool-window",
                    sequence: {sequence},
                    role: "assistant",
                    content: "window row {sequence}"
                }}) {{ _docID }}"#
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let window_tool_fields = (1..=400)
        .map(|index| {
            let sequence = ((index - 1) / 40) + 1;
            format!(
                r#"wt{index}: create_AgentToolCall(input: {{
                    tool_call_key: "tool-window:{index}",
                    session_id: "tool-window",
                    message_sequence: {sequence},
                    tool_name: "window_tool",
                    tool_call_id: "window-call-{index}"
                }}) {{ _docID }}"#
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let response = node
        .execute(&format!(
            "mutation {{ {message_fields} {window_tool_fields} }}"
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let window_tip =
        load_session_transcript_page(node.as_ref(), "tool-window", None, None, None, Some(10))
            .await
            .expect("tool-window tip");
    assert_eq!(window_tip.store.tool_calls.len(), 320);
    assert!(window_tip
        .store
        .messages
        .iter()
        .all(|row| row.sequence.is_some_and(|sequence| sequence > 2)));
    assert!(!window_tip.source_exhausted);
    let window_older = load_session_transcript_page(
        node.as_ref(),
        "tool-window",
        None,
        None,
        Some("tool-window:3"),
        Some(10),
    )
    .await
    .expect("tool-window older page");
    assert_eq!(
        window_older
            .store
            .messages
            .iter()
            .filter_map(|row| row.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(window_older.store.tool_calls.len(), 80);
    assert!(window_older.source_exhausted);
}

#[tokio::test]
async fn transcript_pages_preserve_scope_and_equal_sequence_groups() {
    let node = Arc::new(NodeBuilder::default().build().await.expect("node"));
    ensure_runtime_schemas(node.as_ref())
        .await
        .expect("schemas");

    let response = node
        .execute(
            r#"mutation {
                kept: create_AgentMessage(input: {
                    message_key: "scope:kept", session_id: "shared-session",
                    agent_did: "did:test:selected", requester_did: "did:test:local",
                    sequence: 3, role: "assistant", content: "kept"
                }) { _docID }
                keptOlder: create_AgentMessage(input: {
                    message_key: "scope:kept-older", session_id: "shared-session",
                    agent_did: "did:test:selected", requester_did: "did:test:local",
                    sequence: 2, role: "assistant", content: "kept older"
                }) { _docID }
                legacyUnscoped: create_AgentMessage(input: {
                    message_key: "scope:legacy-unscoped", session_id: "shared-session",
                    agent_did: "did:test:selected",
                    sequence: 1, role: "assistant", content: "must stay isolated"
                }) { _docID }
                wrongAgent: create_AgentMessage(input: {
                    message_key: "scope:wrong-agent", session_id: "shared-session",
                    agent_did: "did:test:other", requester_did: "did:test:local",
                    sequence: 4, role: "assistant", content: "wrong agent"
                }) { _docID }
                wrongRequester: create_AgentMessage(input: {
                    message_key: "scope:wrong-requester", session_id: "shared-session",
                    agent_did: "did:test:selected", requester_did: "did:test:other-requester",
                    sequence: 5, role: "assistant", content: "wrong requester"
                }) { _docID }
                keptTool: create_AgentToolCall(input: {
                    tool_call_key: "scope:kept-tool", session_id: "shared-session",
                    agent_did: "did:test:selected", requester_did: "did:test:local",
                    message_sequence: 3, tool_name: "kept"
                }) { _docID }
                wrongTool: create_AgentToolCall(input: {
                    tool_call_key: "scope:wrong-tool", session_id: "shared-session",
                    agent_did: "did:test:other", requester_did: "did:test:local",
                    message_sequence: 4, tool_name: "wrong"
                }) { _docID }
            }"#,
        )
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);

    let scoped = load_session_transcript_page(
        node.as_ref(),
        "shared-session",
        Some("did:test:selected"),
        Some("did:test:local"),
        None,
        Some(10),
    )
    .await
    .expect("scoped page");
    assert_eq!(scoped.store.messages.len(), 2);
    assert!(scoped
        .store
        .messages
        .iter()
        .all(|row| row.requester_did.as_deref() == Some("did:test:local")));
    assert_eq!(scoped.store.tool_calls.len(), 1);
    assert_eq!(scoped.store.tool_calls[0].tool_call_key, "scope:kept-tool");

    let scoped_older = load_session_transcript_page(
        node.as_ref(),
        "shared-session",
        Some("did:test:selected"),
        Some("did:test:local"),
        Some("scope:kept"),
        Some(10),
    )
    .await
    .expect("scoped older page");
    assert_eq!(scoped_older.store.messages.len(), 1);
    assert_eq!(
        scoped_older.store.messages[0].message_key,
        "scope:kept-older"
    );
    let scoped_tool_cursor = load_session_transcript_page(
        node.as_ref(),
        "shared-session",
        Some("did:test:selected"),
        Some("did:test:local"),
        Some("tools-3"),
        Some(10),
    )
    .await
    .expect("scoped tool watermark");
    assert_eq!(scoped_tool_cursor.store.messages.len(), 1);
    assert_eq!(
        scoped_tool_cursor.store.messages[0].message_key,
        "scope:kept-older"
    );
    assert!(load_session_transcript_page(
        node.as_ref(),
        "shared-session",
        Some("did:test:selected"),
        Some("did:test:local"),
        Some("tools-4"),
        Some(10),
    )
    .await
    .is_err());

    let response = node
        .execute(
            r#"mutation {
                first: create_AgentMessage(input: {
                    message_key: "equal:a", session_id: "equal",
                    sequence: 2, role: "assistant", content: "a"
                }) { _docID }
                second: create_AgentMessage(input: {
                    message_key: "equal:b", session_id: "equal",
                    sequence: 2, role: "assistant", content: "b"
                }) { _docID }
                older: create_AgentMessage(input: {
                    message_key: "equal:older", session_id: "equal",
                    sequence: 1, role: "assistant", content: "older"
                }) { _docID }
            }"#,
        )
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let equal_tip = load_session_transcript_page(node.as_ref(), "equal", None, None, None, Some(2))
        .await
        .expect("equal-sequence tip");
    assert_eq!(equal_tip.store.messages.len(), 2);
    assert!(equal_tip
        .store
        .messages
        .iter()
        .all(|row| row.sequence == Some(2)));
    let equal_older =
        load_session_transcript_page(node.as_ref(), "equal", None, None, Some("equal:a"), Some(2))
            .await
            .expect("equal-sequence older page");
    assert_eq!(equal_older.store.messages.len(), 1);
    assert_eq!(equal_older.store.messages[0].message_key, "equal:older");
}

#[tokio::test]
async fn transcript_pages_reject_unrepresentable_null_sequences_truthfully() {
    let node = Arc::new(NodeBuilder::default().build().await.expect("node"));
    ensure_runtime_schemas(node.as_ref())
        .await
        .expect("schemas");
    let response = node
        .execute(
            r#"mutation {
                create_AgentMessage(input: {
                    message_key: "legacy:null", session_id: "legacy",
                    role: "assistant", content: "legacy"
                }) { _docID }
            }"#,
        )
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);

    let error = load_session_transcript_page(node.as_ref(), "legacy", None, None, None, Some(10))
        .await
        .expect_err("null sequence must not be silently skipped");
    assert!(error
        .to_string()
        .contains("bounded pagination cannot represent that schema state losslessly"));
}
