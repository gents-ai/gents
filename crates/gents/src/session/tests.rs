use super::*;
use crate::ensure_schemas;
use crate::identity::{AgentIdentity as _, KeyIdentity};
use crate::llm::message::{AssistantContent, Text, UserContent};
use crate::test_support::first_content;
use gents_protocol::transcript::decode_persisted_message;

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

#[test]
fn signed_snapshot_timestamps_compare_as_rfc3339_instants() {
    assert!(super::history::rfc3339_instants_equal(
        "2026-08-08T07:46:17.76087Z",
        "2026-08-08T07:46:17.760870Z"
    ));
    assert!(super::history::rfc3339_instants_equal(
        "2026-08-08T07:46:17.760870Z",
        "2026-08-08T00:46:17.760870-07:00"
    ));
    assert!(!super::history::rfc3339_instants_equal(
        "2026-08-08T07:46:17.760870Z",
        "2026-08-08T07:46:17.760871Z"
    ));
    assert!(!super::history::rfc3339_instants_equal(
        "not-a-timestamp",
        "not-a-timestamp"
    ));
}

async fn message_test_node() -> (defra_node::EmbeddedNode, String, tempfile::TempDir) {
    let key_dir = tempfile::tempdir().unwrap();
    let identity = KeyIdentity::load_or_create(key_dir.path().join("node.key"), None).unwrap();
    let did = identity.did().to_owned();
    let node = defra_node::EmbeddedNode::builder()
        .with_node_identity_did(&did)
        .build()
        .await
        .unwrap();
    ensure_schemas(&node).await.unwrap();
    (node, did, key_dir)
}

async fn message_row(
    node: &defra_node::EmbeddedNode,
    collection: &str,
    session_id: &str,
) -> serde_json::Value {
    let response = node
        .execute(&format!(
            r#"{{
                {collection}(filter: {{ session_id: {{ _eq: "{}" }} }}) {{
                    _docID message_key sequence role content reasoning
                }}
            }}"#,
            crate::graphql::escape_graphql_string(session_id)
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    response.data.unwrap()[collection][0].clone()
}

#[tokio::test]
async fn agent_message_draft_finalizes_exactly_once_and_final_fact_is_immutable() {
    let (node, signer_did, _key_dir) = message_test_node().await;
    let draft = Message::Assistant {
        id: None,
        content: vec![AssistantContent::Text(Text {
            text: "draft".to_owned(),
        })],
    };
    let finalized = Message::Assistant {
        id: None,
        content: vec![AssistantContent::Text(Text {
            text: "final".to_owned(),
        })],
    };
    let draft_json = serde_json::to_string(&draft).unwrap();
    let finalized_json = serde_json::to_string(&finalized).unwrap();

    save_message_draft_with_requester_did(
        &node,
        "finalized-message-session",
        &signer_did,
        None,
        1,
        "assistant",
        &draft_json,
        None,
    )
    .await
    .unwrap();
    let draft_row = message_row(&node, "AgentMessageDraft", "finalized-message-session").await;
    let draft_doc_id = draft_row["_docID"].as_str().unwrap().to_owned();
    assert!(load_history(&node, "finalized-message-session")
        .await
        .unwrap()
        .is_empty());

    save_message_draft_with_requester_did(
        &node,
        "finalized-message-session",
        &signer_did,
        None,
        1,
        "assistant",
        &finalized_json,
        None,
    )
    .await
    .unwrap();
    let updated_draft = message_row(&node, "AgentMessageDraft", "finalized-message-session").await;
    assert_eq!(updated_draft["_docID"], draft_doc_id);
    assert_eq!(updated_draft["content"], finalized_json);

    save_message_with_requester_did(
        &node,
        "finalized-message-session",
        &signer_did,
        None,
        1,
        "assistant",
        &finalized_json,
        None,
    )
    .await
    .unwrap();
    let finalized_row = message_row(&node, "AgentMessage", "finalized-message-session").await;
    let finalized_doc_id = finalized_row["_docID"].as_str().unwrap().to_owned();
    assert_ne!(finalized_doc_id, draft_doc_id);
    let loaded = load_history_with_refs(&node, "finalized-message-session")
        .await
        .unwrap();
    assert_eq!(loaded.messages, vec![finalized.clone()]);
    assert_eq!(loaded.fact_refs.len(), 1);
    assert_eq!(loaded.fact_refs[0].sequence, 1);
    assert_eq!(loaded.fact_refs[0].doc_id, finalized_doc_id);
    assert!(!loaded.fact_refs[0].composite_commit_cid.is_empty());
    assert_eq!(loaded.fact_refs[0].signer_did, signer_did);

    save_message_with_requester_did(
        &node,
        "finalized-message-session",
        &signer_did,
        None,
        1,
        "assistant",
        &finalized_json,
        None,
    )
    .await
    .expect("identical finalized replay is idempotent");
    let conflict = save_message_with_requester_did(
        &node,
        "finalized-message-session",
        &signer_did,
        None,
        1,
        "assistant",
        &draft_json,
        None,
    )
    .await
    .unwrap_err();
    assert!(
        conflict
            .to_string()
            .contains("AgentMessage finalized fact conflict"),
        "{conflict:#}"
    );
    let unchanged = message_row(&node, "AgentMessage", "finalized-message-session").await;
    assert_eq!(unchanged["_docID"], finalized_doc_id);
    assert_eq!(unchanged["content"], finalized_json);
}

#[tokio::test]
async fn provider_history_rejects_duplicate_finalized_order() {
    let (node, signer_did, _key_dir) = message_test_node().await;
    let content = serde_json::to_string(&Message::User {
        content: vec![UserContent::Text(Text {
            text: "hello".to_owned(),
        })],
    })
    .unwrap();
    for key in ["duplicate-order-a", "duplicate-order-b"] {
        let response = node
            .execute(&format!(
                r#"mutation {{
                    create_AgentMessage(input: {{
                        message_key: "{key}"
                        session_id: "duplicate-order-session"
                        agent_did: "{signer_did}"
                        request_id: ""
                        sequence: 1
                        role: "user"
                        content: "{}"
                        reasoning: ""
                        timestamp: "2026-08-07T00:00:00Z"
                    }}) {{ _docID }}
                }}"#,
                crate::graphql::escape_graphql_string(&content)
            ))
            .await;
        assert!(!response.has_errors(), "{:?}", response.errors);
    }

    let error = load_history(&node, "duplicate-order-session")
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("provider history rejected ambiguous AgentMessage facts"),
        "{error:#}"
    );
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
        "Second summary",
        &["/tmp/c.rs".to_string(), "/tmp/a.rs".to_string()],
        &["/tmp/d.rs".to_string()],
        7,
        1200,
        250,
    )
    .await
    .unwrap();

    let entries = load_compaction_entries(&node, "session-1").await.unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].files_read, vec!["/tmp/a.rs"]);
    assert_eq!(entries[1].files_read, vec!["/tmp/a.rs", "/tmp/c.rs"]);
    assert_eq!(entries[1].files_modified, vec!["/tmp/b.rs", "/tmp/d.rs"]);

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
async fn upsert_conversation_from_request_keeps_title_empty_until_generated() {
    let data_path =
        std::env::temp_dir().join(format!("gents-conversation-{}", uuid::Uuid::new_v4()));
    let node = defra_node::EmbeddedNode::builder()
        .data_path(&data_path)
        .build()
        .await
        .unwrap();
    ensure_schemas(&node).await.unwrap();

    let agent_did = "did:key:zTestGeneral";
    upsert_conversation_from_request_with_identity(
        &node,
        "session-1",
        "general",
        agent_did,
        "general",
        "request-1",
        "Draft a weekly fleet report",
        "processing",
    )
    .await
    .unwrap();
    upsert_conversation_from_request_with_identity(
        &node,
        "session-1",
        "general",
        agent_did,
        "general",
        "request-2",
        "Now include the overnight daemon failures too",
        "processing",
    )
    .await
    .unwrap();
    update_conversation_status_if_latest_with_identity(
        &node,
        "session-1",
        "general",
        agent_did,
        "general",
        "request-2",
        "completed",
    )
    .await
    .unwrap();

    let resp = node
        .execute(
            r#"{
                AgentConversation(
                    filter: { session_id: { _eq: "session-1" } },
                    limit: 1
                ) {
                    session_id
                    agent_name
                    agent_did
                    behavior_id
                    title
                    title_source
                    preview_text
                    status
                    latest_request_id
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

    assert_eq!(row.get("title").and_then(|value| value.as_str()), Some(""));
    assert_eq!(
        row.get("title_source").and_then(|value| value.as_str()),
        Some("placeholder")
    );
    assert_eq!(
        row.get("preview_text").and_then(|value| value.as_str()),
        Some("Now include the overnight daemon failures too")
    );
    assert_eq!(
        row.get("status").and_then(|value| value.as_str()),
        Some("completed")
    );
    assert_eq!(
        row.get("latest_request_id")
            .and_then(|value| value.as_str()),
        Some("request-2")
    );
    assert_eq!(
        row.get("agent_did").and_then(|value| value.as_str()),
        Some(agent_did)
    );
    assert_eq!(
        row.get("behavior_id").and_then(|value| value.as_str()),
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

    upsert_conversation_from_request_with_identity(
        &node,
        "session-1",
        "general",
        "did:key:zTestGeneral",
        "general",
        "request-1",
        "Draft a weekly fleet report",
        "processing",
    )
    .await
    .unwrap();

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

#[tokio::test]
async fn upsert_conversation_rejects_mismatched_existing_behavior() {
    let data_path = std::env::temp_dir().join(format!(
        "gents-conversation-binding-{}",
        uuid::Uuid::new_v4()
    ));
    let node = defra_node::EmbeddedNode::builder()
        .data_path(&data_path)
        .build()
        .await
        .unwrap();
    ensure_schemas(&node).await.unwrap();

    let agent_did = "did:key:zTestGeneral";
    upsert_conversation_from_request_with_identity(
        &node,
        "session-1",
        "general",
        agent_did,
        "general",
        "request-1",
        "Hello",
        "processing",
    )
    .await
    .unwrap();

    let error = upsert_conversation_from_request_with_identity(
        &node,
        "session-1",
        "general",
        agent_did,
        "code",
        "request-2",
        "Hello again",
        "processing",
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("behavior mismatch"));

    let _ = std::fs::remove_dir_all(&data_path);
}
