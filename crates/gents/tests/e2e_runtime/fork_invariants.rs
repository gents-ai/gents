use std::sync::Arc;

use gents::adapter_projection::{
    build_adapter_projection, validate_adapter_projection_contract, AdapterProjectionKind,
    ProjectionContext,
};
use gents::config_client::ConfigAccess;
use gents::graphql::escape_graphql_string;
use gents::run_timeline_fetch::load_run_timeline;
use gents::session::{fork, fork_via_http, ForkError, ForkParams};

use crate::support::snapshots::fetch_compaction_entry_snapshots_for_session;
use crate::support::snapshots::fetch_message_snapshots_for_session;
use crate::support::snapshots::fetch_session_snapshot;
use crate::support::snapshots::fetch_tool_call_snapshots_for_session;
use crate::support::snapshots::fetch_tool_result_snapshots_for_session;
use crate::support::{
    create_agent_behavior, create_agent_message, create_agent_session, create_agent_tool_call,
    create_agent_tool_result, create_compaction_entry, create_request, test_db, AGENT_DID,
    AGENT_NAME,
};

async fn insert_fork_fixture(
    node: &defra_node::EmbeddedNode,
    collection: &str,
    input: serde_json::Value,
) -> String {
    let input = gents_protocol::graphql::graphql_input_literal(&input).unwrap();
    let response = node
        .execute(&format!(
            "mutation {{create_{collection}(input:{input}) {{_docID}}}}"
        ))
        .await;
    assert!(
        !response.has_errors(),
        "fixture {collection}: {:?}",
        response.errors
    );
    gents::graphql::single_mutation_document(&response, &format!("create_{collection}"))
        .unwrap()
        .unwrap()["_docID"]
        .as_str()
        .unwrap()
        .to_owned()
}

async fn set_tool_call_trace_fields(
    node: &defra_node::EmbeddedNode,
    session_id: &str,
    tool_call_id: &str,
    selected_service_id: &str,
    selected_tool_name: &str,
    tool_failure_class: &str,
    latency_ms: i64,
    started_at: &str,
    completed_at: &str,
) {
    let session_id = escape_graphql_string(session_id);
    let tool_call_id = escape_graphql_string(tool_call_id);
    let tool_call_key = format!("{session_id}:{tool_call_id}");
    let selected_service_id = escape_graphql_string(selected_service_id);
    let selected_tool_name = escape_graphql_string(selected_tool_name);
    let tool_failure_class = escape_graphql_string(tool_failure_class);
    let started_at = escape_graphql_string(started_at);
    let completed_at = escape_graphql_string(completed_at);
    let mutation = format!(
        r#"mutation {{
            update_AgentToolCall(
                filter: {{ tool_call_key: {{ _eq: "{tool_call_key}" }} }},
                input: {{
                    started_at: "{started_at}",
                    completed_at: "{completed_at}",
                    selected_service_id: "{selected_service_id}",
                    selected_tool_name: "{selected_tool_name}",
                    tool_failure_class: "{tool_failure_class}",
                    latency_ms: {latency_ms}
                }}
            ) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "set tool call trace fields failed: {:?}",
        resp.errors
    );
}

#[tokio::test]
async fn fork_copies_message_prefix_up_to_user_turn_boundary() {
    let db = test_db("fork-happy-path-messages").await;

    let parent_session = "parent-session";
    create_agent_session(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
    create_agent_behavior(&db.node, AGENT_NAME, AGENT_DID).await;

    create_agent_message(
        &db.node,
        parent_session,
        1,
        "user",
        "u1",
        "2026-04-21T10:00:01Z",
    )
    .await;
    create_agent_message(
        &db.node,
        parent_session,
        2,
        "assistant",
        "a1",
        "2026-04-21T10:00:02Z",
    )
    .await;
    create_agent_message(
        &db.node,
        parent_session,
        3,
        "user",
        "u2",
        "2026-04-21T10:00:03Z",
    )
    .await;
    create_agent_message(
        &db.node,
        parent_session,
        4,
        "assistant",
        "a2",
        "2026-04-21T10:00:04Z",
    )
    .await;
    create_agent_message(
        &db.node,
        parent_session,
        5,
        "user",
        "u3",
        "2026-04-21T10:00:05Z",
    )
    .await;
    create_agent_message(
        &db.node,
        parent_session,
        6,
        "assistant",
        "a3",
        "2026-04-21T10:00:06Z",
    )
    .await;

    let outcome = fork(
        &db.node,
        ForkParams {
            source_session_id: parent_session,
            fork_at_user_turn: 1,
            caller_agent_did: AGENT_DID,
            caller_requester_did: None,
            target_behavior_id: None,
        },
    )
    .await
    .expect("fork succeeds");

    let child_messages = fetch_message_snapshots_for_session(&db.node, &outcome.session_id).await;
    assert_eq!(
        child_messages.len(),
        2,
        "child should have 2 messages (u1, a1)"
    );
    assert_eq!(child_messages[0].sequence, 1);
    assert_eq!(child_messages[0].role, "user");
    assert_eq!(child_messages[0].content, "u1");
    assert_eq!(child_messages[0].timestamp, "2026-04-21T10:00:01Z");
    assert_eq!(child_messages[0].session_id, outcome.session_id);
    assert_eq!(
        child_messages[0].message_key,
        gents::session::sequence_message_key(AGENT_DID, &outcome.session_id, None, 1)
    );
    assert_eq!(child_messages[1].sequence, 2);
    assert_eq!(child_messages[1].role, "assistant");
    assert_eq!(child_messages[1].content, "a1");

    let parent_messages = fetch_message_snapshots_for_session(&db.node, parent_session).await;
    assert_eq!(parent_messages.len(), 6);

    assert_eq!(outcome.copied_messages, 2);
}

#[tokio::test]
async fn fork_via_http_copies_message_prefix_up_to_user_turn_boundary() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let node = Arc::new(
        defra_node::EmbeddedNode::builder()
            .with_http(defra_node::HttpConfig::with_addr(address))
            .build()
            .await
            .unwrap(),
    );
    gents::ensure_runtime_schemas(&node).await.unwrap();
    let graphql = format!("http://{address}/api/v0/graphql");
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if reqwest::Client::new()
                .post(&graphql)
                .json(&serde_json::json!({"query":"{__typename}"}))
                .send()
                .await
                .is_ok()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("native HTTP server starts");

    let parent_session = "parent-http-session";
    create_agent_session(&node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
    create_agent_behavior(&node, AGENT_NAME, AGENT_DID).await;

    create_agent_message(
        &node,
        parent_session,
        1,
        "user",
        "u1",
        "2026-04-21T10:00:01Z",
    )
    .await;
    create_agent_message(
        &node,
        parent_session,
        2,
        "assistant",
        "a1",
        "2026-04-21T10:00:02Z",
    )
    .await;
    create_agent_message(
        &node,
        parent_session,
        3,
        "user",
        "u2",
        "2026-04-21T10:00:03Z",
    )
    .await;

    let outcome = fork_via_http(
        &graphql,
        ForkParams {
            source_session_id: parent_session,
            fork_at_user_turn: 1,
            caller_agent_did: AGENT_DID,
            caller_requester_did: None,
            target_behavior_id: None,
        },
    )
    .await
    .expect("fork via http succeeds");

    let child_messages = fetch_message_snapshots_for_session(&node, &outcome.session_id).await;
    assert_eq!(child_messages.len(), 2);
    assert_eq!(child_messages[0].sequence, 1);
    assert_eq!(child_messages[0].role, "user");
    assert_eq!(child_messages[0].content, "u1");
    assert_eq!(child_messages[0].session_id, outcome.session_id);
    assert_eq!(child_messages[1].sequence, 2);
    assert_eq!(child_messages[1].role, "assistant");
    assert_eq!(child_messages[1].content, "a1");

    let child_session = fetch_session_snapshot(&node, &outcome.session_id)
        .await
        .expect("child canonical session exists");
    assert_eq!(
        child_session.requester_did, None,
        "absent requester scope is preserved exactly"
    );
    assert_eq!(
        Some(fork_origin(&child_session).source_session_id.as_str()),
        Some(parent_session)
    );
    assert_eq!(
        Some(i64::from(fork_origin(&child_session).at_user_turn)),
        Some(1)
    );
    assert!(
        !child_session.created_at.is_empty(),
        "child creation time records fork time"
    );

    assert_eq!(outcome.copied_messages, 2);
}

#[tokio::test]
async fn fork_copies_tool_calls_up_to_user_turn_boundary() {
    let db = test_db("fork-copy-tool-calls").await;

    let parent_session = "parent-tc";
    create_agent_session(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
    create_agent_behavior(&db.node, AGENT_NAME, AGENT_DID).await;

    create_agent_message(
        &db.node,
        parent_session,
        1,
        "user",
        "u1",
        "2026-04-21T10:00:01Z",
    )
    .await;
    create_agent_message(
        &db.node,
        parent_session,
        2,
        "assistant",
        "a1",
        "2026-04-21T10:00:02Z",
    )
    .await;
    create_agent_tool_call(
        &db.node,
        parent_session,
        2,
        "tc-1",
        "describe_tool",
        r#"{"service_id":"x-data","tool_name":"missing"}"#,
        "tool 'missing' not found on service 'x-data'. Available tools: search_posts",
        "completed",
        "2026-04-21T10:00:02Z",
        "2026-04-21T10:00:02.025Z",
    )
    .await;
    set_tool_call_trace_fields(
        &db.node,
        parent_session,
        "tc-1",
        "x-data",
        "missing",
        "tool_not_found",
        25,
        "2026-04-21T10:00:02Z",
        "2026-04-21T10:00:02.025Z",
    )
    .await;
    create_agent_message(
        &db.node,
        parent_session,
        3,
        "tool",
        "r1",
        "2026-04-21T10:00:03Z",
    )
    .await;
    create_agent_message(
        &db.node,
        parent_session,
        4,
        "user",
        "u2",
        "2026-04-21T10:00:04Z",
    )
    .await;
    create_agent_tool_call(
        &db.node,
        parent_session,
        4,
        "tc-2",
        "write_file",
        r#"{"path":"bar"}"#,
        "ok",
        "completed",
        "2026-04-21T10:00:04Z",
        "2026-04-21T10:00:04Z",
    )
    .await;
    create_agent_message(
        &db.node,
        parent_session,
        5,
        "assistant",
        "a2",
        "2026-04-21T10:00:05Z",
    )
    .await;

    let outcome = fork(
        &db.node,
        ForkParams {
            source_session_id: parent_session,
            fork_at_user_turn: 1,
            caller_agent_did: AGENT_DID,
            caller_requester_did: None,
            target_behavior_id: None,
        },
    )
    .await
    .expect("fork succeeds");

    let child_tool_calls =
        fetch_tool_call_snapshots_for_session(&db.node, &outcome.session_id).await;
    assert_eq!(
        child_tool_calls.len(),
        1,
        "only tc-1 (message_sequence=2) should be copied"
    );
    assert_eq!(child_tool_calls[0].tool_call_id, "tc-1");
    assert_eq!(child_tool_calls[0].message_sequence, 2);
    assert_eq!(child_tool_calls[0].session_id, outcome.session_id);
    assert_eq!(
        child_tool_calls[0].tool_call_key,
        format!("{}:tc-1", outcome.session_id)
    );
    assert_eq!(
        child_tool_calls[0].selected_service_id.as_deref(),
        Some("x-data")
    );
    assert_eq!(
        child_tool_calls[0].selected_tool_name.as_deref(),
        Some("missing")
    );
    assert_eq!(
        child_tool_calls[0].tool_failure_class.as_deref(),
        Some("tool_not_found")
    );
    assert_eq!(child_tool_calls[0].latency_ms, Some(25));

    assert_eq!(outcome.copied_tool_calls, 1);
}

#[tokio::test]
async fn fork_copies_spills_by_retained_call_not_creation_time() {
    let db = test_db("fork-copy-tool-results").await;

    let parent_session = "parent-tr";
    create_agent_session(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
    create_agent_behavior(&db.node, AGENT_NAME, AGENT_DID).await;

    create_agent_message(
        &db.node,
        parent_session,
        1,
        "user",
        "u1",
        "2026-04-21T10:00:01Z",
    )
    .await;
    create_agent_message(
        &db.node,
        parent_session,
        2,
        "user",
        "u2",
        "2026-04-21T10:00:03Z",
    )
    .await;
    let retained_call = create_agent_tool_call(
        &db.node,
        parent_session,
        1,
        "retained",
        "read_file",
        "{}",
        "early",
        "completed",
        "2026-04-21T10:00:01Z",
        "2026-04-21T10:00:01Z",
    )
    .await;
    let excluded_call = create_agent_tool_call(
        &db.node,
        parent_session,
        2,
        "excluded",
        "read_file",
        "{}",
        "late",
        "completed",
        "2026-04-21T10:00:03Z",
        "2026-04-21T10:00:03Z",
    )
    .await;
    create_agent_tool_result(
        &db.node,
        parent_session,
        &retained_call,
        "read_file",
        "{}",
        "early",
        "2026-04-21T10:00:09Z",
    )
    .await;
    create_agent_tool_result(
        &db.node,
        parent_session,
        &excluded_call,
        "read_file",
        "{}",
        "late",
        "2026-04-21T10:00:00Z",
    )
    .await;

    let outcome = fork(
        &db.node,
        ForkParams {
            source_session_id: parent_session,
            fork_at_user_turn: 1,
            caller_agent_did: AGENT_DID,
            caller_requester_did: None,
            target_behavior_id: None,
        },
    )
    .await
    .expect("fork succeeds");

    let child_results =
        fetch_tool_result_snapshots_for_session(&db.node, &outcome.session_id).await;
    assert_eq!(
        child_results.len(),
        1,
        "only the spill associated with the retained call should be copied"
    );
    assert_eq!(child_results[0].output_text, "early");
    assert_eq!(child_results[0].session_id, outcome.session_id);
    assert_eq!(child_results[0].agent_did, AGENT_DID);
    assert_eq!(child_results[0].tool_name, "read_file");
    assert_eq!(child_results[0].tool_input, "{}");
    assert!(!child_results[0].truncated);
    assert_eq!(child_results[0].truncation_metadata.as_deref(), Some(""));
    let child = escape_graphql_string(&outcome.session_id);
    let calls = db.node.execute(&format!(r#"{{
        AgentToolCall(filter: {{session_id: {{_eq: "{child}"}}}}) {{_docID tool_call_id request_id request_doc_id}}
    }}"#)).await;
    assert!(!calls.has_errors(), "{:?}", calls.errors);
    let calls = calls.data.as_ref().unwrap()["AgentToolCall"]
        .as_array()
        .unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["tool_call_id"], "retained");
    assert_eq!(
        child_results[0].tool_call_doc_id.as_deref(),
        calls[0]["_docID"].as_str()
    );
    assert_ne!(
        child_results[0].tool_call_doc_id.as_deref(),
        Some(retained_call.as_str())
    );
    assert!(calls[0]["request_id"].is_null() && calls[0]["request_doc_id"].is_null());
    assert_eq!(child_results[0].created_at, "2026-04-21T10:00:09Z");
    assert_eq!(outcome.copied_tool_results, 1);
}

#[tokio::test]
async fn fork_copies_compaction_cursor_with_retained_prefix() {
    let db = test_db("fork-copy-compactions").await;

    let parent_session = "parent-ce";
    create_agent_session(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
    create_agent_behavior(&db.node, AGENT_NAME, AGENT_DID).await;

    create_agent_message(
        &db.node,
        parent_session,
        1,
        "user",
        "u1",
        "2026-04-21T10:00:01Z",
    )
    .await;
    create_agent_message(
        &db.node,
        parent_session,
        2,
        "user",
        "u2",
        "2026-04-21T10:00:03Z",
    )
    .await;
    create_compaction_entry(
        &db.node,
        parent_session,
        1,
        "early summary",
        1,
        1,
        "2026-04-21T10:00:09Z",
    )
    .await;
    create_compaction_entry(
        &db.node,
        parent_session,
        2,
        "late summary",
        2,
        2,
        "2026-04-21T10:00:00Z",
    )
    .await;

    let outcome = fork(
        &db.node,
        ForkParams {
            source_session_id: parent_session,
            fork_at_user_turn: 1,
            caller_agent_did: AGENT_DID,
            caller_requester_did: None,
            target_behavior_id: None,
        },
    )
    .await
    .expect("fork succeeds");

    let child_compactions =
        fetch_compaction_entry_snapshots_for_session(&db.node, &outcome.session_id).await;
    assert_eq!(child_compactions.len(), 1);
    assert_eq!(child_compactions[0].summary, "early summary");
    assert_eq!(child_compactions[0].sequence, 1);
    assert_eq!(child_compactions[0].request_id, None);
    assert_eq!(child_compactions[0].compacted_through_sequence, Some(1));
    assert_eq!(child_compactions[0].request_doc_id, None);
    assert_eq!(
        child_compactions[0].compaction_key,
        gents::session::compaction_key(AGENT_DID, &outcome.session_id, None, 1)
    );
    assert_eq!(outcome.copied_compaction_entries, 1);
}

#[tokio::test]
async fn forked_history_drops_uncopied_physical_edges_and_remains_projectable() {
    let db = test_db("fork-history-physical-edges").await;
    let parent_session = "parent-physical-edges";
    let parent_request_id = "parent-physical-request";
    create_agent_session(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
    create_agent_behavior(&db.node, AGENT_NAME, AGENT_DID).await;
    let parent_request_doc_id = create_request(
        &db.node,
        parent_request_id,
        parent_session,
        "completed",
        "2026-04-21T10:00:00Z",
    )
    .await;

    let parent_session_escaped = escape_graphql_string(parent_session);
    let parent_request_id_escaped = escape_graphql_string(parent_request_id);
    let parent_request_doc_id_escaped = escape_graphql_string(&parent_request_doc_id);
    let tool_call_key = format!("{parent_session_escaped}:physical-tool");
    let source_compaction_key = escape_graphql_string(&gents::session::compaction_key(
        AGENT_DID,
        parent_session,
        None,
        1,
    ));
    let mutation = format!(
        r#"mutation {{
            user: create_AgentMessage(input: {{
                message_key: "{parent_session_escaped}:1",
                session_id: "{parent_session_escaped}",
                agent_did: "{AGENT_DID}",
                request_id: "{parent_request_id_escaped}",
                request_doc_id: "{parent_request_doc_id_escaped}",
                sequence: 1,
                role: "user",
                content: "fork this history",
                timestamp: "2026-04-21T10:00:01Z"
            }}) {{ _docID }}
            assistant: create_AgentMessage(input: {{
                message_key: "{parent_session_escaped}:2",
                session_id: "{parent_session_escaped}",
                agent_did: "{AGENT_DID}",
                request_id: "{parent_request_id_escaped}",
                request_doc_id: "{parent_request_doc_id_escaped}",
                sequence: 2,
                role: "assistant",
                content: "calling a tool",
                timestamp: "2026-04-21T10:00:02Z"
            }}) {{ _docID }}
            tool: create_AgentToolCall(input: {{
                tool_call_key: "{tool_call_key}",
                session_id: "{parent_session_escaped}",
                agent_did: "{AGENT_DID}",
                request_id: "{parent_request_id_escaped}",
                request_doc_id: "{parent_request_doc_id_escaped}",
                message_sequence: 2,
                tool_name: "read_file",
                tool_call_id: "physical-tool",
                args: "{{}}",
                result: "tool output",
                status: "completed",
                lifecycle_state: "completed",
                started_at: "2026-04-21T10:00:02Z",
                completed_at: "2026-04-21T10:00:03Z"
            }}) {{ _docID }}
            compaction: create_CompactionEntry(input: {{
                compaction_key: "{source_compaction_key}",
                session_id: "{parent_session_escaped}",
                agent_did: "{AGENT_DID}",
                request_id: "{parent_request_id_escaped}",
                request_doc_id: "{parent_request_doc_id_escaped}",
                sequence: 1,
                summary: "parent summary",
                files_read: "[]",
                files_modified: "[]",
                messages_compacted: 1,
                compacted_through_sequence: 1,
                original_tokens: 10,
                compacted_tokens: 5,
                created_at: "2026-04-21T10:00:02Z"
            }}) {{ _docID }}
            providerReduction: create_ProviderContextReduction(input: {{
                reduction_key: "parent-provider-reduction"
                agent_did: "{AGENT_DID}"
                requester_did: null
                session_id: "{parent_session_escaped}"
                request_id: "{parent_request_id_escaped}"
                request_doc_id: "{parent_request_doc_id_escaped}"
                request_commit_cid: "request-cid"
                reduction_index: 1
                turn_index: 0
                parent_reduction_key: null
                producer_call_id: null
                producer_call_seq: null
                source_boundary_json: "{{}}"
                compacted_prefix_json: "null"
                retained_suffix_json: "null"
                pair_closed: true
                checkpoint_messages_json: "null"
                summary: "parent provider summary"
                messages_compacted: 1
                original_tokens: 10
                compacted_tokens: 5
                created_at: "2026-04-21T10:00:02Z"
            }}) {{ _docID }}
        }}"#
    );
    let response = db.node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "create request-bound fork facts failed: {:?}",
        response.errors
    );
    let tool_query = format!(
        r#"{{
            AgentToolCall(filter: {{ tool_call_key: {{ _eq: "{tool_call_key}" }} }}, limit: 1) {{
                _docID
            }}
        }}"#
    );
    let response = db.node.execute(&tool_query).await;
    let parent_tool_call_doc_id = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .and_then(serde_json::Value::as_array)
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("_docID"))
        .and_then(serde_json::Value::as_str)
        .expect("parent AgentToolCall _docID");
    let result_mutation = format!(
        r#"mutation {{
            create_AgentToolResult(input: {{
                tool_call_doc_id: "{}",
                agent_did: "{AGENT_DID}",
                session_id: "{parent_session_escaped}",
                tool_name: "read_file",
                tool_input: "{{}}",
                output_text: "spilled output",
                truncated: false,
                truncation_metadata: "",
                created_at: "2026-04-21T10:00:03Z"
            }}) {{ _docID }}
        }}"#,
        escape_graphql_string(parent_tool_call_doc_id),
    );
    let response = db.node.execute(&result_mutation).await;
    assert!(
        !response.has_errors(),
        "create request-bound tool result failed: {:?}",
        response.errors
    );

    let outcome = fork(
        &db.node,
        ForkParams {
            source_session_id: parent_session,
            fork_at_user_turn: 1,
            caller_agent_did: AGENT_DID,
            caller_requester_did: None,
            target_behavior_id: None,
        },
    )
    .await
    .expect("fork succeeds");

    let child_session_escaped = escape_graphql_string(&outcome.session_id);
    let query = format!(
        r#"{{
            AgentMessage(filter: {{ session_id: {{ _eq: "{child_session_escaped}" }} }}) {{
                request_id request_doc_id
            }}
            AgentToolCall(filter: {{ session_id: {{ _eq: "{child_session_escaped}" }} }}) {{
                _docID request_id request_doc_id
            }}
            CompactionEntry(filter: {{ session_id: {{ _eq: "{child_session_escaped}" }} }}) {{
                request_id request_doc_id
            }}
            ProviderContextReduction(filter: {{ session_id: {{ _eq: "{child_session_escaped}" }} }}) {{
                reduction_key
            }}
            AgentToolResult(filter: {{ session_id: {{ _eq: "{child_session_escaped}" }} }}) {{
                tool_call_doc_id
            }}
        }}"#
    );
    let response = db.node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "query fork facts: {:?}",
        response.errors
    );
    let data = response.data.as_ref().expect("fork fact data");
    // Assert cardinality before per-row predicates: dropping a whole copied
    // collection must not satisfy detachment or spill-remapping vacuously.
    for (collection, expected) in [
        ("AgentMessage", 2),
        ("AgentToolCall", 1),
        ("CompactionEntry", 1),
        ("AgentToolResult", 1),
    ] {
        assert_eq!(
            data[collection]
                .as_array()
                .expect("copied collection")
                .len(),
            expected,
            "{collection} retained rows"
        );
    }
    for collection in ["AgentMessage", "AgentToolCall", "CompactionEntry"] {
        for row in data[collection]
            .as_array()
            .expect("fork request-scoped rows")
        {
            assert!(
                row["request_id"].is_null(),
                "copied history has no live request label"
            );
            assert!(
                row["request_doc_id"].is_null(),
                "copied history has no live physical request"
            );
        }
    }
    for row in data["AgentToolResult"]
        .as_array()
        .expect("fork tool-result rows")
    {
        let child_call = row["tool_call_doc_id"]
            .as_str()
            .expect("exact copied call link");
        assert_ne!(child_call, parent_tool_call_doc_id);
        assert!(
            data["AgentToolCall"]
                .as_array()
                .unwrap()
                .iter()
                .any(|call| call["_docID"].as_str() == Some(child_call)),
            "spill resolves to a copied child call, not source history"
        );
    }
    assert!(data["ProviderContextReduction"]
        .as_array()
        .expect("fork provider reductions")
        .is_empty());

    let child_request_id = "child-projection-request";
    create_request(
        &db.node,
        child_request_id,
        &outcome.session_id,
        "completed",
        "2026-04-21T10:00:04Z",
    )
    .await;
    let timeline = load_run_timeline(&ConfigAccess::Local(db.node.clone()), child_request_id)
        .await
        .expect("forked session timeline remains valid");
    let projection = build_adapter_projection(
        AdapterProjectionKind::OpenAiCodexRunTrace,
        &timeline,
        &ProjectionContext::default(),
    );
    validate_adapter_projection_contract(&projection)
        .expect("forked session projection remains contract-valid");
}

#[tokio::test]
async fn fork_batches_multiple_rows_for_all_copy_collections() {
    let db = test_db("fork-batch-copy-all").await;

    let parent_session = "parent-batch-copy";
    let requester = "did:key:fork-requester";
    let mut session =
        crate::support::session_document(parent_session, AGENT_NAME, "2026-04-21T10:00:00Z");
    session.requester_did = Some(requester.into());
    crate::support::create_session_document(&db.node, &session).await;
    create_agent_behavior(&db.node, AGENT_NAME, AGENT_DID).await;
    for (sequence, role, content) in [
        (1, "user", "u1"),
        (2, "assistant", "a1"),
        (3, "tool", "tool output"),
        (4, "user", "u2"),
    ] {
        insert_fork_fixture(&db.node, "AgentMessage", serde_json::json!({
            "message_key":gents::session::sequence_message_key(AGENT_DID,parent_session,Some(requester),sequence),
            "agent_did":AGENT_DID,"session_id":parent_session,"requester_did":requester,
            "sequence":sequence,"role":role,"content":content,"timestamp":"2026-04-21T10:00:00Z"
        })).await;
    }
    for i in 1..=3 {
        let timestamp = format!("2026-04-21T10:00:0{i}Z");
        let call = insert_fork_fixture(&db.node, "AgentToolCall", serde_json::json!({
            "tool_call_key":format!("{parent_session}:tc-{i}"),"tool_call_id":format!("tc-{i}"),
            "agent_did":AGENT_DID,"session_id":parent_session,"requester_did":requester,
            "message_sequence":i,"tool_name":"read_file","args":format!(r#"{{"index":{i}}}"#),
            "result":format!("tool-call-result-{i}"),"status":"completed","started_at":timestamp,"completed_at":timestamp
        })).await;
        insert_fork_fixture(&db.node, "AgentToolResult", serde_json::json!({
            "agent_did":AGENT_DID,"session_id":parent_session,"requester_did":requester,
            "tool_call_doc_id":call,"tool_name":"read_file","tool_input":format!(r#"{{"path":"file-{i}.txt"}}"#),
            "output_text":format!("tool-result-{i}"),"truncated":false,"created_at":timestamp
        })).await;
        insert_fork_fixture(&db.node, "CompactionEntry", serde_json::json!({
            "compaction_key":gents::session::compaction_key(AGENT_DID,parent_session,Some(requester),i),
            "agent_did":AGENT_DID,"session_id":parent_session,"requester_did":requester,
            "sequence":i,"summary":format!("summary-{i}"),"files_read":"[]","files_modified":"[]",
            "messages_compacted":i,"compacted_through_sequence":i,"original_tokens":100,"compacted_tokens":50,"created_at":timestamp
        })).await;
    }
    let outcome = fork(
        &db.node,
        ForkParams {
            source_session_id: parent_session,
            fork_at_user_turn: 1,
            caller_agent_did: AGENT_DID,
            caller_requester_did: Some("did:key:fork-requester"),
            target_behavior_id: None,
        },
    )
    .await
    .expect("fork succeeds");

    assert_eq!(outcome.copied_messages, 3);
    assert_eq!(outcome.copied_tool_calls, 3);
    assert_eq!(outcome.copied_tool_results, 3);
    assert_eq!(outcome.copied_compaction_entries, 3);

    let child_messages = fetch_message_snapshots_for_session(&db.node, &outcome.session_id).await;
    assert_eq!(child_messages.len(), 3);
    assert_eq!(
        child_messages
            .iter()
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>(),
        vec!["u1", "a1", "tool output"]
    );

    let child_tool_calls =
        fetch_tool_call_snapshots_for_session(&db.node, &outcome.session_id).await;
    assert_eq!(child_tool_calls.len(), 3);
    assert_eq!(
        child_tool_calls
            .iter()
            .map(|tool_call| tool_call.tool_call_id.as_str())
            .collect::<Vec<_>>(),
        vec!["tc-1", "tc-2", "tc-3"]
    );

    let child_tool_results =
        fetch_tool_result_snapshots_for_session(&db.node, &outcome.session_id).await;
    assert_eq!(child_tool_results.len(), 3);
    assert_eq!(
        child_tool_results
            .iter()
            .map(|tool_result| tool_result.output_text.as_str())
            .collect::<Vec<_>>(),
        vec!["tool-result-1", "tool-result-2", "tool-result-3"]
    );

    let child_compactions =
        fetch_compaction_entry_snapshots_for_session(&db.node, &outcome.session_id).await;
    assert_eq!(child_compactions.len(), 3);
    for (index, compaction) in child_compactions.iter().enumerate() {
        let sequence = (index + 1) as u32;
        assert_eq!(compaction.sequence, sequence);
        assert_eq!(compaction.summary, format!("summary-{sequence}"));
        assert_eq!(
            compaction.compaction_key,
            gents::session::compaction_key(
                AGENT_DID,
                &outcome.session_id,
                Some("did:key:fork-requester"),
                sequence
            )
        );
    }
    for collection in [
        "AgentSession",
        "AgentMessage",
        "AgentToolCall",
        "AgentToolResult",
        "CompactionEntry",
    ] {
        let child = escape_graphql_string(&outcome.session_id);
        let response = db.node.execute(&format!(r#"{{ {collection}(filter: {{session_id: {{_eq: "{child}"}}}}) {{requester_did}} }}"#)).await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        let rows = response.data.as_ref().unwrap()[collection]
            .as_array()
            .expect("child rows");
        assert!(!rows.is_empty(), "{collection} was copied");
        for row in rows {
            assert_eq!(
                row["requester_did"], "did:key:fork-requester",
                "{collection} exact requester"
            );
        }
    }
}

#[tokio::test]
async fn fork_rejects_source_with_non_terminal_request() {
    let db = test_db("fork-busy-source").await;

    let parent_session = "parent-busy";
    create_agent_session(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
    create_agent_behavior(&db.node, AGENT_NAME, AGENT_DID).await;
    create_agent_message(
        &db.node,
        parent_session,
        1,
        "user",
        "u1",
        "2026-04-21T10:00:01Z",
    )
    .await;

    create_request(
        &db.node,
        "req-pending",
        parent_session,
        "pending",
        "2026-04-21T10:00:02Z",
    )
    .await;

    let err = fork(
        &db.node,
        ForkParams {
            source_session_id: parent_session,
            fork_at_user_turn: 0,
            caller_agent_did: AGENT_DID,
            caller_requester_did: None,
            target_behavior_id: None,
        },
    )
    .await
    .expect_err("fork must reject busy source");

    assert!(
        matches!(err, ForkError::ForkSourceBusy),
        "expected ForkSourceBusy, got {:?}",
        err
    );
}

#[tokio::test]
async fn fork_rejects_mismatched_caller_principal() {
    let db = test_db("fork-wrong-principal").await;

    let parent_session = "parent-wp";
    create_agent_session(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
    create_agent_behavior(&db.node, AGENT_NAME, AGENT_DID).await;
    create_agent_message(
        &db.node,
        parent_session,
        1,
        "user",
        "u1",
        "2026-04-21T10:00:01Z",
    )
    .await;

    let err = fork(
        &db.node,
        ForkParams {
            source_session_id: parent_session,
            fork_at_user_turn: 0,
            caller_agent_did: "did:test:someone-else",
            caller_requester_did: None,
            target_behavior_id: None,
        },
    )
    .await
    .expect_err("fork must reject mismatched principal");

    assert!(
        matches!(err, ForkError::ForkSourceNotFound(_)),
        "foreign principal must not resolve the source, got {:?}",
        err
    );
}

#[tokio::test]
async fn fork_accepts_behavior_swap_within_same_principal() {
    let db = test_db("fork-behavior-swap-ok").await;

    let parent_session = "parent-swap-ok";
    create_agent_session(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
    create_agent_behavior(&db.node, AGENT_NAME, AGENT_DID).await;
    create_agent_behavior(&db.node, "alt-behavior", AGENT_DID).await;
    create_agent_message(
        &db.node,
        parent_session,
        1,
        "user",
        "u1",
        "2026-04-21T10:00:01Z",
    )
    .await;

    let outcome = fork(
        &db.node,
        ForkParams {
            source_session_id: parent_session,
            fork_at_user_turn: 0,
            caller_agent_did: AGENT_DID,
            caller_requester_did: None,
            target_behavior_id: Some("alt-behavior"),
        },
    )
    .await
    .expect("fork with matching-principal behavior succeeds");

    let child_session =
        crate::support::snapshots::fetch_session_snapshot(&db.node, &outcome.session_id)
            .await
            .expect("child canonical session exists");
    assert_eq!(
        child_session.requester_did, None,
        "absent requester scope is preserved exactly"
    );
    assert_eq!(child_session.behavior_id, "alt-behavior");
}

#[tokio::test]
async fn fork_rejects_behavior_owned_by_different_principal() {
    let db = test_db("fork-behavior-swap-bad").await;

    let parent_session = "parent-swap-bad";
    create_agent_session(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
    create_agent_behavior(&db.node, AGENT_NAME, AGENT_DID).await;
    create_agent_behavior(&db.node, "foreign-behavior", "did:test:someone-else").await;
    create_agent_message(
        &db.node,
        parent_session,
        1,
        "user",
        "u1",
        "2026-04-21T10:00:01Z",
    )
    .await;

    let err = fork(
        &db.node,
        ForkParams {
            source_session_id: parent_session,
            fork_at_user_turn: 0,
            caller_agent_did: AGENT_DID,
            caller_requester_did: None,
            target_behavior_id: Some("foreign-behavior"),
        },
    )
    .await
    .expect_err("fork must reject cross-principal behavior swap");

    assert!(
        matches!(err, ForkError::ForkBehaviorNotFound(_)),
        "foreign behavior must not resolve within caller scope, got {:?}",
        err
    );
}

#[tokio::test]
async fn fork_rejects_out_of_range_user_turn() {
    let db = test_db("fork-oor").await;

    let parent_session = "parent-oor";
    create_agent_session(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
    create_agent_behavior(&db.node, AGENT_NAME, AGENT_DID).await;
    create_agent_message(
        &db.node,
        parent_session,
        1,
        "user",
        "u1",
        "2026-04-21T10:00:01Z",
    )
    .await;
    create_agent_message(
        &db.node,
        parent_session,
        2,
        "assistant",
        "a1",
        "2026-04-21T10:00:02Z",
    )
    .await;

    let err = fork(
        &db.node,
        ForkParams {
            source_session_id: parent_session,
            fork_at_user_turn: 5,
            caller_agent_did: AGENT_DID,
            caller_requester_did: None,
            target_behavior_id: None,
        },
    )
    .await
    .expect_err("fork must reject out-of-range user turn");

    assert!(
        matches!(err, ForkError::ForkAtUserTurnOutOfRange(5, 1)),
        "expected ForkAtUserTurnOutOfRange(5, 1), got {:?}",
        err
    );

    for collection in [
        "AgentMessage",
        "AgentSession",
        "AgentToolCall",
        "AgentToolResult",
        "CompactionEntry",
    ] {
        let query = format!(
            r#"{{
                {collection}(filter: {{ session_id: {{ _neq: "{parent_session}" }} }}) {{ session_id }}
            }}"#
        );
        let resp = db.node.execute(&query).await;
        let rows = resp
            .data
            .as_ref()
            .and_then(|d| d.get(collection))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        assert!(
            rows.is_empty(),
            "out-of-range fork must not create orphan {collection} rows: got {:?}",
            rows
        );
    }
}

#[tokio::test]
async fn fork_at_user_turn_zero_produces_empty_child_with_provenance() {
    let db = test_db("fork-user-turn-zero").await;

    let parent_session = "parent-zero";
    create_agent_session(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
    create_agent_behavior(&db.node, AGENT_NAME, AGENT_DID).await;
    create_agent_message(
        &db.node,
        parent_session,
        1,
        "user",
        "u1",
        "2026-04-21T10:00:01Z",
    )
    .await;
    create_agent_message(
        &db.node,
        parent_session,
        2,
        "assistant",
        "a1",
        "2026-04-21T10:00:02Z",
    )
    .await;

    let outcome = fork(
        &db.node,
        ForkParams {
            source_session_id: parent_session,
            fork_at_user_turn: 0,
            caller_agent_did: AGENT_DID,
            caller_requester_did: None,
            target_behavior_id: None,
        },
    )
    .await
    .expect("fork at user-turn 0 succeeds");

    assert_eq!(outcome.copied_messages, 0);
    assert_eq!(outcome.copied_tool_calls, 0);
    assert_eq!(outcome.copied_tool_results, 0);
    assert_eq!(outcome.copied_compaction_entries, 0);

    let child_messages = fetch_message_snapshots_for_session(&db.node, &outcome.session_id).await;
    assert!(child_messages.is_empty());

    let child_session =
        crate::support::snapshots::fetch_session_snapshot(&db.node, &outcome.session_id)
            .await
            .expect("child canonical session exists");
    assert_eq!(
        child_session.requester_did, None,
        "absent requester scope is preserved exactly"
    );
    assert_eq!(
        Some(fork_origin(&child_session).source_session_id.as_str()),
        Some(parent_session)
    );
    assert_eq!(
        Some(i64::from(fork_origin(&child_session).at_user_turn)),
        Some(0)
    );
    assert!(
        !child_session.created_at.is_empty(),
        "child creation time records fork time"
    );
}

#[tokio::test]
async fn fork_at_total_user_turns_copies_full_history() {
    let db = test_db("fork-end-of-history").await;

    let parent_session = "parent-end";
    create_agent_session(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
    create_agent_behavior(&db.node, AGENT_NAME, AGENT_DID).await;
    create_agent_message(
        &db.node,
        parent_session,
        1,
        "user",
        "u1",
        "2026-04-21T10:00:01Z",
    )
    .await;
    create_agent_message(
        &db.node,
        parent_session,
        2,
        "assistant",
        "a1",
        "2026-04-21T10:00:02Z",
    )
    .await;
    create_agent_message(
        &db.node,
        parent_session,
        3,
        "user",
        "u2",
        "2026-04-21T10:00:03Z",
    )
    .await;
    create_agent_message(
        &db.node,
        parent_session,
        4,
        "assistant",
        "a2",
        "2026-04-21T10:00:04Z",
    )
    .await;

    let outcome = fork(
        &db.node,
        ForkParams {
            source_session_id: parent_session,
            fork_at_user_turn: 2,
            caller_agent_did: AGENT_DID,
            caller_requester_did: None,
            target_behavior_id: None,
        },
    )
    .await
    .expect("fork at end of history succeeds");

    assert_eq!(outcome.copied_messages, 4);
    let child_messages = fetch_message_snapshots_for_session(&db.node, &outcome.session_id).await;
    assert_eq!(child_messages.len(), 4);
    assert_eq!(child_messages[0].content, "u1");
    assert_eq!(child_messages[1].content, "a1");
    assert_eq!(child_messages[2].content, "u2");
    assert_eq!(child_messages[3].content, "a2");

    let child_session =
        crate::support::snapshots::fetch_session_snapshot(&db.node, &outcome.session_id)
            .await
            .expect("child canonical session exists");
    assert_eq!(
        child_session.requester_did, None,
        "absent requester scope is preserved exactly"
    );
    assert_eq!(
        Some(fork_origin(&child_session).source_session_id.as_str()),
        Some(parent_session)
    );
    assert_eq!(
        Some(i64::from(fork_origin(&child_session).at_user_turn)),
        Some(2)
    );
}

#[tokio::test]
async fn fork_preserves_parent_session_and_message_fields() {
    let db = test_db("fork-parent-unchanged").await;

    let parent_session = "parent-unchanged";
    let mut parent =
        crate::support::session_document(parent_session, AGENT_NAME, "2026-04-21T10:00:00Z");
    parent.title = Some(gents_protocol::session::SessionTitle {
        text: "Keep my title".into(),
        source: gents_protocol::session::SessionTitleSource::User,
    });
    parent.tags = vec!["audit".into(), "fork-parent".into()];
    parent.provenance = Some(gents_protocol::session::SessionProvenance {
        task_id: Some("task-source".into()),
        graph_run_id: Some("graph-source".into()),
        ..Default::default()
    });
    crate::support::create_session_document(&db.node, &parent).await;
    create_agent_behavior(&db.node, AGENT_NAME, AGENT_DID).await;

    for (i, role) in [
        (1u32, "user"),
        (2, "assistant"),
        (3, "tool"),
        (4, "user"),
        (5, "assistant"),
    ] {
        let ts = format!("2026-04-21T10:00:0{i}Z");
        create_agent_message(&db.node, parent_session, i, role, &format!("msg{i}"), &ts).await;
    }

    let before_messages = fetch_message_snapshots_for_session(&db.node, parent_session).await;
    let before_session =
        crate::support::snapshots::fetch_session_snapshot(&db.node, parent_session).await;

    let _ = fork(
        &db.node,
        ForkParams {
            source_session_id: parent_session,
            fork_at_user_turn: 1,
            caller_agent_did: AGENT_DID,
            caller_requester_did: None,
            target_behavior_id: None,
        },
    )
    .await
    .expect("fork succeeds");

    let after_messages = fetch_message_snapshots_for_session(&db.node, parent_session).await;
    let after_session =
        crate::support::snapshots::fetch_session_snapshot(&db.node, parent_session).await;

    assert_eq!(
        before_messages, after_messages,
        "parent AgentMessage rows unchanged"
    );
    assert_eq!(
        before_session, after_session,
        "parent canonical session unchanged"
    );
}

#[tokio::test]
async fn concurrent_forks_of_same_parent_produce_disjoint_children() {
    let db = test_db("fork-concurrent").await;

    let parent_session = "parent-concurrent";
    create_agent_session(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
    create_agent_behavior(&db.node, AGENT_NAME, AGENT_DID).await;
    create_agent_message(
        &db.node,
        parent_session,
        1,
        "user",
        "u1",
        "2026-04-21T10:00:01Z",
    )
    .await;
    create_agent_message(
        &db.node,
        parent_session,
        2,
        "assistant",
        "a1",
        "2026-04-21T10:00:02Z",
    )
    .await;
    create_agent_message(
        &db.node,
        parent_session,
        3,
        "user",
        "u2",
        "2026-04-21T10:00:03Z",
    )
    .await;

    let node = db.node.clone();
    let parent_session_a = parent_session.to_string();
    let parent_session_b = parent_session.to_string();
    let node_a = node.clone();
    let node_b = node.clone();

    let handle_a = tokio::spawn(async move {
        fork(
            &node_a,
            ForkParams {
                source_session_id: &parent_session_a,
                fork_at_user_turn: 0,
                caller_agent_did: AGENT_DID,
                caller_requester_did: None,
                target_behavior_id: None,
            },
        )
        .await
    });
    let handle_b = tokio::spawn(async move {
        fork(
            &node_b,
            ForkParams {
                source_session_id: &parent_session_b,
                fork_at_user_turn: 1,
                caller_agent_did: AGENT_DID,
                caller_requester_did: None,
                target_behavior_id: None,
            },
        )
        .await
    });

    let outcome_a = handle_a
        .await
        .expect("task a panicked")
        .expect("fork a succeeds");
    let outcome_b = handle_b
        .await
        .expect("task b panicked")
        .expect("fork b succeeds");

    assert_ne!(outcome_a.session_id, outcome_b.session_id);
    assert_eq!(outcome_a.copied_messages, 0);
    assert_eq!(outcome_b.copied_messages, 2);
}

#[tokio::test]
async fn fork_rejects_nonexistent_source_session() {
    let db = test_db("fork-source-not-found").await;

    let err = fork(
        &db.node,
        ForkParams {
            source_session_id: "does-not-exist",
            fork_at_user_turn: 0,
            caller_agent_did: AGENT_DID,
            caller_requester_did: None,
            target_behavior_id: None,
        },
    )
    .await
    .expect_err("fork must reject unknown source");

    assert!(
        matches!(err, ForkError::ForkSourceNotFound(ref id) if id == "does-not-exist"),
        "expected ForkSourceNotFound(\"does-not-exist\"), got {:?}",
        err
    );
}

#[tokio::test]
async fn fork_rejects_unknown_target_behavior() {
    let db = test_db("fork-behavior-not-found").await;

    let parent_session = "parent-unknown-behavior";
    create_agent_session(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
    create_agent_behavior(&db.node, AGENT_NAME, AGENT_DID).await;
    create_agent_message(
        &db.node,
        parent_session,
        1,
        "user",
        "u1",
        "2026-04-21T10:00:01Z",
    )
    .await;

    let err = fork(
        &db.node,
        ForkParams {
            source_session_id: parent_session,
            fork_at_user_turn: 0,
            caller_agent_did: AGENT_DID,
            caller_requester_did: None,
            target_behavior_id: Some("no-such-behavior"),
        },
    )
    .await
    .expect_err("fork must reject unknown target behavior");

    assert!(
        matches!(err, ForkError::ForkBehaviorNotFound(ref id) if id == "no-such-behavior"),
        "expected ForkBehaviorNotFound(\"no-such-behavior\"), got {:?}",
        err
    );
}

#[tokio::test]
async fn fork_of_fork_links_to_immediate_parent_not_grandparent() {
    let db = test_db("fork-of-fork").await;

    let grandparent_session = "grandparent";
    create_agent_session(
        &db.node,
        grandparent_session,
        AGENT_NAME,
        "2026-04-21T10:00:00Z",
    )
    .await;
    create_agent_behavior(&db.node, AGENT_NAME, AGENT_DID).await;

    create_agent_message(
        &db.node,
        grandparent_session,
        1,
        "user",
        "gp_u1",
        "2026-04-21T10:00:01Z",
    )
    .await;
    create_agent_message(
        &db.node,
        grandparent_session,
        2,
        "assistant",
        "gp_a1",
        "2026-04-21T10:00:02Z",
    )
    .await;
    create_agent_message(
        &db.node,
        grandparent_session,
        3,
        "user",
        "gp_u2",
        "2026-04-21T10:00:03Z",
    )
    .await;
    create_agent_message(
        &db.node,
        grandparent_session,
        4,
        "assistant",
        "gp_a2",
        "2026-04-21T10:00:04Z",
    )
    .await;

    let child_outcome = fork(
        &db.node,
        ForkParams {
            source_session_id: grandparent_session,
            fork_at_user_turn: 1,
            caller_agent_did: AGENT_DID,
            caller_requester_did: None,
            target_behavior_id: None,
        },
    )
    .await
    .expect("child fork succeeds");
    assert_eq!(child_outcome.copied_messages, 2);

    create_agent_message(
        &db.node,
        &child_outcome.session_id,
        3,
        "user",
        "child_u2",
        "2026-04-21T10:10:00Z",
    )
    .await;
    create_agent_message(
        &db.node,
        &child_outcome.session_id,
        4,
        "assistant",
        "child_a2",
        "2026-04-21T10:10:01Z",
    )
    .await;

    let grandchild_outcome = fork(
        &db.node,
        ForkParams {
            source_session_id: &child_outcome.session_id,
            fork_at_user_turn: 1,
            caller_agent_did: AGENT_DID,
            caller_requester_did: None,
            target_behavior_id: None,
        },
    )
    .await
    .expect("grandchild fork succeeds");
    assert_eq!(
        grandchild_outcome.copied_messages, 2,
        "grandchild inherits child's prefix (which is grandparent's prefix)"
    );

    let grandchild_session =
        crate::support::snapshots::fetch_session_snapshot(&db.node, &grandchild_outcome.session_id)
            .await
            .expect("grandchild canonical session exists");
    assert_eq!(
        Some(fork_origin(&grandchild_session).source_session_id.as_str()),
        Some(child_outcome.session_id.as_str()),
        "grandchild must record its immediate parent (child), not its grandparent"
    );
    assert_eq!(
        Some(i64::from(fork_origin(&grandchild_session).at_user_turn)),
        Some(1)
    );

    let grandchild_messages =
        fetch_message_snapshots_for_session(&db.node, &grandchild_outcome.session_id).await;
    assert_eq!(grandchild_messages.len(), 2);
    assert_eq!(grandchild_messages[0].content, "gp_u1");
    assert_eq!(grandchild_messages[1].content, "gp_a1");
    assert_eq!(
        grandchild_messages[0].session_id,
        grandchild_outcome.session_id
    );
    assert_eq!(
        grandchild_messages[0].message_key,
        gents::session::sequence_message_key(AGENT_DID, &grandchild_outcome.session_id, None, 1)
    );

    assert!(!grandchild_messages
        .iter()
        .any(|m| m.session_id == child_outcome.session_id));
}

fn fork_origin(
    session: &gents_protocol::session::AgentSession,
) -> &gents_protocol::session::SessionFork {
    session
        .provenance
        .as_ref()
        .and_then(|p| p.fork.as_ref())
        .expect("canonical fork provenance")
}

#[tokio::test]
async fn fork_rejects_nonexistent_call_associations_and_compaction_prefixes() {
    for malformed in ["call", "cursor"] {
        let db = test_db(&format!("fork-invalid-{malformed}")).await;
        let parent = "source";
        create_agent_session(&db.node, parent, AGENT_NAME, "2026-04-21T10:00:00Z").await;
        create_agent_behavior(&db.node, AGENT_NAME, AGENT_DID).await;
        create_agent_message(&db.node, parent, 1, "user", "one", "2026-04-21T10:00:01Z").await;
        if malformed == "call" {
            create_agent_tool_call(
                &db.node,
                parent,
                99,
                "orphan",
                "read_file",
                "{}",
                "done",
                "completed",
                "2026-04-21T10:00:01Z",
                "2026-04-21T10:00:01Z",
            )
            .await;
        } else {
            create_compaction_entry(
                &db.node,
                parent,
                1,
                "invalid prefix",
                1,
                99,
                "2026-04-21T10:00:01Z",
            )
            .await;
        }
        assert!(
            fork(
                &db.node,
                ForkParams {
                    source_session_id: parent,
                    fork_at_user_turn: 1,
                    caller_agent_did: AGENT_DID,
                    caller_requester_did: None,
                    target_behavior_id: None
                }
            )
            .await
            .is_err(),
            "{malformed}: malformed source must fail before publishing a child"
        );
        let response = db.node.execute("{ AgentSession { session_id } }").await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        let rows = response
            .data
            .as_ref()
            .and_then(|v| v.get("AgentSession"))
            .and_then(serde_json::Value::as_array)
            .expect("sessions");
        assert_eq!(rows.len(), 1, "{malformed}: failed fork leaves no child");
        assert_eq!(rows[0]["session_id"], parent);
        // A copier can fail before creating the session yet leave orphan rows.
        // Rejection must publish neither a child session nor child history.
        for collection in [
            "AgentMessage",
            "AgentToolCall",
            "AgentToolResult",
            "CompactionEntry",
        ] {
            let response = db
                .node
                .execute(&format!("{{ {collection} {{ session_id }} }}"))
                .await;
            assert!(
                !response.has_errors(),
                "{collection}: {:?}",
                response.errors
            );
            for row in response.data.as_ref().unwrap()[collection]
                .as_array()
                .unwrap()
            {
                assert_eq!(
                    row["session_id"], parent,
                    "{malformed}: orphan {collection} after failed fork"
                );
            }
        }
    }
}

#[tokio::test]
async fn fork_rolls_back_earlier_copies_when_later_copy_identity_conflicts() {
    let db = test_db("fork-atomic-copy-failure").await;
    let parent = "parent-copy-conflict";
    create_agent_session(&db.node, parent, AGENT_NAME, "2026-04-21T10:00:00Z").await;
    create_agent_behavior(&db.node, AGENT_NAME, AGENT_DID).await;
    create_agent_message(&db.node, parent, 1, "user", "u1", "2026-04-21T10:00:01Z").await;
    // Distinct physical source calls collide in the destination's actual unique
    // tool-call key. The first copy must not survive the later failure.
    for source_key in ["source-call-a", "source-call-b"] {
        insert_fork_fixture(
            &db.node,
            "AgentToolCall",
            serde_json::json!({
                "tool_call_key":source_key,"session_id":parent,"agent_did":AGENT_DID,
                "requester_did":null,"message_sequence":1,"tool_call_id":"same-call",
                "tool_name":"read_file","args":"{}","result":"output","status":"completed",
                "started_at":"2026-04-21T10:00:01Z","completed_at":"2026-04-21T10:00:02Z"
            }),
        )
        .await;
    }
    assert!(fork(
        &db.node,
        ForkParams {
            source_session_id: parent,
            fork_at_user_turn: 1,
            caller_agent_did: AGENT_DID,
            caller_requester_did: None,
            target_behavior_id: None
        }
    )
    .await
    .is_err());
    for (collection, count) in [
        ("AgentSession", 1),
        ("AgentMessage", 1),
        ("AgentToolCall", 2),
        ("AgentToolResult", 0),
        ("CompactionEntry", 0),
    ] {
        let response = db
            .node
            .execute(&format!("{{{collection}{{session_id}}}}"))
            .await;
        assert!(
            !response.has_errors(),
            "{collection}: {:?}",
            response.errors
        );
        let rows = response.data.as_ref().unwrap()[collection]
            .as_array()
            .unwrap();
        assert_eq!(
            rows.len(),
            count,
            "{collection} must roll back all child rows"
        );
        assert!(rows.iter().all(|row| row["session_id"] == parent));
    }
}
