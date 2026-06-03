use std::sync::Arc;

use axum::{extract::State, routing::post, Json, Router};
use defra_agent::graphql::escape_graphql_string;
use defra_agent::session::{fork, fork_via_http, ForkError, ForkParams};
use serde::Deserialize;
use tokio::net::TcpListener;

mod support;

use support::snapshots::fetch_compaction_entry_snapshots_for_session;
use support::snapshots::fetch_conversation_snapshot;
use support::snapshots::fetch_message_snapshots_for_session;
use support::snapshots::fetch_tool_call_snapshots_for_session;
use support::snapshots::fetch_tool_result_snapshots_for_session;
use support::{
    create_agent_behavior, create_agent_conversation, create_agent_message, create_agent_session,
    create_agent_tool_call, create_agent_tool_result, create_compaction_entry, create_request,
    test_db, AGENT_DID, AGENT_NAME,
};

#[derive(Clone)]
struct EmbeddedGraphqlState {
    node: Arc<defra_node::EmbeddedNode>,
}

#[derive(Deserialize)]
struct GraphqlRequest {
    query: String,
}

async fn embedded_graphql_handler(
    State(state): State<EmbeddedGraphqlState>,
    Json(request): Json<GraphqlRequest>,
) -> Json<serde_json::Value> {
    let response = state.node.execute(&request.query).await;
    Json(serde_json::json!({
        "data": response.data,
        "errors": response.errors,
    }))
}

async fn spawn_embedded_graphql(node: Arc<defra_node::EmbeddedNode>) -> String {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind embedded graphql listener");
    let addr = listener
        .local_addr()
        .expect("read embedded graphql listener addr");
    let router = Router::new()
        .route("/api/v0/graphql", post(embedded_graphql_handler))
        .with_state(EmbeddedGraphqlState { node });

    tokio::spawn(async move {
        axum::serve(listener, router)
            .await
            .expect("serve embedded graphql");
    });

    format!("http://{addr}/api/v0/graphql")
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

    // Parent session with three user turns interleaved with assistant replies.
    let parent_session = "parent-session";
    create_agent_session(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
    create_agent_conversation(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
    create_agent_behavior(&db.node, AGENT_NAME, AGENT_DID).await;

    // seq 1: user, seq 2: assistant, seq 3: user, seq 4: assistant, seq 5: user, seq 6: assistant
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

    // Fork before the 2nd user message (user-turn index 1).
    let outcome = fork(
        &db.node,
        ForkParams {
            source_session_id: parent_session,
            fork_at_user_turn: 1,
            caller_agent_did: AGENT_DID,
            target_behavior_id: None,
        },
    )
    .await
    .expect("fork succeeds");

    // Prefix match: child has seq 1 and seq 2 (everything before seq 3, the 2nd user message).
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
        format!("{}:1", outcome.session_id)
    );
    assert_eq!(child_messages[1].sequence, 2);
    assert_eq!(child_messages[1].role, "assistant");
    assert_eq!(child_messages[1].content, "a1");

    // Parent unchanged.
    let parent_messages = fetch_message_snapshots_for_session(&db.node, parent_session).await;
    assert_eq!(parent_messages.len(), 6);

    // Outcome counters.
    assert_eq!(outcome.copied_messages, 2);
}

#[tokio::test]
async fn fork_via_http_copies_message_prefix_up_to_user_turn_boundary() {
    let db = test_db("fork-http-happy-path-messages").await;

    let parent_session = "parent-http-session";
    create_agent_session(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
    create_agent_conversation(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
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

    let graphql = spawn_embedded_graphql(db.node.clone()).await;
    let outcome = fork_via_http(
        &graphql,
        ForkParams {
            source_session_id: parent_session,
            fork_at_user_turn: 1,
            caller_agent_did: AGENT_DID,
            target_behavior_id: None,
        },
    )
    .await
    .expect("fork via http succeeds");

    let child_messages = fetch_message_snapshots_for_session(&db.node, &outcome.session_id).await;
    assert_eq!(child_messages.len(), 2);
    assert_eq!(child_messages[0].sequence, 1);
    assert_eq!(child_messages[0].role, "user");
    assert_eq!(child_messages[0].content, "u1");
    assert_eq!(child_messages[0].session_id, outcome.session_id);
    assert_eq!(child_messages[1].sequence, 2);
    assert_eq!(child_messages[1].role, "assistant");
    assert_eq!(child_messages[1].content, "a1");

    let child_conv = fetch_conversation_snapshot(&db.node, &outcome.session_id)
        .await
        .expect("child conversation exists");
    assert_eq!(
        child_conv.forked_from_session_id.as_deref(),
        Some(parent_session)
    );
    assert_eq!(child_conv.fork_at_user_turn, Some(1));
    assert!(child_conv.forked_at.is_some(), "forked_at must be set");

    assert_eq!(outcome.copied_messages, 2);
}

#[tokio::test]
async fn fork_copies_tool_calls_up_to_user_turn_boundary() {
    let db = test_db("fork-copy-tool-calls").await;

    let parent_session = "parent-tc";
    create_agent_session(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
    create_agent_conversation(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
    create_agent_behavior(&db.node, AGENT_NAME, AGENT_DID).await;

    // Turn 1: u @ seq 1 → a @ seq 2 → tool_call @ seq 3 → u @ seq 4 → a @ seq 5
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
async fn fork_copies_tool_results_strictly_before_cut_ts() {
    let db = test_db("fork-copy-tool-results").await;

    let parent_session = "parent-tr";
    create_agent_session(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
    create_agent_conversation(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
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
    // Two spills: one before u2 (created_at=10:00:02Z, should be copied), one after (10:00:04Z, should NOT).
    create_agent_tool_result(
        &db.node,
        parent_session,
        "read_file",
        "{}",
        "early",
        "2026-04-21T10:00:02Z",
    )
    .await;
    create_agent_tool_result(
        &db.node,
        parent_session,
        "read_file",
        "{}",
        "late",
        "2026-04-21T10:00:04Z",
    )
    .await;

    // Fork before user-turn 1 (which is u2 at seq 2, ts=10:00:03Z). Cut_ts = 10:00:03Z.
    let outcome = fork(
        &db.node,
        ForkParams {
            source_session_id: parent_session,
            fork_at_user_turn: 1,
            caller_agent_did: AGENT_DID,
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
        "only the early tool result should be copied"
    );
    assert_eq!(child_results[0].output_text, "early");
    assert_eq!(child_results[0].session_id, outcome.session_id);
    // Every persisted column is faithfully copied (not just output_text).
    // The helper seeds truncated=false, truncation_metadata="", conversation_doc_id="" —
    // assert those survive the copy so future refactors can't accidentally zero-out
    // or mis-serialize any spill field.
    assert_eq!(child_results[0].agent_did, AGENT_DID);
    assert_eq!(child_results[0].tool_name, "read_file");
    assert_eq!(child_results[0].tool_input, "{}");
    assert!(!child_results[0].truncated);
    assert_eq!(child_results[0].truncation_metadata, "");
    assert_eq!(child_results[0].conversation_doc_id, "");
    assert_eq!(child_results[0].created_at, "2026-04-21T10:00:02Z");
    assert_eq!(outcome.copied_tool_results, 1);
}

#[tokio::test]
async fn fork_copies_compaction_entries_strictly_before_cut_ts() {
    let db = test_db("fork-copy-compactions").await;

    let parent_session = "parent-ce";
    create_agent_session(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
    create_agent_conversation(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
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
        2,
        "2026-04-21T10:00:02Z",
    )
    .await;
    create_compaction_entry(
        &db.node,
        parent_session,
        2,
        "late summary",
        3,
        "2026-04-21T10:00:04Z",
    )
    .await;

    // Fork before user-turn 1. Cut_ts = 10:00:03Z.
    let outcome = fork(
        &db.node,
        ForkParams {
            source_session_id: parent_session,
            fork_at_user_turn: 1,
            caller_agent_did: AGENT_DID,
            target_behavior_id: None,
        },
    )
    .await
    .expect("fork succeeds");

    let child_compactions =
        fetch_compaction_entry_snapshots_for_session(&db.node, &outcome.session_id).await;
    assert_eq!(child_compactions.len(), 1);
    assert_eq!(child_compactions[0].summary, "early summary");
    assert_eq!(child_compactions[0].sequence, 1); // preserved from parent
    assert_eq!(
        child_compactions[0].compaction_key,
        format!("{}:1", outcome.session_id)
    );
    assert_eq!(outcome.copied_compaction_entries, 1);
}

#[tokio::test]
async fn fork_batches_multiple_rows_for_all_copy_collections() {
    let db = test_db("fork-batch-copy-all").await;

    let parent_session = "parent-batch-copy";
    create_agent_session(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
    create_agent_conversation(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
    create_agent_behavior(&db.node, AGENT_NAME, AGENT_DID).await;

    for (sequence, role, content, timestamp) in [
        (1, "user", "u1", "2026-04-21T10:00:00Z"),
        (2, "assistant", "a1", "2026-04-21T10:00:01Z"),
        (3, "tool", "tool output", "2026-04-21T10:00:02Z"),
        (4, "user", "u2", "2026-04-21T10:00:04Z"),
    ] {
        create_agent_message(&db.node, parent_session, sequence, role, content, timestamp).await;
    }

    for i in 1..=3 {
        let tool_call_id = format!("tc-{i}");
        let args = format!(r#"{{"index":{i}}}"#);
        let result = format!("tool-call-result-{i}");
        let timestamp = format!("2026-04-21T10:00:0{i}Z");
        create_agent_tool_call(
            &db.node,
            parent_session,
            i,
            &tool_call_id,
            "read_file",
            &args,
            &result,
            "completed",
            &timestamp,
            &timestamp,
        )
        .await;

        let tool_input = format!(r#"{{"path":"file-{i}.txt"}}"#);
        let output_text = format!("tool-result-{i}");
        create_agent_tool_result(
            &db.node,
            parent_session,
            "read_file",
            &tool_input,
            &output_text,
            &timestamp,
        )
        .await;

        create_compaction_entry(
            &db.node,
            parent_session,
            i,
            &format!("summary-{i}"),
            i,
            &timestamp,
        )
        .await;
    }

    let outcome = fork(
        &db.node,
        ForkParams {
            source_session_id: parent_session,
            fork_at_user_turn: 1,
            caller_agent_did: AGENT_DID,
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
            format!("{}:{sequence}", outcome.session_id)
        );
    }
}

#[tokio::test]
async fn fork_rejects_source_with_non_terminal_request() {
    let db = test_db("fork-busy-source").await;

    let parent_session = "parent-busy";
    create_agent_session(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
    create_agent_conversation(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
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

    // Create a non-terminal AgentRequest (status=pending, lifecycle_state=pending).
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
    create_agent_conversation(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
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
            caller_agent_did: "did:defra-agent:someone-else",
            target_behavior_id: None,
        },
    )
    .await
    .expect_err("fork must reject mismatched principal");

    assert!(
        matches!(err, ForkError::ForkNotSameAgent),
        "expected ForkNotSameAgent, got {:?}",
        err
    );
}

#[tokio::test]
async fn fork_accepts_behavior_swap_within_same_principal() {
    let db = test_db("fork-behavior-swap-ok").await;

    let parent_session = "parent-swap-ok";
    create_agent_session(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
    create_agent_conversation(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
    create_agent_behavior(&db.node, AGENT_NAME, AGENT_DID).await;
    // A second behavior owned by the same principal.
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
            target_behavior_id: Some("alt-behavior"),
        },
    )
    .await
    .expect("fork with matching-principal behavior succeeds");

    // Confirm the child's AgentConversation records the swapped behavior_id.
    let child_conv = support::snapshots::fetch_conversation_snapshot(&db.node, &outcome.session_id)
        .await
        .expect("child conversation exists");
    assert_eq!(child_conv.behavior_id, "alt-behavior");
}

#[tokio::test]
async fn fork_rejects_behavior_owned_by_different_principal() {
    let db = test_db("fork-behavior-swap-bad").await;

    let parent_session = "parent-swap-bad";
    create_agent_session(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
    create_agent_conversation(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
    create_agent_behavior(&db.node, AGENT_NAME, AGENT_DID).await;
    create_agent_behavior(&db.node, "foreign-behavior", "did:defra-agent:someone-else").await;
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
            target_behavior_id: Some("foreign-behavior"),
        },
    )
    .await
    .expect_err("fork must reject cross-principal behavior swap");

    assert!(
        matches!(err, ForkError::ForkBehaviorNotOwnedByPrincipal(_, _)),
        "expected ForkBehaviorNotOwnedByPrincipal, got {:?}",
        err
    );
}

#[tokio::test]
async fn fork_rejects_out_of_range_user_turn() {
    let db = test_db("fork-oor").await;

    let parent_session = "parent-oor";
    create_agent_session(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
    create_agent_conversation(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
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

    // Only 1 user message exists (index 0). Requesting index 5 is out of range.
    let err = fork(
        &db.node,
        ForkParams {
            source_session_id: parent_session,
            fork_at_user_turn: 5,
            caller_agent_did: AGENT_DID,
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

    // Also assert no orphan rows were created in ANY fork-touched collection:
    // out-of-range must short-circuit before any write.
    for collection in [
        "AgentMessage",
        "AgentSession",
        "AgentConversation",
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
    create_agent_conversation(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
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

    let child_conv = support::snapshots::fetch_conversation_snapshot(&db.node, &outcome.session_id)
        .await
        .expect("child conversation exists");
    assert_eq!(
        child_conv.forked_from_session_id.as_deref(),
        Some(parent_session)
    );
    assert_eq!(child_conv.fork_at_user_turn, Some(0));
    assert!(child_conv.forked_at.is_some(), "forked_at must be set");
}

#[tokio::test]
async fn fork_at_total_user_turns_copies_full_history() {
    let db = test_db("fork-end-of-history").await;

    let parent_session = "parent-end";
    create_agent_session(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
    create_agent_conversation(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
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

    let child_conv = support::snapshots::fetch_conversation_snapshot(&db.node, &outcome.session_id)
        .await
        .expect("child conversation exists");
    assert_eq!(
        child_conv.forked_from_session_id.as_deref(),
        Some(parent_session)
    );
    assert_eq!(child_conv.fork_at_user_turn, Some(2));
}

#[tokio::test]
async fn fork_leaves_parent_byte_identical() {
    let db = test_db("fork-parent-unchanged").await;

    let parent_session = "parent-unchanged";
    create_agent_session(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
    create_agent_conversation(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
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
    // Use the full-column snapshot here: fork's read-only-parent claim must hold
    // across every AgentConversation field, not just the handful ConversationSnapshot
    // exposes. Catches silent mutations on title, preview_text, agent_did, agent_name,
    // created_at, updated_at if a future refactor adds parent-side writes.
    let before_conv =
        support::snapshots::fetch_full_conversation_snapshot(&db.node, parent_session).await;

    let _ = fork(
        &db.node,
        ForkParams {
            source_session_id: parent_session,
            fork_at_user_turn: 1,
            caller_agent_did: AGENT_DID,
            target_behavior_id: None,
        },
    )
    .await
    .expect("fork succeeds");

    let after_messages = fetch_message_snapshots_for_session(&db.node, parent_session).await;
    let after_conv =
        support::snapshots::fetch_full_conversation_snapshot(&db.node, parent_session).await;

    assert_eq!(
        before_messages, after_messages,
        "parent AgentMessage rows unchanged"
    );
    assert_eq!(
        before_conv, after_conv,
        "parent AgentConversation unchanged"
    );
}

#[tokio::test]
async fn concurrent_forks_of_same_parent_produce_disjoint_children() {
    let db = test_db("fork-concurrent").await;

    let parent_session = "parent-concurrent";
    create_agent_session(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
    create_agent_conversation(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
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
    assert_eq!(outcome_a.copied_messages, 0); // cut before the 1st user message
    assert_eq!(outcome_b.copied_messages, 2); // u1 + a1
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
    create_agent_conversation(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
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

/// Fork-of-fork: the spec requires that ancestry can be walked one link at a
/// time via `forked_from_session_id`, and that each child is independent of
/// deeper ancestors. This test covers the two-generation case:
///   grandparent -> (fork @ turn 1) -> child -> (fork @ turn 1) -> grandchild
/// Grandchild must:
///   - exist as a regular session (messages, conversation present)
///   - record `forked_from_session_id == child`, NOT `grandparent`
///   - copy exactly the prefix of child (which itself already contains only
///     grandparent's prefix plus child's post-fork messages)
#[tokio::test]
async fn fork_of_fork_links_to_immediate_parent_not_grandparent() {
    let db = test_db("fork-of-fork").await;

    // --- Generation 0: grandparent ---
    let grandparent_session = "grandparent";
    create_agent_session(
        &db.node,
        grandparent_session,
        AGENT_NAME,
        "2026-04-21T10:00:00Z",
    )
    .await;
    create_agent_conversation(
        &db.node,
        grandparent_session,
        AGENT_NAME,
        "2026-04-21T10:00:00Z",
    )
    .await;
    create_agent_behavior(&db.node, AGENT_NAME, AGENT_DID).await;

    // seq 1: user, 2: assistant, 3: user, 4: assistant
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

    // --- Generation 1: child = fork(grandparent, user_turn=1) ---
    // Cut is before grandparent's 2nd user message (seq 3), so child inherits
    // seq 1 (gp_u1) and seq 2 (gp_a1).
    let child_outcome = fork(
        &db.node,
        ForkParams {
            source_session_id: grandparent_session,
            fork_at_user_turn: 1,
            caller_agent_did: AGENT_DID,
            target_behavior_id: None,
        },
    )
    .await
    .expect("child fork succeeds");
    assert_eq!(child_outcome.copied_messages, 2);

    // Child extends history with its own post-fork turn.
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

    // --- Generation 2: grandchild = fork(child, user_turn=1) ---
    // Child's user messages are: gp_u1 at seq 1, child_u2 at seq 3. So
    // user_turn=1 cuts before child_u2 (seq 3) and grandchild copies
    // seq 1 (gp_u1) + seq 2 (gp_a1). This matches what the grandparent fork
    // produced — fork-of-fork is consistent.
    let grandchild_outcome = fork(
        &db.node,
        ForkParams {
            source_session_id: &child_outcome.session_id,
            fork_at_user_turn: 1,
            caller_agent_did: AGENT_DID,
            target_behavior_id: None,
        },
    )
    .await
    .expect("grandchild fork succeeds");
    assert_eq!(
        grandchild_outcome.copied_messages, 2,
        "grandchild inherits child's prefix (which is grandparent's prefix)"
    );

    // Provenance: grandchild's forked_from points to CHILD, not grandparent.
    let grandchild_conv =
        support::snapshots::fetch_conversation_snapshot(&db.node, &grandchild_outcome.session_id)
            .await
            .expect("grandchild conversation exists");
    assert_eq!(
        grandchild_conv.forked_from_session_id.as_deref(),
        Some(child_outcome.session_id.as_str()),
        "grandchild must record its immediate parent (child), not its grandparent"
    );
    assert_eq!(grandchild_conv.fork_at_user_turn, Some(1));

    // Verify the copied messages match the child's prefix (content carries
    // grandparent's labels because they were copied verbatim at generation 1).
    let grandchild_messages =
        fetch_message_snapshots_for_session(&db.node, &grandchild_outcome.session_id).await;
    assert_eq!(grandchild_messages.len(), 2);
    assert_eq!(grandchild_messages[0].content, "gp_u1");
    assert_eq!(grandchild_messages[1].content, "gp_a1");
    // And grandchild's message_keys are remapped to its own session_id.
    assert_eq!(
        grandchild_messages[0].session_id,
        grandchild_outcome.session_id
    );
    assert_eq!(
        grandchild_messages[0].message_key,
        format!("{}:1", grandchild_outcome.session_id)
    );

    // Child's rows must not appear in grandchild's query, and vice-versa.
    assert!(!grandchild_messages
        .iter()
        .any(|m| m.session_id == child_outcome.session_id));
}
