use super::*;
use crate::ensure_schemas;
use crate::llm::message::{AssistantContent, Text, ToolResult, ToolResultContent, UserContent};
use crate::test_support::first_content;
use gents_protocol::transcript::decode_persisted_message;

#[test]
fn compaction_created_at_canonicalizes_fractional_trailing_zeroes() {
    assert_eq!(
        canonical_compaction_created_at("2026-08-26T03:10:11.948510Z").unwrap(),
        "2026-08-26T03:10:11.94851Z"
    );
    assert_eq!(
        canonical_compaction_created_at("2026-08-26T03:10:11.000000Z").unwrap(),
        "2026-08-26T03:10:11Z"
    );
}

#[test]
fn test_load_history_deserializes_plain_text() {
    let user_msg = Message::User {
        content: vec![UserContent::Text(Text {
            text: "hello".to_string(),
        })],
    };
    let json = serde_json::to_string(&user_msg).unwrap();
    let restored = decode_persisted_message("user", &json);
    assert_eq!(user_msg, restored);
}

#[test]
fn test_load_history_deserializes_legacy_assistant_content() {
    let legacy_content = vec![
        AssistantContent::Reasoning(
            crate::llm::message::Reasoning::new("Need to inspect first")
                .with_id("rs_1".to_string()),
        ),
        AssistantContent::Text(Text {
            text: "Done".to_string(),
        }),
    ];

    let restored = decode_persisted_message(
        "assistant",
        &serde_json::to_string(&legacy_content).unwrap(),
    );
    assert!(matches!(
        restored,
        Message::Assistant { content, .. }
            if content.len() == 2
                && matches!(first_content(&content), AssistantContent::Reasoning(reasoning) if reasoning.id.as_deref() == Some("rs_1"))
                && matches!(content.get(1), Some(AssistantContent::Text(Text { text })) if text == "Done")
    ));
}

#[tokio::test]
async fn provider_history_excludes_current_input_but_keeps_its_tool_results() {
    let tempdir = tempfile::tempdir().unwrap();
    let node = defra_node::EmbeddedNode::builder()
        .data_path(tempdir.path())
        .build()
        .await
        .unwrap();
    ensure_schemas(&node).await.unwrap();
    let session_id = "session-steering-provider-history";
    append_message_once_with_key_and_requester_did(
        &node,
        session_id,
        "did:test:test",
        None,
        "user",
        "older steering",
        None,
        Some("request-old"),
        Some("doc-old"),
        "steering-input:request-old",
        Some(1),
    )
    .await
    .unwrap();
    let tool_result = Message::User {
        content: vec![UserContent::ToolResult(ToolResult {
            id: "result-current".to_string(),
            call_id: Some("call-current".to_string()),
            content: vec![ToolResultContent::Text(Text {
                text: "tool finished".to_string(),
            })],
        })],
    };
    append_message_once_with_key_and_requester_did(
        &node,
        session_id,
        "did:test:test",
        None,
        "user",
        &serde_json::to_string(&tool_result).unwrap(),
        None,
        Some("request-current"),
        Some("doc-current"),
        "session-steering-provider-history:tool-result:current",
        Some(3),
    )
    .await
    .unwrap();
    append_message_once_with_key_and_requester_did(
        &node,
        session_id,
        "did:test:test",
        None,
        "user",
        "current steering",
        None,
        Some("request-current"),
        Some("doc-current"),
        "session-steering-provider-history:2",
        Some(2),
    )
    .await
    .unwrap();

    let current_input = Message::user("current steering");
    let history = history::load_history_projection(
        &node,
        session_id,
        None,
        Some(("request-current", current_input)),
    )
    .await
    .unwrap();

    assert_eq!(history.len(), 2);
    assert!(matches!(
        history.first(),
        Some(Message::User { content })
            if matches!(first_content(&content), UserContent::Text(Text { text }) if text == "older steering")
    ));
    assert_eq!(history.get(1), Some(&tool_result));
}

#[tokio::test]
async fn compaction_entries_track_files_cumulatively() {
    let data_path = std::env::temp_dir().join(format!("gents-compaction-{}", uuid::Uuid::new_v4()));
    let node = defra_node::EmbeddedNode::builder()
        .data_path(&data_path)
        .build()
        .await
        .unwrap();
    ensure_schemas(&node).await.unwrap();

    save_compaction_entry(
        &node,
        "session-1",
        "did:test:test",
        "request-1",
        "request-doc-1",
        "First summary",
        &["/tmp/a.rs".to_string()],
        &["/tmp/b.rs".to_string()],
        5,
        1000,
        200,
    )
    .await
    .unwrap();
    save_compaction_entry(
        &node,
        "session-1",
        "did:test:test",
        "request-2",
        "request-doc-2",
        "Second summary",
        &["/tmp/c.rs".to_string(), "/tmp/a.rs".to_string()],
        &["/tmp/d.rs".to_string()],
        7,
        1200,
        250,
    )
    .await
    .unwrap();
    let generation = load_prompt_compaction_state(&node, "session-1", None)
        .await
        .unwrap()
        .generation;
    save_compaction_entry_with_requester_did(
        &node,
        "session-1",
        "did:test:test",
        None,
        "request-3",
        "request-doc-3",
        "Third summary",
        &[],
        &[],
        3,
        Some(40),
        500,
        100,
        &generation,
    )
    .await
    .unwrap();

    let entries = load_compaction_entries(&node, "session-1").await.unwrap();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].files_read, vec!["/tmp/a.rs"]);
    assert_eq!(entries[1].files_read, vec!["/tmp/a.rs", "/tmp/c.rs"]);
    assert_eq!(entries[1].files_modified, vec!["/tmp/b.rs", "/tmp/d.rs"]);
    assert_eq!(entries[2].compacted_through_sequence, Some(40));
    let prompt_state = load_prompt_compaction_state(&node, "session-1", None)
        .await
        .unwrap();
    assert_eq!(prompt_state.summaries.len(), 3);
    assert_eq!(prompt_state.total_messages_compacted, 15);
    assert_eq!(prompt_state.compacted_through_sequence, Some(40));
    let before_cursor = load_prompt_compaction_state(&node, "session-1", Some(20))
        .await
        .unwrap();
    assert!(before_cursor.summaries.is_empty());
    assert_eq!(before_cursor.total_messages_compacted, 0);
    assert_eq!(before_cursor.compacted_through_sequence, None);
    let at_cursor = load_prompt_compaction_state(&node, "session-1", Some(40))
        .await
        .unwrap();
    assert_eq!(at_cursor.summaries.len(), 3);
    assert_eq!(at_cursor.total_messages_compacted, 15);
    assert_eq!(at_cursor.compacted_through_sequence, Some(40));

    let _ = std::fs::remove_dir_all(&data_path);
}

#[tokio::test]
async fn concurrent_compactions_from_one_generation_persist_exactly_one_fact() {
    let node = defra_node::EmbeddedNode::builder().build().await.unwrap();
    ensure_schemas(&node).await.unwrap();
    let generation = load_prompt_compaction_state(&node, "session-race", None)
        .await
        .unwrap()
        .generation;

    let left_files = ["/tmp/left.rs".to_string()];
    let right_files = ["/tmp/right.rs".to_string()];
    let left = save_compaction_entry_with_requester_did(
        &node,
        "session-race",
        "did:test:test",
        None,
        "request-left",
        "request-doc-left",
        "left summary",
        &left_files,
        &[],
        2,
        Some(20),
        100,
        20,
        &generation,
    );
    let right = save_compaction_entry_with_requester_did(
        &node,
        "session-race",
        "did:test:test",
        None,
        "request-right",
        "request-doc-right",
        "right summary",
        &[],
        &right_files,
        3,
        Some(30),
        200,
        40,
        &generation,
    );
    let (left, right) = tokio::join!(left, right);
    assert_ne!(
        left.is_ok(),
        right.is_ok(),
        "exactly one stale writer must win"
    );
    let loser = if left.is_err() {
        left.unwrap_err()
    } else {
        right.unwrap_err()
    };
    assert!(
        format!("{loser:#}").contains("stale compaction generation"),
        "unexpected loser error: {loser:#}"
    );

    let entries = load_compaction_entries(&node, "session-race")
        .await
        .unwrap();
    assert_eq!(entries.len(), 1);
    let winner = &entries[0];
    match winner.summary.as_str() {
        "left summary" => {
            assert_eq!(winner.messages_compacted, 2);
            assert_eq!(winner.compacted_through_sequence, Some(20));
            assert_eq!(winner.files_read, vec!["/tmp/left.rs"]);
            assert!(winner.files_modified.is_empty());
        }
        "right summary" => {
            assert_eq!(winner.messages_compacted, 3);
            assert_eq!(winner.compacted_through_sequence, Some(30));
            assert!(winner.files_read.is_empty());
            assert_eq!(winner.files_modified, vec!["/tmp/right.rs"]);
        }
        summary => panic!("mixed or unexpected winner payload: {summary}"),
    }
}

#[tokio::test]
async fn exact_compaction_redelivery_is_idempotent() {
    let node = defra_node::EmbeddedNode::builder().build().await.unwrap();
    ensure_schemas(&node).await.unwrap();
    let generation = load_prompt_compaction_state(&node, "session-redelivery", None)
        .await
        .unwrap()
        .generation;
    let files = ["/tmp/a.rs".to_string()];
    let save = || {
        save_compaction_entry_with_requester_did(
            &node,
            "session-redelivery",
            "did:test:test",
            None,
            "request-redelivery",
            "request-doc-redelivery",
            "same summary",
            &files,
            &[],
            2,
            Some(20),
            100,
            20,
            &generation,
        )
    };
    let first = save().await.unwrap();
    let second = save().await.unwrap();
    assert_eq!(second, first);
    let next_generation = load_prompt_compaction_state(&node, "session-redelivery", None)
        .await
        .unwrap()
        .generation;
    save_compaction_entry_with_requester_did(
        &node,
        "session-redelivery",
        "did:test:test",
        None,
        "request-later",
        "request-doc-later",
        "later summary",
        &[],
        &[],
        1,
        Some(30),
        80,
        10,
        &next_generation,
    )
    .await
    .unwrap();
    let replay_after_later = save().await.unwrap();
    assert_eq!(replay_after_later, first);
    assert_eq!(
        load_compaction_entries(&node, "session-redelivery")
            .await
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn history_sequence_cursor_loads_only_the_sparse_suffix() {
    let node = defra_node::EmbeddedNode::builder().build().await.unwrap();
    ensure_schemas(&node).await.unwrap();
    for (sequence, content) in [(10, "old-a"), (20, "old-b"), (40, "active")] {
        save_message(
            &node,
            "session-cursor",
            "did:test:test",
            sequence,
            "user",
            content,
            None,
        )
        .await
        .unwrap();
    }

    let rows =
        history::load_sequenced_history_projection(&node, "session-cursor", None, Some(20), None)
            .await
            .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].sequence, 40);
    assert_eq!(rows[0].message, Message::user("active"));
}

#[tokio::test]
async fn compaction_entry_stores_exact_request_document_edge() {
    let data_path =
        std::env::temp_dir().join(format!("gents-compaction-edge-{}", uuid::Uuid::new_v4()));
    let node = defra_node::EmbeddedNode::builder()
        .data_path(&data_path)
        .build()
        .await
        .unwrap();
    ensure_schemas(&node).await.unwrap();

    let request_id = "request-exact-edge";
    let created_at = chrono::Utc::now().to_rfc3339();
    let response = node
        .execute(&format!(
            r#"mutation {{
                create_AgentRequest(input: {{
                    request_id: "{request_id}",
                    agent_did: "did:test:test",
                    session_id: "session-exact-edge",
                    content: "compact me",
                    status: "processing",
                    lifecycle_state: "processing",
                    created_at: "{created_at}"
                }}) {{ _docID }}
            }}"#
        ))
        .await;
    assert!(
        !response.has_errors(),
        "creating request: {:?}",
        response.errors
    );
    let response = node
        .execute(&format!(
            r#"{{
                AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}, limit: 2) {{
                    _docID
                }}
            }}"#
        ))
        .await;
    assert!(
        !response.has_errors(),
        "loading request: {:?}",
        response.errors
    );
    let request_rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(serde_json::Value::as_array)
        .expect("request rows");
    assert_eq!(request_rows.len(), 1, "request lookup must be unambiguous");
    let request_doc_id = request_rows[0]
        .get("_docID")
        .and_then(serde_json::Value::as_str)
        .expect("created request _docID");

    save_compaction_entry(
        &node,
        "session-exact-edge",
        "did:test:test",
        request_id,
        request_doc_id,
        "Exact edge summary",
        &[],
        &[],
        3,
        600,
        120,
    )
    .await
    .unwrap();

    let response = node
        .execute(
            r#"{
                CompactionEntry(
                    filter: { compaction_key: { _eq: "session-exact-edge:1" } },
                    limit: 1
                ) {
                    request_id
                    request_doc_id
                }
            }"#,
        )
        .await;
    assert!(
        !response.has_errors(),
        "querying compaction: {:?}",
        response.errors
    );
    let row = response
        .data
        .as_ref()
        .and_then(|data| data.get("CompactionEntry"))
        .and_then(serde_json::Value::as_array)
        .and_then(|rows| rows.first())
        .expect("compaction row");
    assert_eq!(row["request_id"], request_id);
    assert_eq!(row["request_doc_id"], request_doc_id);

    let _ = std::fs::remove_dir_all(&data_path);
}

#[tokio::test]
async fn close_session_preserves_started_datetime() {
    let data_path = std::env::temp_dir().join(format!("gents-session-{}", uuid::Uuid::new_v4()));
    let node = defra_node::EmbeddedNode::builder()
        .data_path(&data_path)
        .build()
        .await
        .unwrap();
    ensure_schemas(&node).await.unwrap();

    create_session_with_id(&node, "session-1", "deploy-test", "did:test:test")
        .await
        .unwrap();
    close_session(&node, "session-1").await.unwrap();

    let resp = node
        .execute(
            r#"{
                AgentSession(
                    filter: { session_id: { _eq: "session-1" } },
                    limit: 1
                ) {
                    status
                    behavior_id
                    started
                    ended
                }
            }"#,
        )
        .await;
    assert!(
        !resp.has_errors(),
        "query session failed: {:?}",
        resp.errors
    );

    let row = resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentSession"))
        .and_then(|value| value.as_array())
        .and_then(|rows| rows.first())
        .cloned()
        .expect("session row");

    assert_eq!(
        row.get("status").and_then(|value| value.as_str()),
        Some("completed")
    );
    assert_eq!(
        row.get("behavior_id").and_then(|value| value.as_str()),
        Some("deploy-test")
    );
    assert!(row
        .get("started")
        .and_then(|value| value.as_str())
        .is_some_and(|value| !value.is_empty()));
    assert!(row
        .get("ended")
        .and_then(|value| value.as_str())
        .is_some_and(|value| !value.is_empty()));

    let _ = std::fs::remove_dir_all(&data_path);
}

#[tokio::test]
async fn create_session_with_id_is_idempotent() {
    let data_path =
        std::env::temp_dir().join(format!("gents-session-upsert-{}", uuid::Uuid::new_v4()));
    let node = defra_node::EmbeddedNode::builder()
        .data_path(&data_path)
        .build()
        .await
        .unwrap();
    ensure_schemas(&node).await.unwrap();

    create_session_with_id(&node, "session-1", "general", "did:test:test")
        .await
        .unwrap();
    create_session_with_id(&node, "session-1", "general", "did:test:test")
        .await
        .unwrap();

    let resp = node
        .execute(
            r#"{
                AgentSession(
                    filter: { session_id: { _eq: "session-1" } }
                ) {
                    session_id
                    agent_name
                    behavior_id
                }
            }"#,
        )
        .await;
    assert!(
        !resp.has_errors(),
        "query session rows failed: {:?}",
        resp.errors
    );

    let rows = resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentSession"))
        .and_then(|value| value.as_array())
        .cloned()
        .expect("session rows");

    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].get("agent_name").and_then(|value| value.as_str()),
        Some("general")
    );
    assert_eq!(
        rows[0].get("behavior_id").and_then(|value| value.as_str()),
        Some("general")
    );

    let _ = std::fs::remove_dir_all(&data_path);
}

#[tokio::test]
async fn update_conversation_title_with_source_persists_generated_title() {
    let data_path =
        std::env::temp_dir().join(format!("gents-conversation-title-{}", uuid::Uuid::new_v4()));
    let node = defra_node::EmbeddedNode::builder()
        .data_path(&data_path)
        .build()
        .await
        .unwrap();
    ensure_schemas(&node).await.unwrap();

    let create = node
        .execute(
            r#"mutation {
                create_AgentConversation(input: {
                    session_id: "session-1",
                    agent_name: "general",
                    agent_did: "did:key:zTestGeneral",
                    behavior_id: "general",
                    title: "",
                    title_source: "placeholder",
                    preview_text: "Draft a weekly fleet report",
                    status: "processing",
                    created_at: "2026-05-01T00:00:00Z",
                    updated_at: "2026-05-01T00:00:00Z",
                    latest_request_id: "request-1"
                }) { _docID }
            }"#,
        )
        .await;
    assert!(!create.has_errors(), "{:?}", create.errors);

    update_conversation_title_with_source(&node, "session-1", "fleet-report-draft", "generated")
        .await
        .unwrap();

    let resp = node
        .execute(
            r#"{
                AgentConversation(
                    filter: { session_id: { _eq: "session-1" } },
                    limit: 1
                ) {
                    title
                    title_source
                }
            }"#,
        )
        .await;
    assert!(
        !resp.has_errors(),
        "query conversation failed: {:?}",
        resp.errors
    );

    let row = resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentConversation"))
        .and_then(|value| value.as_array())
        .and_then(|rows| rows.first())
        .cloned()
        .expect("conversation row");

    assert_eq!(
        row.get("title").and_then(|value| value.as_str()),
        Some("fleet-report-draft")
    );
    assert_eq!(
        row.get("title_source").and_then(|value| value.as_str()),
        Some("generated")
    );

    let _ = std::fs::remove_dir_all(&data_path);
}

#[tokio::test]
async fn create_session_with_behavior_id_rejects_mismatched_existing_binding() {
    let data_path =
        std::env::temp_dir().join(format!("gents-session-binding-{}", uuid::Uuid::new_v4()));
    let node = defra_node::EmbeddedNode::builder()
        .data_path(&data_path)
        .build()
        .await
        .unwrap();
    ensure_schemas(&node).await.unwrap();

    create_session_with_behavior_id(&node, "session-1", "general", "did:test:test", "general")
        .await
        .unwrap();

    let error =
        create_session_with_behavior_id(&node, "session-1", "general", "did:test:test", "code")
            .await
            .unwrap_err();
    assert!(error.to_string().contains("behavior mismatch"));

    let _ = std::fs::remove_dir_all(&data_path);
}
