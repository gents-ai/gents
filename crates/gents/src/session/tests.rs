use super::*;
use crate::ensure_runtime_schemas;
use crate::llm::message::{Text, ToolResult, ToolResultContent, UserContent};
use crate::test_support::first_content;

#[tokio::test]
async fn provider_history_excludes_current_input_but_keeps_its_tool_results() {
    let tempdir = tempfile::tempdir().unwrap();
    let node = defra_node::EmbeddedNode::builder()
        .data_path(tempdir.path())
        .build()
        .await
        .unwrap();
    ensure_runtime_schemas(&node).await.unwrap();
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
        "did:test:test",
        None,
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
    ensure_runtime_schemas(&node).await.unwrap();

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
        10,
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
        30,
        1200,
        250,
    )
    .await
    .unwrap();
    let generation = load_prompt_compaction_state(&node, "session-1", "did:test:test", None, None)
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
        40,
        500,
        100,
        &generation,
    )
    .await
    .unwrap();

    let entries = load_compaction_entries(&node, "session-1", "did:test:test", None)
        .await
        .unwrap();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].files_read, vec!["/tmp/a.rs"]);
    assert_eq!(entries[1].files_read, vec!["/tmp/a.rs", "/tmp/c.rs"]);
    assert_eq!(entries[1].files_modified, vec!["/tmp/b.rs", "/tmp/d.rs"]);
    assert_eq!(entries[2].compacted_through_sequence, Some(40));
    let prompt_state =
        load_prompt_compaction_state(&node, "session-1", "did:test:test", None, None)
            .await
            .unwrap();
    assert_eq!(prompt_state.summaries.len(), 3);
    assert_eq!(prompt_state.total_messages_compacted, 15);
    assert_eq!(prompt_state.compacted_through_sequence, Some(40));
    let before_cursor =
        load_prompt_compaction_state(&node, "session-1", "did:test:test", None, Some(20))
            .await
            .unwrap();
    assert_eq!(before_cursor.summaries, vec!["First summary"]);
    assert_eq!(before_cursor.total_messages_compacted, 5);
    assert_eq!(before_cursor.compacted_through_sequence, Some(10));
    let at_cursor =
        load_prompt_compaction_state(&node, "session-1", "did:test:test", None, Some(40))
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
    ensure_runtime_schemas(&node).await.unwrap();
    let generation =
        load_prompt_compaction_state(&node, "session-race", "did:test:test", None, None)
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
        20,
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
        30,
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

    let entries = load_compaction_entries(&node, "session-race", "did:test:test", None)
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
    ensure_runtime_schemas(&node).await.unwrap();
    let generation =
        load_prompt_compaction_state(&node, "session-redelivery", "did:test:test", None, None)
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
            20,
            100,
            20,
            &generation,
        )
    };
    let first = save().await.unwrap();
    let second = save().await.unwrap();
    assert_eq!(second, first);
    let next_generation =
        load_prompt_compaction_state(&node, "session-redelivery", "did:test:test", None, None)
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
        30,
        80,
        10,
        &next_generation,
    )
    .await
    .unwrap();
    let replay_after_later = save().await.unwrap();
    assert_eq!(replay_after_later, first);
    assert_eq!(
        load_compaction_entries(&node, "session-redelivery", "did:test:test", None)
            .await
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn history_sequence_cursor_loads_only_the_sparse_suffix() {
    let node = defra_node::EmbeddedNode::builder().build().await.unwrap();
    ensure_runtime_schemas(&node).await.unwrap();
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

    let rows = history::load_sequenced_history_projection(
        &node,
        "session-cursor",
        "did:test:test",
        None,
        None,
        Some(20),
        None,
    )
    .await
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].sequence, 40);
    assert_eq!(rows[0].message, Message::user("active"));
}

#[tokio::test]
async fn close_session_preserves_creation_time() {
    let data_path = std::env::temp_dir().join(format!("gents-session-{}", uuid::Uuid::new_v4()));
    let node = defra_node::EmbeddedNode::builder()
        .data_path(&data_path)
        .build()
        .await
        .unwrap();
    ensure_runtime_schemas(&node).await.unwrap();

    create_session_with_id(&node, "session-1", "deploy-test", "did:test:test")
        .await
        .unwrap();
    let before = node
        .execute(r#"{ AgentSession(filter: {session_id: {_eq: "session-1"}}) {created_at} }"#)
        .await;
    assert!(!before.has_errors(), "{:?}", before.errors);
    let original_created_at =
        before.data.as_ref().unwrap()["AgentSession"][0]["created_at"].clone();
    close_session(&node, "did:test:test", "session-1", None)
        .await
        .unwrap();

    let resp = node
        .execute(
            r#"{
                AgentSession(
                    filter: { session_id: { _eq: "session-1" } },
                    limit: 1
                ) {
                    behavior_id
                    created_at
                    closed_at
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
        row.get("behavior_id").and_then(|value| value.as_str()),
        Some("deploy-test")
    );
    assert!(row
        .get("created_at")
        .and_then(|value| value.as_str())
        .is_some_and(|value| !value.is_empty()));
    assert!(row
        .get("closed_at")
        .and_then(|value| value.as_str())
        .is_some_and(|value| !value.is_empty()));

    assert_eq!(row["created_at"], original_created_at);
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
    ensure_runtime_schemas(&node).await.unwrap();

    create_session_with_id(&node, "session-1", "general", "did:test:test")
        .await
        .unwrap();
    let before = node
        .execute(r#"{ AgentSession(filter: {session_id: {_eq: "session-1"}}) {created_at} }"#)
        .await;
    assert!(!before.has_errors(), "{:?}", before.errors);
    let original_created_at =
        before.data.as_ref().unwrap()["AgentSession"][0]["created_at"].clone();
    let patched = node
        .execute(
            r#"mutation { update_AgentSession(filter: {session_id: {_eq: "session-1"}}, input: {
        tags: ["keep-on-resume"], title: {text: "User title", source: "user"}
    }) {_docID} }"#,
        )
        .await;
    assert!(!patched.has_errors(), "{:?}", patched.errors);

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
                    created_at
                    tags
                    title
                    agent_did
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
    assert_eq!(rows[0]["created_at"], original_created_at);
    assert_eq!(rows[0]["tags"], serde_json::json!(["keep-on-resume"]));
    assert_eq!(
        rows[0]["title"],
        serde_json::json!({"text": "User title", "source": "user"})
    );
    assert_eq!(
        rows[0].get("agent_did").and_then(|value| value.as_str()),
        Some("did:test:test")
    );
    assert_eq!(
        rows[0].get("behavior_id").and_then(|value| value.as_str()),
        Some("general")
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
    ensure_runtime_schemas(&node).await.unwrap();

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

#[tokio::test]
async fn history_reads_exact_principal_and_requester_scope() {
    let node = defra_node::EmbeddedNode::builder().build().await.unwrap();
    ensure_runtime_schemas(&node).await.unwrap();
    let rows = [
        ("did:key:owner", None, "local"),
        ("did:key:other", None, "foreign owner"),
        (
            "did:key:owner",
            Some("did:key:requester"),
            "remote requester",
        ),
    ];
    for (index, (owner, requester, text)) in rows.into_iter().enumerate() {
        let mutation = create_message_mutation(
            "shared-session",
            owner,
            requester,
            1,
            "user",
            text,
            None,
            None,
            None,
            Some(&format!("scope-fixture-{index}")),
        );
        let response = node.execute(&mutation).await;
        assert!(!response.has_errors(), "{:?}", response.errors);
    }
    assert_eq!(
        load_history(&node, "shared-session", "did:key:owner", None)
            .await
            .unwrap(),
        vec![Message::user("local")]
    );
    assert_eq!(
        load_history(
            &node,
            "shared-session",
            "did:key:owner",
            Some("did:key:requester")
        )
        .await
        .unwrap(),
        vec![Message::user("remote requester")]
    );
    assert!(
        load_history(&node, "shared-session", "did:key:absent", None)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn append_sequences_and_default_keys_are_scoped() {
    let node = defra_node::EmbeddedNode::builder().build().await.unwrap();
    ensure_runtime_schemas(&node).await.unwrap();
    for (owner, requester, text) in [
        ("did:key:owner", None, "local"),
        ("did:key:other", None, "foreign"),
        ("did:key:owner", Some("did:key:requester"), "requested"),
    ] {
        let sequence = append_message_with_requester_did(
            &node, "same", owner, requester, "user", text, None, None, None,
        )
        .await
        .unwrap();
        assert_eq!(sequence, 1);
    }
    save_message(
        &node,
        "same",
        "did:key:owner",
        1,
        "user",
        "updated local",
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        load_history(&node, "same", "did:key:other", None)
            .await
            .unwrap(),
        vec![Message::user("foreign")]
    );
    assert_eq!(
        load_history(&node, "same", "did:key:owner", Some("did:key:requester"))
            .await
            .unwrap(),
        vec![Message::user("requested")]
    );
    assert_eq!(
        load_history(&node, "same", "did:key:owner", None)
            .await
            .unwrap(),
        vec![Message::user("updated local")]
    );
}

#[tokio::test]
async fn compaction_chains_with_equal_session_labels_remain_owner_scoped() {
    let node = defra_node::EmbeddedNode::builder().build().await.unwrap();
    ensure_runtime_schemas(&node).await.unwrap();
    for (owner, summary) in [
        ("did:key:owner", "local summary"),
        ("did:key:other", "foreign summary"),
    ] {
        save_compaction_entry(
            &node,
            "same-chain",
            owner,
            "request",
            "request-doc",
            summary,
            &[],
            &[],
            1,
            1,
            100,
            10,
        )
        .await
        .unwrap();
    }
    let local = load_compaction_entries(&node, "same-chain", "did:key:owner", None)
        .await
        .unwrap();
    let foreign = load_compaction_entries(&node, "same-chain", "did:key:other", None)
        .await
        .unwrap();
    assert_eq!(local.len(), 1);
    assert_eq!(foreign.len(), 1);
    assert_eq!(local[0].summary, "local summary");
    assert_eq!(foreign[0].summary, "foreign summary");
    assert!(load_compaction_entries(
        &node,
        "same-chain",
        "did:key:owner",
        Some("did:key:requester")
    )
    .await
    .unwrap()
    .is_empty());
}

#[tokio::test]
async fn concurrent_keyed_appends_resolve_sequence_conflicts_without_duplicates() {
    let node = defra_node::EmbeddedNode::builder().build().await.unwrap();
    ensure_runtime_schemas(&node).await.unwrap();
    let append = |key: &'static str| {
        append_message_once_with_key_and_requester_did(
            &node,
            "append-race",
            "did:key:owner",
            None,
            "user",
            key,
            None,
            None,
            None,
            key,
            Some(1),
        )
    };
    let (left, right) = tokio::join!(append("left"), append("right"));
    let (left, fresh_left) = left.unwrap();
    let (right, fresh_right) = right.unwrap();
    assert!(fresh_left && fresh_right);
    assert_ne!(left, right);
    assert_eq!(append("left").await.unwrap(), (left, false));
    assert_eq!(
        load_history(&node, "append-race", "did:key:owner", None)
            .await
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn response_materialization_uses_physical_request_and_exact_session_scope() {
    let tempdir = tempfile::tempdir().unwrap();
    let node = defra_node::EmbeddedNode::builder()
        .data_path(tempdir.path())
        .build()
        .await
        .unwrap();
    ensure_runtime_schemas(&node).await.unwrap();
    for (key, owner, requester, document) in [
        ("selected", "owner", "null", "selected-doc"),
        ("other-request", "owner", "null", "other-doc"),
        ("other-owner", "foreign", "null", "selected-doc"),
        ("other-requester", "owner", "\"requester\"", "selected-doc"),
    ] {
        let result = node
            .execute(&format!(
                r#"mutation {{ create_AgentResponse(input: {{
            response_key: "{key}", request_id: "same-label", request_doc_id: "{document}",
            agent_did: "{owner}", requester_did: {requester}, session_id: "session"
        }}) {{ _docID }} }}"#
            ))
            .await;
        assert!(!result.has_errors(), "{:?}", result.errors);
    }
    mark_response_materialized(&node, "owner", "session", None, "selected-doc", 7)
        .await
        .unwrap();
    let response = node
        .execute("{AgentResponse {response_key materialized_message_sequence}}")
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    for row in response.data.as_ref().unwrap()["AgentResponse"]
        .as_array()
        .unwrap()
    {
        assert_eq!(
            row["materialized_message_sequence"],
            if row["response_key"] == "selected" {
                serde_json::json!(7)
            } else {
                serde_json::Value::Null
            }
        );
    }
    assert!(
        mark_response_materialized(&node, "owner", "session", None, "missing-doc", 8)
            .await
            .is_err()
    );

    // A malformed duplicate must abort the transaction, including its first update.
    let result = node
        .execute(
            r#"mutation {create_AgentResponse(input: {
        response_key: "duplicate", request_id: "same-label", request_doc_id: "selected-doc",
        agent_did: "owner", session_id: "session"
    }) {_docID}}"#,
        )
        .await;
    assert!(!result.has_errors(), "{:?}", result.errors);
    assert!(
        mark_response_materialized(&node, "owner", "session", None, "selected-doc", 9)
            .await
            .is_err()
    );
    let response = node
        .execute("{AgentResponse {response_key materialized_message_sequence}}")
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    for row in response.data.as_ref().unwrap()["AgentResponse"]
        .as_array()
        .unwrap()
    {
        assert_eq!(
            row["materialized_message_sequence"],
            if row["response_key"] == "selected" {
                serde_json::json!(7)
            } else {
                serde_json::Value::Null
            }
        );
    }
}
