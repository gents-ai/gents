use defra_agent::session::{fork, ForkParams};

mod support;

use support::snapshots::fetch_message_snapshots_for_session;
use support::snapshots::fetch_tool_call_snapshots_for_session;
use support::snapshots::fetch_tool_result_snapshots_for_session;
use support::snapshots::fetch_compaction_entry_snapshots_for_session;
use support::{
    create_agent_behavior, create_agent_conversation, create_agent_message,
    create_agent_session, create_agent_tool_call, create_agent_tool_result,
    create_compaction_entry, test_db, AGENT_DID, AGENT_NAME,
};

#[tokio::test]
async fn fork_copies_message_prefix_up_to_user_turn_boundary() {
    let db = test_db("fork-happy-path-messages").await;

    // Parent session with three user turns interleaved with assistant replies.
    let parent_session = "parent-session";
    create_agent_session(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
    create_agent_conversation(
        &db.node,
        parent_session,
        AGENT_NAME,
        "2026-04-21T10:00:00Z",
    )
    .await;
    create_agent_behavior(&db.node, AGENT_NAME, AGENT_DID).await;

    // seq 1: user, seq 2: assistant, seq 3: user, seq 4: assistant, seq 5: user, seq 6: assistant
    create_agent_message(&db.node, parent_session, 1, "user", "u1", "2026-04-21T10:00:01Z").await;
    create_agent_message(&db.node, parent_session, 2, "assistant", "a1", "2026-04-21T10:00:02Z").await;
    create_agent_message(&db.node, parent_session, 3, "user", "u2", "2026-04-21T10:00:03Z").await;
    create_agent_message(&db.node, parent_session, 4, "assistant", "a2", "2026-04-21T10:00:04Z").await;
    create_agent_message(&db.node, parent_session, 5, "user", "u3", "2026-04-21T10:00:05Z").await;
    create_agent_message(&db.node, parent_session, 6, "assistant", "a3", "2026-04-21T10:00:06Z").await;

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
    assert_eq!(child_messages.len(), 2, "child should have 2 messages (u1, a1)");
    assert_eq!(child_messages[0].sequence, 1);
    assert_eq!(child_messages[0].role, "user");
    assert_eq!(child_messages[0].content, "u1");
    assert_eq!(child_messages[0].timestamp, "2026-04-21T10:00:01Z");
    assert_eq!(child_messages[0].session_id, outcome.session_id);
    assert_eq!(child_messages[0].message_key, format!("{}:1", outcome.session_id));
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
async fn fork_copies_tool_calls_up_to_user_turn_boundary() {
    let db = test_db("fork-copy-tool-calls").await;

    let parent_session = "parent-tc";
    create_agent_session(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
    create_agent_conversation(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
    create_agent_behavior(&db.node, AGENT_NAME, AGENT_DID).await;

    // Turn 1: u @ seq 1 → a @ seq 2 → tool_call @ seq 3 → u @ seq 4 → a @ seq 5
    create_agent_message(&db.node, parent_session, 1, "user", "u1", "2026-04-21T10:00:01Z").await;
    create_agent_message(&db.node, parent_session, 2, "assistant", "a1", "2026-04-21T10:00:02Z").await;
    create_agent_tool_call(
        &db.node, parent_session, 2, "tc-1", "read_file",
        r#"{"path":"foo"}"#, "file contents", "completed",
        "2026-04-21T10:00:02Z", "2026-04-21T10:00:02Z",
    ).await;
    create_agent_message(&db.node, parent_session, 3, "tool", "r1", "2026-04-21T10:00:03Z").await;
    create_agent_message(&db.node, parent_session, 4, "user", "u2", "2026-04-21T10:00:04Z").await;
    create_agent_tool_call(
        &db.node, parent_session, 4, "tc-2", "write_file",
        r#"{"path":"bar"}"#, "ok", "completed",
        "2026-04-21T10:00:04Z", "2026-04-21T10:00:04Z",
    ).await;
    create_agent_message(&db.node, parent_session, 5, "assistant", "a2", "2026-04-21T10:00:05Z").await;

    let outcome = fork(&db.node, ForkParams {
        source_session_id: parent_session,
        fork_at_user_turn: 1,
        caller_agent_did: AGENT_DID,
        target_behavior_id: None,
    }).await.expect("fork succeeds");

    let child_tool_calls = fetch_tool_call_snapshots_for_session(&db.node, &outcome.session_id).await;
    assert_eq!(child_tool_calls.len(), 1, "only tc-1 (message_sequence=2) should be copied");
    assert_eq!(child_tool_calls[0].tool_call_id, "tc-1");
    assert_eq!(child_tool_calls[0].message_sequence, 2);
    assert_eq!(child_tool_calls[0].session_id, outcome.session_id);
    assert_eq!(child_tool_calls[0].tool_call_key, format!("{}:tc-1", outcome.session_id));

    assert_eq!(outcome.copied_tool_calls, 1);
}

#[tokio::test]
async fn fork_copies_tool_results_strictly_before_cut_ts() {
    let db = test_db("fork-copy-tool-results").await;

    let parent_session = "parent-tr";
    create_agent_session(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
    create_agent_conversation(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
    create_agent_behavior(&db.node, AGENT_NAME, AGENT_DID).await;

    create_agent_message(&db.node, parent_session, 1, "user", "u1", "2026-04-21T10:00:01Z").await;
    create_agent_message(&db.node, parent_session, 2, "user", "u2", "2026-04-21T10:00:03Z").await;
    // Two spills: one before u2 (created_at=10:00:02Z, should be copied), one after (10:00:04Z, should NOT).
    create_agent_tool_result(&db.node, parent_session, "read_file", "{}", "early", "2026-04-21T10:00:02Z").await;
    create_agent_tool_result(&db.node, parent_session, "read_file", "{}", "late",  "2026-04-21T10:00:04Z").await;

    // Fork before user-turn 1 (which is u2 at seq 2, ts=10:00:03Z). Cut_ts = 10:00:03Z.
    let outcome = fork(&db.node, ForkParams {
        source_session_id: parent_session,
        fork_at_user_turn: 1,
        caller_agent_did: AGENT_DID,
        target_behavior_id: None,
    }).await.expect("fork succeeds");

    let child_results = fetch_tool_result_snapshots_for_session(&db.node, &outcome.session_id).await;
    assert_eq!(child_results.len(), 1, "only the early tool result should be copied");
    assert_eq!(child_results[0].output_text, "early");
    assert_eq!(child_results[0].session_id, outcome.session_id);
    assert_eq!(outcome.copied_tool_results, 1);
}

#[tokio::test]
async fn fork_copies_compaction_entries_strictly_before_cut_ts() {
    let db = test_db("fork-copy-compactions").await;

    let parent_session = "parent-ce";
    create_agent_session(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
    create_agent_conversation(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
    create_agent_behavior(&db.node, AGENT_NAME, AGENT_DID).await;

    create_agent_message(&db.node, parent_session, 1, "user", "u1", "2026-04-21T10:00:01Z").await;
    create_agent_message(&db.node, parent_session, 2, "user", "u2", "2026-04-21T10:00:03Z").await;
    create_compaction_entry(&db.node, parent_session, 1, "early summary", 2, "2026-04-21T10:00:02Z").await;
    create_compaction_entry(&db.node, parent_session, 2, "late summary",  3, "2026-04-21T10:00:04Z").await;

    // Fork before user-turn 1. Cut_ts = 10:00:03Z.
    let outcome = fork(&db.node, ForkParams {
        source_session_id: parent_session,
        fork_at_user_turn: 1,
        caller_agent_did: AGENT_DID,
        target_behavior_id: None,
    }).await.expect("fork succeeds");

    let child_compactions = fetch_compaction_entry_snapshots_for_session(&db.node, &outcome.session_id).await;
    assert_eq!(child_compactions.len(), 1);
    assert_eq!(child_compactions[0].summary, "early summary");
    assert_eq!(child_compactions[0].sequence, 1); // preserved from parent
    assert_eq!(child_compactions[0].compaction_key, format!("{}:1", outcome.session_id));
    assert_eq!(outcome.copied_compaction_entries, 1);
}
