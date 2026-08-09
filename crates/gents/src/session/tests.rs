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

async fn save_unlinked_compaction_test_message(
    node: &defra_node::EmbeddedNode,
    session_id: &str,
    agent_did: &str,
    sequence: u32,
    content: &str,
) {
    let response = node
        .execute(&format!(
            r#"mutation {{
                create_AgentMessage(input: {{
                    message_key: "{session_id}:{sequence}"
                    session_id: "{session_id}"
                    agent_did: "{agent_did}"
                    sequence: {sequence}
                    role: "user"
                    content: "{content}"
                    reasoning: ""
                    timestamp: "2026-08-08T00:00:0{sequence}Z"
                }}) {{ _docID }}
            }}"#,
            session_id = crate::graphql::escape_graphql_string(session_id),
            agent_did = crate::graphql::escape_graphql_string(agent_did),
            content = crate::graphql::escape_graphql_string(content),
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
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
    assert!(!loaded.fact_refs[0].collection_version_id.is_empty());
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
    let (node, signer_did, _key_dir) = message_test_node().await;
    let behavior_id = "compaction-files-behavior";
    let config_provenance = create_test_config_provenance(&node, &signer_did, behavior_id)
        .await
        .unwrap();
    let content = serde_json::to_string(&Message::User {
        content: vec![UserContent::Text(Text {
            text: "first".to_owned(),
        })],
    })
    .unwrap();
    save_message(&node, "session-1", &signer_did, 1, "user", &content, None)
        .await
        .unwrap();
    let first_history = load_history_with_refs(&node, "session-1").await.unwrap();
    let first_manifest = CompactionSourceManifest::new(
        "session-1",
        behavior_id,
        first_history.fact_refs,
        config_provenance.clone(),
        Vec::new(),
        1,
        0,
        1,
    );

    save_compaction_entry(
        &node,
        "session-1",
        &signer_did,
        "First summary",
        &["/tmp/a.rs".to_string()],
        &["/tmp/b.rs".to_string()],
        1,
        1000,
        200,
        first_manifest,
    )
    .await
    .unwrap();
    save_message(&node, "session-1", &signer_did, 2, "user", &content, None)
        .await
        .unwrap();
    let second_history = load_history_with_refs(&node, "session-1").await.unwrap();
    let previous = load_compaction_entries_for_agent(&node, "session-1", &signer_did)
        .await
        .unwrap();
    let second_manifest = CompactionSourceManifest::new(
        "session-1",
        behavior_id,
        second_history.fact_refs,
        config_provenance,
        previous.fact_refs,
        2,
        1,
        1,
    );
    save_compaction_entry(
        &node,
        "session-1",
        &signer_did,
        "Second summary",
        &["/tmp/c.rs".to_string(), "/tmp/a.rs".to_string()],
        &["/tmp/d.rs".to_string()],
        1,
        1200,
        250,
        second_manifest,
    )
    .await
    .unwrap();

    let entries = load_compaction_entries(&node, "session-1").await.unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].files_read, vec!["/tmp/a.rs"]);
    assert_eq!(entries[1].files_read, vec!["/tmp/a.rs", "/tmp/c.rs"]);
    assert_eq!(entries[1].files_modified, vec!["/tmp/b.rs", "/tmp/d.rs"]);
}

#[tokio::test]
async fn compaction_manifest_rejects_schema_version_rebinding_in_every_source_class() {
    let (node, signer_did, _key_dir) = message_test_node().await;
    let session_id = "compaction-schema-rebinding-session";
    let behavior_id = "compaction-schema-rebinding-behavior";
    let config_provenance = create_test_config_provenance(&node, &signer_did, behavior_id)
        .await
        .unwrap();
    let content = serde_json::to_string(&Message::User {
        content: vec![UserContent::Text(Text {
            text: "first".to_owned(),
        })],
    })
    .unwrap();
    save_unlinked_compaction_test_message(&node, session_id, &signer_did, 1, &content).await;
    let first_history = load_history_with_refs(&node, session_id).await.unwrap();
    save_compaction_entry(
        &node,
        session_id,
        &signer_did,
        "first",
        &[],
        &[],
        1,
        100,
        20,
        CompactionSourceManifest::new(
            session_id,
            behavior_id,
            first_history.fact_refs,
            config_provenance.clone(),
            Vec::new(),
            1,
            0,
            1,
        ),
    )
    .await
    .unwrap();

    save_unlinked_compaction_test_message(&node, session_id, &signer_did, 2, &content).await;
    let history = load_history_with_refs(&node, session_id).await.unwrap();
    let previous = load_compaction_entries_for_agent(&node, session_id, &signer_did)
        .await
        .unwrap();
    assert!(history
        .fact_refs
        .iter()
        .all(|fact| !fact.collection_version_id.is_empty()));
    assert!(previous
        .fact_refs
        .iter()
        .all(|fact| !fact.collection_version_id.is_empty()));

    let valid = CompactionSourceManifest::new(
        session_id,
        behavior_id,
        history.fact_refs,
        config_provenance,
        previous.fact_refs,
        2,
        1,
        1,
    );

    let mut transcript_rebound = valid.clone();
    transcript_rebound.transcript_snapshot[0].collection_version_id = "wrong-schema".to_string();
    let transcript_error = save_compaction_entry(
        &node,
        session_id,
        &signer_did,
        "rebound transcript",
        &[],
        &[],
        1,
        100,
        20,
        transcript_rebound,
    )
    .await
    .unwrap_err();
    let transcript_error_chain = format!("{transcript_error:#}");
    assert!(
        transcript_error_chain.contains("pinned schema"),
        "{transcript_error:#}"
    );

    let mut config_rebound = valid.clone();
    config_rebound
        .config_provenance
        .principal
        .collection_version_id = "wrong-schema".to_string();
    let config_error = save_compaction_entry(
        &node,
        session_id,
        &signer_did,
        "rebound config",
        &[],
        &[],
        1,
        100,
        20,
        config_rebound,
    )
    .await
    .unwrap_err();
    let config_error_chain = format!("{config_error:#}");
    assert!(
        config_error_chain.contains("pinned schema"),
        "{config_error:#}"
    );

    let mut prior_rebound = valid;
    prior_rebound.prior_compactions[0].collection_version_id = "wrong-schema".to_string();
    let prior_error = save_compaction_entry(
        &node,
        session_id,
        &signer_did,
        "rebound prior",
        &[],
        &[],
        1,
        100,
        20,
        prior_rebound,
    )
    .await
    .unwrap_err();
    let prior_error_chain = format!("{prior_error:#}");
    assert!(
        prior_error_chain.contains("pinned schema"),
        "{prior_error:#}"
    );
}

#[test]
fn durable_source_refs_do_not_deserialize_without_collection_version_identity() {
    assert!(serde_json::from_value::<MessageFactRef>(serde_json::json!({
        "sequence": 1,
        "doc_id": "message-doc",
        "composite_commit_cid": "message-cid",
        "signer_did": "did:key:signer"
    }))
    .is_err());
    assert!(
        serde_json::from_value::<CompactionFactRef>(serde_json::json!({
            "sequence": 1,
            "source": {
                "version": { "doc_id": "compaction-doc", "composite_commit_cid": "compaction-cid" },
                "signer_did": "did:key:signer"
            }
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<crate::ConfigFactRef>(serde_json::json!({
            "collection": "AgentPrincipal",
            "logical_id": "did:key:agent",
            "source": {
                "version": { "doc_id": "principal-doc", "composite_commit_cid": "principal-cid" },
                "signer_did": "did:key:signer"
            }
        }))
        .is_err()
    );
}

#[tokio::test]
async fn compaction_manifest_rejects_an_exact_message_from_another_session() {
    let (node, signer_did, _key_dir) = message_test_node().await;
    let behavior_id = "compaction-wrong-session-behavior";
    let config_provenance = create_test_config_provenance(&node, &signer_did, behavior_id)
        .await
        .unwrap();
    let content = serde_json::to_string(&Message::User {
        content: vec![UserContent::Text(Text {
            text: "wrong session".to_owned(),
        })],
    })
    .unwrap();
    save_message(
        &node,
        "source-session",
        &signer_did,
        1,
        "user",
        &content,
        None,
    )
    .await
    .unwrap();
    let wrong_history = load_history_with_refs(&node, "source-session")
        .await
        .unwrap();
    let manifest = CompactionSourceManifest::new(
        "target-session",
        behavior_id,
        wrong_history.fact_refs,
        config_provenance,
        Vec::new(),
        1,
        0,
        1,
    );
    let manifest_json =
        crate::rendered_request::canonical_json_string(&serde_json::to_value(&manifest).unwrap())
            .unwrap();
    let response = node
        .execute(&format!(
            r#"mutation {{
                create_CompactionEntry(input: {{
                    compaction_key: "target-session:1"
                    session_id: "target-session"
                    agent_did: "{}"
                    sequence: 1
                    summary: "forged cross-session summary"
                    files_read: "[]"
                    files_modified: "[]"
                    messages_compacted: 1
                    original_tokens: 100
                    compacted_tokens: 25
                    source_manifest_version: 2
                    source_manifest_json: "{}"
                    created_at: "2026-08-08T00:00:00Z"
                }}) {{ _docID }}
            }}"#,
            crate::graphql::escape_graphql_string(&signer_did),
            crate::graphql::escape_graphql_string(&manifest_json),
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);

    let error = load_compaction_entries(&node, "target-session")
        .await
        .expect_err("an exact message from another session must fail closed");
    assert!(
        error
            .to_string()
            .contains("does not bind compaction transcript"),
        "{error:#}"
    );
}

#[tokio::test]
async fn forked_compaction_rejects_an_unproven_payload_derivation() {
    let (node, signer_did, _key_dir) = message_test_node().await;
    let semantic_agent_did = "did:test:semantic-agent";
    assert_ne!(semantic_agent_did, signer_did);
    let behavior_id = "forked-compaction-behavior";
    let config_provenance = create_test_config_provenance(&node, semantic_agent_did, behavior_id)
        .await
        .unwrap();
    let content = serde_json::to_string(&Message::User {
        content: vec![UserContent::Text(Text {
            text: "fork input".to_owned(),
        })],
    })
    .unwrap();
    save_message(
        &node,
        "fork-child-session",
        &signer_did,
        1,
        "user",
        &content,
        None,
    )
    .await
    .unwrap();
    let history = load_history_with_refs(&node, "fork-child-session")
        .await
        .unwrap();
    let manifest = CompactionSourceManifest::new(
        "fork-child-session",
        behavior_id,
        history.fact_refs,
        config_provenance,
        Vec::new(),
        1,
        0,
        1,
    );
    let manifest_json =
        crate::rendered_request::canonical_json_string(&serde_json::to_value(&manifest).unwrap())
            .unwrap();

    let source = node
        .execute(
            r#"mutation {
                create_CompactionEntry(input: {
                    compaction_key: "fork-source-session:1"
                    session_id: "fork-source-session"
                    agent_did: "did:test:source-agent"
                    sequence: 1
                }) { _docID }
            }"#,
        )
        .await;
    assert!(!source.has_errors(), "{:?}", source.errors);
    let source_rows = node
        .execute(
            r#"{ CompactionEntry(filter: { compaction_key: { _eq: "fork-source-session:1" } }) { _docID } }"#,
        )
        .await;
    assert!(!source_rows.has_errors(), "{:?}", source_rows.errors);
    let source_doc_id = source_rows
        .data
        .as_ref()
        .and_then(|data| data.get("CompactionEntry"))
        .and_then(serde_json::Value::as_array)
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("_docID"))
        .and_then(serde_json::Value::as_str)
        .unwrap();
    let source_ref = crate::document_version::verified_current_signed_document_version(
        &node,
        "CompactionEntry",
        source_doc_id,
    )
    .await
    .unwrap();
    assert_eq!(source_ref.signer_did, signer_did);

    let child = node
        .execute(&format!(
            r#"mutation {{
                create_CompactionEntry(input: {{
                    compaction_key: "fork-child-session:1"
                    session_id: "fork-child-session"
                    agent_did: "{}"
                    sequence: 1
                    summary: "derived summary"
                    files_read: "[]"
                    files_modified: "[]"
                    messages_compacted: 1
                    original_tokens: 100
                    compacted_tokens: 25
                    source_manifest_version: 2
                    source_manifest_json: "{}"
                    created_at: "2026-08-08T00:00:00Z"
                    fork_source_doc_id: "{}"
                    fork_source_composite_commit_cid: "{}"
                    fork_source_signer_did: "{}"
                }}) {{ _docID }}
            }}"#,
            crate::graphql::escape_graphql_string(semantic_agent_did),
            crate::graphql::escape_graphql_string(&manifest_json),
            crate::graphql::escape_graphql_string(&source_ref.version.doc_id),
            crate::graphql::escape_graphql_string(&source_ref.version.composite_commit_cid),
            crate::graphql::escape_graphql_string(&source_ref.signer_did),
        ))
        .await;
    assert!(!child.has_errors(), "{:?}", child.errors);

    let error = load_compaction_entries(&node, "fork-child-session")
        .await
        .expect_err("a fork edge alone must not authorize arbitrary child contents");
    assert!(
        error.to_string().contains("fork source") || error.to_string().contains("source manifest"),
        "{error:#}"
    );
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
async fn duplicate_session_logical_id_fails_closed_without_arbitrary_update() {
    // Model a pre-index/replicated collection: current schemas prevent local
    // twins, but old stores and P2P merge can still expose them to readers.
    let node = defra_node::EmbeddedNode::builder().build().await.unwrap();
    node.add_schema(
        r#"
        type AgentSession {
            session_id: String
            agent_name: String
            agent_did: String
            requester_did: String
            behavior_id: String
            started: DateTime
            ended: DateTime
            status: String
        }
        "#,
    )
    .await
    .unwrap();

    for (agent_did, behavior_id, status) in [
        ("did:key:z-owner-a", "behavior-a", "active"),
        ("did:key:z-owner-b", "behavior-b", "paused"),
    ] {
        let response = node
            .execute(&format!(
                r#"mutation {{
                    create_AgentSession(input: {{
                        session_id: "duplicate-session"
                        agent_name: "agent"
                        agent_did: "{agent_did}"
                        behavior_id: "{behavior_id}"
                        started: "2026-08-08T00:00:00Z"
                        status: "{status}"
                    }}) {{ _docID }}
                }}"#
            ))
            .await;
        assert!(!response.has_errors(), "{:?}", response.errors);
    }

    let error = close_session(&node, "duplicate-session")
        .await
        .expect_err("logical twins must not select a session to close");
    assert!(
        error
            .downcast_ref::<LogicalDocumentResolutionError>()
            .is_some_and(|error| matches!(error, LogicalDocumentResolutionError::Conflict(_))),
        "expected typed AgentSession conflict, got {error:#}"
    );

    let response = node
        .execute(
            r#"{
                AgentSession(filter: { session_id: { _eq: "duplicate-session" } }) {
                    _docID status ended
                }
            }"#,
        )
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let mut states = response.data.unwrap()["AgentSession"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            (
                row["status"].as_str().unwrap().to_string(),
                row.get("ended")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
            )
        })
        .collect::<Vec<_>>();
    states.sort();
    assert_eq!(
        states,
        vec![("active".to_string(), None), ("paused".to_string(), None)],
        "duplicate rejection must leave every physical session untouched"
    );
}

#[tokio::test]
async fn session_ensure_rejects_singleton_immutable_owner_mismatch() {
    let node = defra_node::EmbeddedNode::builder().build().await.unwrap();
    node.add_schema(
        r#"
        type AgentSession {
            session_id: String
            agent_name: String
            agent_did: String
            requester_did: String
            behavior_id: String
            started: DateTime
            ended: DateTime
            status: String
        }
        "#,
    )
    .await
    .unwrap();
    let response = node
        .execute(
            r#"mutation {
                create_AgentSession(input: {
                    session_id: "owned-session"
                    agent_name: "foreign"
                    agent_did: "did:key:z-foreign"
                    behavior_id: "behavior"
                    started: "2026-08-08T00:00:00Z"
                    status: "paused"
                }) { _docID }
            }"#,
        )
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);

    let error = ensure_session_with_behavior_id(
        &node,
        "owned-session",
        "local",
        "did:key:z-local",
        "behavior",
    )
    .await
    .expect_err("a logical key must not authorize a different owner");
    assert!(
        error.to_string().contains("immutable owner mismatch"),
        "{error:#}"
    );

    let response = node
        .execute(
            r#"{
                AgentSession(filter: { session_id: { _eq: "owned-session" } }) {
                    agent_name agent_did status
                }
            }"#,
        )
        .await;
    let row = &response.data.unwrap()["AgentSession"][0];
    assert_eq!(row["agent_name"], "foreign");
    assert_eq!(row["agent_did"], "did:key:z-foreign");
    assert_eq!(row["status"], "paused");
}

#[tokio::test]
async fn requester_aware_session_ensure_preserves_create_time_identity_fields() {
    let node = defra_node::EmbeddedNode::builder().build().await.unwrap();
    node.add_schema(
        r#"
        type AgentSession {
            session_id: String
            agent_name: String
            agent_did: String
            requester_did: String
            behavior_id: String
            started: DateTime
            ended: DateTime
            status: String
        }
        "#,
    )
    .await
    .unwrap();
    let response = node
        .execute(
            r#"mutation {
                create_AgentSession(input: {
                    session_id: "requester-bound-session"
                    agent_name: "create-time-name"
                    agent_did: "did:key:z-agent"
                    requester_did: "did:key:z-requester"
                    behavior_id: "behavior"
                    started: "2026-08-08T00:00:00Z"
                    status: "paused"
                }) { _docID }
            }"#,
        )
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);

    ensure_session_with_behavior_id_and_requester_did(
        &node,
        "requester-bound-session",
        "different-runtime-name",
        "did:key:z-agent",
        "behavior",
        Some("did:key:z-requester"),
    )
    .await
    .unwrap();

    let response = node
        .execute(
            r#"{
                AgentSession(filter: { session_id: { _eq: "requester-bound-session" } }) {
                    agent_name agent_did requester_did behavior_id started status
                }
            }"#,
        )
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let row = &response.data.unwrap()["AgentSession"][0];
    assert_eq!(row["agent_name"], "create-time-name");
    assert_eq!(row["agent_did"], "did:key:z-agent");
    assert_eq!(row["requester_did"], "did:key:z-requester");
    assert_eq!(row["behavior_id"], "behavior");
    assert_eq!(row["started"], "2026-08-08T00:00:00Z");
    assert_eq!(row["status"], "active");

    let error = ensure_session_with_behavior_id_and_requester_did(
        &node,
        "requester-bound-session",
        "different-runtime-name",
        "did:key:z-agent",
        "behavior",
        Some("did:key:z-other-requester"),
    )
    .await
    .expect_err("an existing session must reject a different requester principal");
    assert!(
        error.to_string().contains("immutable requester mismatch"),
        "{error:#}"
    );

    let response = node
        .execute(
            r#"{
                AgentSession(filter: { session_id: { _eq: "requester-bound-session" } }) {
                    agent_name requester_did status
                }
            }"#,
        )
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let row = &response.data.unwrap()["AgentSession"][0];
    assert_eq!(row["agent_name"], "create-time-name");
    assert_eq!(row["requester_did"], "did:key:z-requester");
    assert_eq!(row["status"], "active");
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
