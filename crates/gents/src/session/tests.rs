use super::*;
use crate::ensure_runtime_schemas;
use crate::llm::message::{Text, ToolResult, ToolResultContent, UserContent};
use crate::test_support::first_content;

#[tokio::test]
async fn provider_history_excludes_current_input_but_keeps_its_tool_results() {
    let node = std::sync::Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    ensure_runtime_schemas(&node).await.unwrap();
    let session_id = "session-steering-provider-history";
    import_history_observation(
        &node,
        "doc-old",
        session_id,
        "did:test:test",
        None,
        "older steering",
        &canonical_rows::authored_message_key("doc-old", "prompt"),
        1,
        None,
    )
    .await;
    let tool_result = Message::User {
        content: vec![UserContent::ToolResult(ToolResult {
            id: "call-current".to_string(),
            call_id: Some("call-current".to_string()),
            content: vec![ToolResultContent::Text(Text {
                text: "tool finished".to_string(),
            })],
        })],
    };
    import_history_observation(
        &node,
        "doc-current",
        session_id,
        "did:test:test",
        None,
        "tool finished",
        "session-steering-provider-history:tool-result:current",
        3,
        Some(("physical-current-tool", "call-current")),
    )
    .await;
    import_history_observation(
        &node,
        "doc-current",
        session_id,
        "did:test:test",
        None,
        "current steering",
        &canonical_rows::authored_message_key("doc-current", "prompt"),
        2,
        None,
    )
    .await;

    let history = history::load_history_projection(
        &node,
        session_id,
        "did:test:test",
        None,
        None,
        Some("doc-current"),
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
    node.shutdown().await;
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
    let node = std::sync::Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    ensure_runtime_schemas(&node).await.unwrap();
    // Exercise selection over received facts, including an out-of-scope
    // middle header. This does not authorize a requester to change the owner
    // of an existing session; publication authority is tested separately.
    let turns = [
        ("request-cursor-old", None, "sparse-old"),
        (
            "request-cursor-remote",
            Some("did:key:requester"),
            "sparse-remote",
        ),
        ("request-cursor-latest", None, "sparse-latest"),
    ];
    for (index, (request_id, requester, text)) in turns.into_iter().enumerate() {
        import_history_observation(
            &node,
            request_id,
            "session-cursor",
            "did:key:owner",
            requester,
            text,
            &format!("scope-fixture-{index}"),
            index as u32 + 1,
            None,
        )
        .await;
    }

    // Full owner-local history keeps the sparse sequence selection: the
    // remote turn occupies sequence 2 and must not appear.
    let local = history::load_sequenced_history_projection(
        &node,
        "session-cursor",
        "did:key:owner",
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        local.iter().map(|row| row.sequence).collect::<Vec<_>>(),
        vec![1, 3]
    );

    // The sequence cursor loads only the suffix after sequence 1: sequence 3,
    // never the compacted prefix and never the out-of-scope remote sequence.
    let rows = history::load_sequenced_history_projection(
        &node,
        "session-cursor",
        "did:key:owner",
        None,
        None,
        Some(1),
        None,
    )
    .await
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].sequence, 3);
    assert_eq!(rows[0].message, Message::user("sparse-latest"));
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

async fn session_rewrite_rows(node: &defra_node::EmbeddedNode) -> Vec<Vec<String>> {
    let response = node
        .execute(
            "{ AgentSession { _docID session_id behavior_id } \
               AgentMessage { _docID session_id sequence message_key } \
               CompactionEntry { _docID session_id sequence compaction_key summary } }",
        )
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let data = response.data.expect("session rewrite rows");
    ["AgentSession", "AgentMessage", "CompactionEntry"]
        .into_iter()
        .map(|collection| {
            let mut rows = data[collection]
                .as_array()
                .expect("collection rows")
                .iter()
                .map(|row| row.to_string())
                .collect::<Vec<_>>();
            rows.sort();
            rows
        })
        .collect()
}

#[tokio::test]
async fn fork_rolls_back_at_every_mutation_position() {
    const AGENT: &str = "did:test:fork-rollback";
    const SOURCE: &str = "fork-rollback-source";
    let node = std::sync::Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    ensure_runtime_schemas(&node).await.unwrap();
    crate::test_support::install_test_behavior(&node, AGENT, "fork-rollback").await;
    create_session_with_behavior_id(&node, SOURCE, "fork-rollback", AGENT, "fork-rollback")
        .await
        .unwrap();
    for sequence in 1..=3u32 {
        import_history_observation(
            &node,
            &format!("fork-rollback-request-{sequence}"),
            SOURCE,
            AGENT,
            None,
            &format!("turn {sequence}"),
            &format!("fork-rollback:{sequence}"),
            sequence,
            None,
        )
        .await;
    }
    save_compaction_entry(
        &node,
        SOURCE,
        AGENT,
        "fork-rollback-compaction",
        "fork-rollback-compaction-doc",
        "early turns",
        &[],
        &[],
        2,
        2,
        100,
        10,
    )
    .await
    .unwrap();
    let params = ForkParams {
        source_session_id: SOURCE,
        fork_at_user_turn: 3,
        caller_agent_did: AGENT,
        caller_requester_did: None,
        target_behavior_id: None,
    };

    let (outcome, writes) = crate::config_client::ConfigApplyTxn::assert_every_mutation_rolls_back(
        || fork(&node, params.clone()),
        || session_rewrite_rows(&node),
    )
    .await;
    assert_eq!(outcome.copied_messages, 3);
    assert_eq!(outcome.copied_compaction_entries, 1);
    assert!(
        writes > 4,
        "headers, compaction, and session header are separate writes: {writes}"
    );
    node.shutdown().await;
}

#[tokio::test]
async fn compaction_save_fault_leaves_chain_and_generation_intact() {
    const AGENT: &str = "did:test:compaction-rollback";
    const SESSION: &str = "compaction-rollback";
    let node = defra_node::EmbeddedNode::builder().build().await.unwrap();
    ensure_runtime_schemas(&node).await.unwrap();
    save_compaction_entry(
        &node,
        SESSION,
        AGENT,
        "request-first",
        "request-doc-first",
        "first summary",
        &["/tmp/a.rs".to_string()],
        &[],
        2,
        10,
        100,
        20,
    )
    .await
    .unwrap();
    let chain = || async {
        let entries = load_compaction_entries(&node, SESSION, AGENT, None)
            .await
            .unwrap();
        let state = load_prompt_compaction_state(&node, SESSION, AGENT, None, None)
            .await
            .unwrap();
        (entries, state)
    };
    let (_, before) = chain().await;
    let files = ["/tmp/b.rs".to_string()];

    let (saved, _) = crate::config_client::ConfigApplyTxn::assert_every_mutation_rolls_back(
        || {
            save_compaction_entry_with_requester_did(
                &node,
                SESSION,
                AGENT,
                None,
                "request-second",
                "request-doc-second",
                "second summary",
                &files,
                &[],
                3,
                20,
                200,
                30,
                &before.generation,
            )
        },
        chain,
    )
    .await;
    assert_eq!(saved.sequence, 2);
    assert_eq!(saved.files_read, vec!["/tmp/a.rs", "/tmp/b.rs"]);
    let (entries, after) = chain().await;
    assert_eq!(entries.len(), 2);
    assert_ne!(after.generation, before.generation);
    assert_eq!(after.compacted_through_sequence, Some(20));
    node.shutdown().await;
}

/// Insert canonical replica observations for reader filtering tests. These
/// tests intentionally place foreign facts beside local ones; they establish
/// no claim about admission or publication authority. The writer tests below
/// use the claimed execution owner instead.
/// Seed an already-published canonical observation for reader/claim tests.
/// This does not exercise publication authorization or producer transitions.
pub(crate) async fn import_history_observation(
    node: &std::sync::Arc<defra_node::EmbeddedNode>,
    request_id: &str,
    session_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
    text: &str,
    message_key_suffix: &str,
    sequence: u32,
    tool_call: Option<(&str, &str)>,
) {
    use canonical_rows::*;
    use gents_protocol::output::*;
    let now = chrono::Utc::now().to_rfc3339();
    let segment = OutputSegment {
        agent_did: agent_did.into(),
        requester_did: requester_did.map(str::to_owned),
        session_id: session_id.into(),
        request_doc_id: request_id.into(),
        source: match tool_call {
            Some((doc_id, _)) => OutputSource::ToolCall {
                tool_call_doc_id: doc_id.into(),
            },
            None => OutputSource::Authored {
                key: message_key_suffix.into(),
            },
        },
        writer: match tool_call {
            Some((doc_id, _)) => OutputWriter::ToolExecution {
                tool_call_doc_id: doc_id.into(),
            },
            None => OutputWriter::RequestExecution {
                execution_generation: "observed-generation".into(),
            },
        },
        ordinal: Some(0),
        runs: vec![SegmentRun {
            stream: 0,
            bytes: text.len().try_into().unwrap(),
            declaration: Some(StreamDeclaration {
                block_index: 0,
                part_index: 0,
                payload: if tool_call.is_some() {
                    StreamPayload::ToolOutput
                } else {
                    StreamPayload::Text
                },
            }),
        }],
        payload: text.into(),
        close: Some(SourceClose::Closed {
            outcome: OutputOutcome::Complete,
            segments: 1,
            stream_bytes: vec![text.len() as u64],
        }),
        created_at: now.clone(),
    };
    let response = node
        .execute_request_with_retry(
            defra_node::QueryRequest::new(CREATE_AGENT_OUTPUT_SEGMENT_MUTATION)
                .with_variables(output_segment_create_variables(&segment).unwrap()),
            defra_node::ExecuteRetryPolicy::default(),
        )
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let row = crate::graphql::single_mutation_document(&response, "create_AgentOutputSegment")
        .unwrap()
        .unwrap();
    let header = TranscriptMessage {
        message_key: message_key_suffix.into(),
        session_id: session_id.into(),
        agent_did: agent_did.into(),
        requester_did: requester_did.map(str::to_owned),
        request_doc_id: Some(request_id.into()),
        publication: match tool_call {
            Some((doc_id, _)) => MessagePublication::ToolDelivery {
                tool_call_doc_id: doc_id.into(),
            },
            None => MessagePublication::RequestExecution {
                execution_generation: "observed-generation".into(),
            },
        },
        outcome: OutputOutcome::Complete,
        sequence,
        role: MessageRole::User,
        native_id: None,
        blocks: vec![{
            let payload = PresentedPayload {
                output: PayloadRef {
                    close_doc_id: row["_docID"].as_str().unwrap().into(),
                    stream: 0,
                },
                presentation: PayloadPresentation::Full,
            };
            match tool_call {
                Some((doc_id, call_id)) => MessageBlock::ToolResult {
                    tool_call_doc_id: doc_id.into(),
                    id: call_id.into(),
                    call_id: Some(call_id.into()),
                    parts: vec![ToolResultPart::Text { text: payload }],
                },
                None => MessageBlock::Text { text: payload },
            }
        }],
        created_at: now,
    };
    let response = node
        .execute_request_with_retry(
            defra_node::QueryRequest::new(CREATE_AGENT_MESSAGE_MUTATION)
                .with_variables(transcript_message_create_variables(&header).unwrap()),
            defra_node::ExecuteRetryPolicy::default(),
        )
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let row = crate::graphql::single_mutation_document(&response, "create_AgentMessage")
        .unwrap()
        .unwrap();
    let (_, reconstructed) = load_canonical_message_from_node(
        node,
        row["_docID"].as_str().unwrap(),
        agent_did,
        requester_did,
    )
    .await
    .unwrap();
    let expected = match tool_call {
        Some((_, call_id)) => Message::User {
            content: vec![UserContent::ToolResult(ToolResult {
                id: call_id.into(),
                call_id: Some(call_id.into()),
                content: vec![ToolResultContent::Text(Text { text: text.into() })],
            })],
        },
        None => Message::user(text),
    };
    assert_eq!(reconstructed, expected);
}

#[tokio::test]
async fn history_reads_exact_principal_and_requester_scope() {
    let node = std::sync::Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
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
        import_history_observation(
            &node,
            &format!("request-scope-{index}"),
            "shared-session",
            owner,
            requester,
            text,
            &format!("scope-fixture-{index}"),
            index as u32 + 1,
            None,
        )
        .await;
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
async fn authored_keys_are_immutable_and_sequences_are_principal_scoped() {
    let node = std::sync::Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    ensure_runtime_schemas(&node).await.unwrap();
    for (owner, text) in [("did:key:owner", "local"), ("did:key:other", "foreign")] {
        let mut lifecycle = claimed_authored_request(&node, owner, owner, "same").await;
        let writer = crate::streaming::DefraStreamWriter::new(
            node.clone(),
            owner,
            std::time::Duration::ZERO,
        );
        lifecycle.begin_owned_execution(&writer).await.unwrap();
        let message = Message::user(text);
        let first = writer
            .publish_authored_message(&lifecycle, "prompt", &message)
            .await
            .unwrap();
        assert_eq!(
            writer
                .publish_authored_message(&lifecycle, "prompt", &message)
                .await
                .unwrap(),
            first
        );
        assert!(
            writer
                .publish_authored_message(&lifecycle, "prompt", &Message::user("replacement"))
                .await
                .is_err(),
            "an existing canonical key cannot overwrite immutable content"
        );
        let (header, native) = load_canonical_message_from_node(&node, &first, owner, None)
            .await
            .unwrap();
        assert_eq!(header.sequence, 1);
        assert_eq!(native, message);
        assert_eq!(
            load_history(&node, "same", owner, None).await.unwrap(),
            vec![message]
        );
        lifecycle
            .terminalize_owned(
                crate::lifecycle::RequestTerminalOutcome::Completed,
                gents_protocol::output::TerminalOutput::NoMessage,
                None,
            )
            .await
            .unwrap();
    }
    assert!(
        load_history(&node, "same", "did:key:owner", Some("did:key:requester"))
            .await
            .unwrap()
            .is_empty()
    );
    node.shutdown().await;
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

async fn claimed_authored_request(
    node: &std::sync::Arc<defra_node::EmbeddedNode>,
    request_id: &str,
    agent_did: &str,
    session_id: &str,
) -> crate::lifecycle::RequestLifecycle {
    use crate::lifecycle::{ClaimOutcome, RequestLifecycle};
    let request_id = crate::graphql::escape_graphql_string(request_id);
    let escaped_agent = crate::graphql::escape_graphql_string(agent_did);
    let session_id = crate::graphql::escape_graphql_string(session_id);
    let now = crate::graphql::escape_graphql_string(&chrono::Utc::now().to_rfc3339());
    let created = node
        .execute(&format!(
            r#"mutation {{ create_AgentRequest(input: {{
        request_id: "{request_id}", purpose: "normal", agent_did: "{escaped_agent}",
        behavior_id: "general", session_id: "{session_id}", content: "race",
        lifecycle_state: "pending", execution_origin: "interactive", created_at: "{now}",
        retry_count: 0, max_retries: 3, subagent_depth: 0
    }}) {{ _docID }} }}"#
        ))
        .await;
    assert!(!created.has_errors(), "{:?}", created.errors);
    let response = node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{request_id: {{_eq: "{request_id}"}}}}) {{ {} }} }}"#,
            crate::watcher::AGENT_REQUEST_FIELDS,
        ))
        .await;
    let request: gents_protocol::row::AgentRequestRow =
        crate::graphql::first_row(&response, "AgentRequest")
            .unwrap()
            .unwrap();
    let mut lifecycle = RequestLifecycle::new_with_agent_did(
        node.clone(),
        "general",
        agent_did,
        request.try_into().unwrap(),
        60,
    );
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    lifecycle
}

#[tokio::test]
async fn concurrent_keyed_appends_resolve_sequence_conflicts_without_duplicates() {
    use crate::streaming::DefraStreamWriter;
    use std::{sync::Arc, time::Duration};
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    ensure_runtime_schemas(&node).await.unwrap();
    let mut lifecycle =
        claimed_authored_request(&node, "append-race-request", "did:test:test", "append-race")
            .await;
    let writer = DefraStreamWriter::new(node.clone(), "did:test:test", Duration::ZERO);
    lifecycle.begin_owned_execution(&writer).await.unwrap();
    let left_message = Message::user("left");
    let right_message = Message::user("right");
    let (left, right) = tokio::join!(
        writer.publish_authored_message(&lifecycle, "left", &left_message),
        writer.publish_authored_message(&lifecycle, "right", &right_message),
    );
    let left = left.unwrap();
    let right = right.unwrap();
    assert_ne!(left, right);
    assert_eq!(
        writer
            .publish_authored_message(&lifecycle, "left", &left_message)
            .await
            .unwrap(),
        left
    );
    let (left_header, left_native) =
        load_canonical_message_from_node(&node, &left, "did:test:test", None)
            .await
            .unwrap();
    let (right_header, right_native) =
        load_canonical_message_from_node(&node, &right, "did:test:test", None)
            .await
            .unwrap();
    assert_ne!(left_header.sequence, right_header.sequence);
    let mut sequences = [left_header.sequence, right_header.sequence];
    sequences.sort();
    assert_eq!(
        sequences,
        [1, 2],
        "racing publications must not leave a sequence hole"
    );
    assert_eq!(left_native, left_message);
    assert_eq!(right_native, right_message);
    assert_eq!(
        load_history(&node, "append-race", "did:test:test", None)
            .await
            .unwrap()
            .len(),
        2
    );
    node.shutdown().await;
}

#[tokio::test]
async fn single_header_transaction_matches_bulk_coordinate_validation() {
    use crate::config_client::ConfigAccess;
    use canonical_rows::{
        decode_transcript_message_row, transcript_message_create_variables, AGENT_MESSAGE_FIELDS,
        CREATE_AGENT_MESSAGE_MUTATION,
    };
    use gents_protocol::output::MessagePublication;

    for case in [
        "unrelated",
        "key",
        "sequence",
        "both",
        "foreign",
        "requester",
        "fork",
    ] {
        let node = std::sync::Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
        ensure_runtime_schemas(&node).await.unwrap();
        import_history_observation(
            &node,
            "request",
            "session",
            "did:test:owner",
            None,
            "accepted",
            "accepted-key",
            1,
            None,
        )
        .await;
        ConfigAccess::transact_local(&node, None, "test.single_header_coordinates", |txn| {
            Box::pin(async move {
                let response = txn
                    .execute(&format!("{{ AgentMessage {{ {AGENT_MESSAGE_FIELDS} }} }}"))
                    .await?;
                let original = decode_transcript_message_row(&response["data"]["AgentMessage"][0])?;
                let mut noise = transcript_message_create_variables(&original.message)?;
                noise["input"]["message_key"] = serde_json::json!("unrelated-key");
                noise["input"]["sequence"] = serde_json::json!(99);
                noise["input"]["blocks"] = serde_json::json!({"invalid": "unrelated"});
                for index in 0..if case == "unrelated" { 49 } else { 1 } {
                    noise["input"]["message_key"] = serde_json::json!(format!("unrelated-{index}"));
                    noise["input"]["sequence"] = serde_json::json!(99 + index);
                    txn.execute_with_variables(CREATE_AGENT_MESSAGE_MUTATION, &noise)
                        .await?;
                }

                let mut other = original.message.clone();
                let mut target = original.doc_id.clone();
                match case {
                    "key" => other.sequence = 2,
                    "sequence" => other.message_key = "sequence-twin".into(),
                    "both" => other.native_id = Some("distinct-physical-twin".into()),
                    "foreign" => other.agent_did = "did:test:foreign".into(),
                    "requester" => other.requester_did = Some("did:test:requester".into()),
                    "fork" => {
                        other.session_id = "child-session".into();
                        other.message_key = "fork-key".into();
                        other.request_doc_id = None;
                        other.publication = MessagePublication::Fork {
                            origin_message_doc_id: original.doc_id.clone(),
                        };
                    }
                    _ => {}
                }
                if case != "unrelated" {
                    let created = txn
                        .execute_with_variables(
                            CREATE_AGENT_MESSAGE_MUTATION,
                            &transcript_message_create_variables(&other)?,
                        )
                        .await?;
                    assert_ne!(
                        crate::graphql::created_doc_id(&created, "AgentMessage")?,
                        original.doc_id,
                    );
                    if case == "fork" {
                        target = crate::graphql::created_doc_id(&created, "AgentMessage")?;
                    }
                }
                let single =
                    output::load_canonical_message_in_txn(txn, &target, "did:test:owner", None)
                        .await
                        .map_err(|error| format!("{error:#}"));
                let bulk = output::TxnCanonicalReader::new(txn, "did:test:owner", None)
                    .load_message(&target)
                    .await
                    .map_err(|error| format!("{error:#}"));
                assert_eq!(single, bulk, "single/bulk mismatch for {case}");
                assert_eq!(
                    single.is_err(),
                    matches!(case, "key" | "sequence" | "both"),
                    "{case}"
                );
                Ok(())
            })
        })
        .await
        .unwrap();
        node.shutdown().await;
    }
}

#[tokio::test]
async fn retry_frontier_matches_lean_admission() {
    let node = std::sync::Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    ensure_runtime_schemas(&node).await.unwrap();
    async fn write_doc(
        node: &std::sync::Arc<defra_node::EmbeddedNode>,
        mutation: &str,
        field: &str,
    ) -> String {
        let response = crate::config_client::ConfigAccess::Local(node.clone())
            .write("test.retry_frontier", mutation)
            .await
            .unwrap();
        let response: defra_node::QueryResponse = serde_json::from_value(response).unwrap();
        crate::graphql::single_mutation_document(&response, field)
            .unwrap()
            .unwrap()["_docID"]
            .as_str()
            .unwrap()
            .to_owned()
    }
    for (index, case) in crate::lean_vocab_test::lean_contract_snapshot()
        .retry_frontier_cases
        .iter()
        .enumerate()
    {
        let session_id = format!("retry-frontier-{index}");
        let session = escape_graphql_string(&session_id);
        let state = if case["scoped_terminal"].as_bool().unwrap() {
            "dead"
        } else {
            "processing"
        };
        let parent = write_doc(&node, &format!(r#"mutation {{ create_AgentRequest(input: {{
            request_id: "retry-frontier-parent-{index}", purpose: "normal", agent_did: "did:test:test",
            behavior_id: "general", session_id: "{session}", content: "Do the work",
            lifecycle_state: "{state}", failure_reason:"Stale", execution_origin: "interactive", created_at: "2026-10-07T00:00:00Z"
        }}) {{ _docID }} }}"#), "create_AgentRequest").await;
        let mut request = crate::watcher::AgentRequest::try_from(
            serde_json::from_value::<gents_protocol::row::AgentRequestRow>(serde_json::json!({
                "_docID":"successor", "request_id":"successor", "purpose":"normal",
                "agent_did":"did:test:test", "session_id":session_id, "behavior_id":"general",
                "content":"Do the work", "created_at":"2026-10-07T00:00:01Z",
                "retry_parent_request_doc_id":parent
            }))
            .unwrap(),
        )
        .unwrap();
        if !case["same_requester"].as_bool().unwrap() {
            request.requester_did = Some("did:test:other".into());
        }
        if !case["same_session"].as_bool().unwrap() {
            request.session_id = format!("fresh-session-{index}");
        }
        if case["published"].as_bool().unwrap() {
            import_history_observation(
                &node,
                &parent,
                &session_id,
                "did:test:test",
                None,
                "Do the work",
                &canonical_rows::authored_message_key(&parent, "prompt"),
                1,
                None,
            )
            .await;
        }
        let parent_gql = escape_graphql_string(&parent);
        if case["running"].as_bool().unwrap() {
            let mode = if case["background"].as_bool().unwrap() {
                "background"
            } else {
                "foreground"
            };
            write_doc(
                &node,
                &format!(
                    r#"mutation {{ create_AgentToolCall(input: {{
                tool_call_key:"retry-frontier-tool-{index}", request_doc_id:"{parent_gql}",
                lifecycle_state:"running", await_mode:"{mode}"
            }}) {{_docID}} }}"#
                ),
                "create_AgentToolCall",
            )
            .await;
        }
        let actual = history::retry_has_published_input(&node, &request).await;
        assert_eq!(actual.ok(), case["expected"].as_bool(), "{case}");
        if case["name"] == "ready" {
            request.retry_parent_request_doc_id = Some(
                write_doc(
                    &node,
                    &format!(
                        r#"mutation {{
                create_AgentRequest(input: {{ request_id:"retry-frontier-second", purpose:"normal",
                    agent_did:"did:test:test", behavior_id:"general", session_id:"{session}",
                    content:"Do the work", lifecycle_state:"failed", execution_origin:"interactive",
                    created_at:"2026-10-07T00:00:02Z", retry_parent_request_doc_id:"{parent_gql}"
                }}) {{_docID}} }}"#
                    ),
                    "create_AgentRequest",
                )
                .await,
            );
            assert_eq!(
                history::retry_has_published_input(&node, &request)
                    .await
                    .ok(),
                case["expected"].as_bool(),
                "retry chain must find the same authored input"
            );
        }
    }
    node.shutdown().await;
}
