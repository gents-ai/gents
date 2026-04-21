use defra_agent::session::{fork, ForkError, ForkParams};

mod support;

use support::snapshots::fetch_message_snapshots_for_session;
use support::snapshots::fetch_tool_call_snapshots_for_session;
use support::snapshots::fetch_tool_result_snapshots_for_session;
use support::snapshots::fetch_compaction_entry_snapshots_for_session;
use support::{
    create_agent_behavior, create_agent_conversation, create_agent_message,
    create_agent_session, create_agent_tool_call, create_agent_tool_result,
    create_compaction_entry, create_request, test_db, AGENT_DID, AGENT_NAME,
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

#[tokio::test]
async fn fork_rejects_source_with_non_terminal_request() {
    let db = test_db("fork-busy-source").await;

    let parent_session = "parent-busy";
    create_agent_session(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
    create_agent_conversation(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
    create_agent_behavior(&db.node, AGENT_NAME, AGENT_DID).await;
    create_agent_message(&db.node, parent_session, 1, "user", "u1", "2026-04-21T10:00:01Z").await;

    // Create a non-terminal AgentRequest (status=pending, lifecycle_state=pending).
    create_request(&db.node, "req-pending", parent_session, "pending", "2026-04-21T10:00:02Z").await;

    let err = fork(&db.node, ForkParams {
        source_session_id: parent_session,
        fork_at_user_turn: 0,
        caller_agent_did: AGENT_DID,
        target_behavior_id: None,
    }).await.expect_err("fork must reject busy source");

    assert!(matches!(err, ForkError::ForkSourceBusy), "expected ForkSourceBusy, got {:?}", err);
}

#[tokio::test]
async fn fork_rejects_mismatched_caller_principal() {
    let db = test_db("fork-wrong-principal").await;

    let parent_session = "parent-wp";
    create_agent_session(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
    create_agent_conversation(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
    create_agent_behavior(&db.node, AGENT_NAME, AGENT_DID).await;
    create_agent_message(&db.node, parent_session, 1, "user", "u1", "2026-04-21T10:00:01Z").await;

    let err = fork(&db.node, ForkParams {
        source_session_id: parent_session,
        fork_at_user_turn: 0,
        caller_agent_did: "did:defra-agent:someone-else",
        target_behavior_id: None,
    }).await.expect_err("fork must reject mismatched principal");

    assert!(matches!(err, ForkError::ForkNotSameAgent), "expected ForkNotSameAgent, got {:?}", err);
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
    create_agent_message(&db.node, parent_session, 1, "user", "u1", "2026-04-21T10:00:01Z").await;

    let outcome = fork(&db.node, ForkParams {
        source_session_id: parent_session,
        fork_at_user_turn: 0,
        caller_agent_did: AGENT_DID,
        target_behavior_id: Some("alt-behavior"),
    }).await.expect("fork with matching-principal behavior succeeds");

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
    create_agent_message(&db.node, parent_session, 1, "user", "u1", "2026-04-21T10:00:01Z").await;

    let err = fork(&db.node, ForkParams {
        source_session_id: parent_session,
        fork_at_user_turn: 0,
        caller_agent_did: AGENT_DID,
        target_behavior_id: Some("foreign-behavior"),
    }).await.expect_err("fork must reject cross-principal behavior swap");

    assert!(
        matches!(err, ForkError::ForkBehaviorNotOwnedByPrincipal(_, _)),
        "expected ForkBehaviorNotOwnedByPrincipal, got {:?}", err
    );
}

#[tokio::test]
async fn fork_rejects_out_of_range_user_turn() {
    let db = test_db("fork-oor").await;

    let parent_session = "parent-oor";
    create_agent_session(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
    create_agent_conversation(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
    create_agent_behavior(&db.node, AGENT_NAME, AGENT_DID).await;
    create_agent_message(&db.node, parent_session, 1, "user", "u1", "2026-04-21T10:00:01Z").await;
    create_agent_message(&db.node, parent_session, 2, "assistant", "a1", "2026-04-21T10:00:02Z").await;

    // Only 1 user message exists (index 0). Requesting index 5 is out of range.
    let err = fork(&db.node, ForkParams {
        source_session_id: parent_session,
        fork_at_user_turn: 5,
        caller_agent_did: AGENT_DID,
        target_behavior_id: None,
    }).await.expect_err("fork must reject out-of-range user turn");

    assert!(
        matches!(err, ForkError::ForkAtUserTurnOutOfRange(5, 1)),
        "expected ForkAtUserTurnOutOfRange(5, 1), got {:?}", err
    );

    // Also assert no orphan rows were created: no AgentMessage rows exist
    // outside the parent session.
    let query = format!(
        r#"{{
            AgentMessage(filter: {{ session_id: {{ _neq: "{parent_session}" }} }}) {{ session_id }}
        }}"#
    );
    let resp = db.node.execute(&query).await;
    let rows = resp.data.as_ref()
        .and_then(|d| d.get("AgentMessage"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(rows.is_empty(), "out-of-range fork must not create orphans: got {:?}", rows);
}

#[tokio::test]
async fn fork_at_user_turn_zero_produces_empty_child_with_provenance() {
    let db = test_db("fork-user-turn-zero").await;

    let parent_session = "parent-zero";
    create_agent_session(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
    create_agent_conversation(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
    create_agent_behavior(&db.node, AGENT_NAME, AGENT_DID).await;
    create_agent_message(&db.node, parent_session, 1, "user", "u1", "2026-04-21T10:00:01Z").await;
    create_agent_message(&db.node, parent_session, 2, "assistant", "a1", "2026-04-21T10:00:02Z").await;

    let outcome = fork(&db.node, ForkParams {
        source_session_id: parent_session,
        fork_at_user_turn: 0,
        caller_agent_did: AGENT_DID,
        target_behavior_id: None,
    }).await.expect("fork at user-turn 0 succeeds");

    assert_eq!(outcome.copied_messages, 0);
    assert_eq!(outcome.copied_tool_calls, 0);
    assert_eq!(outcome.copied_tool_results, 0);
    assert_eq!(outcome.copied_compaction_entries, 0);

    let child_messages = fetch_message_snapshots_for_session(&db.node, &outcome.session_id).await;
    assert!(child_messages.is_empty());

    let child_conv = support::snapshots::fetch_conversation_snapshot(&db.node, &outcome.session_id)
        .await
        .expect("child conversation exists");
    assert_eq!(child_conv.forked_from_session_id.as_deref(), Some(parent_session));
    assert_eq!(child_conv.fork_at_user_turn, Some(0));
    assert!(child_conv.forked_at.is_some(), "forked_at must be set");
}

#[tokio::test]
async fn fork_leaves_parent_byte_identical() {
    let db = test_db("fork-parent-unchanged").await;

    let parent_session = "parent-unchanged";
    create_agent_session(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
    create_agent_conversation(&db.node, parent_session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
    create_agent_behavior(&db.node, AGENT_NAME, AGENT_DID).await;

    for (i, role) in [
        (1u32, "user"), (2, "assistant"), (3, "tool"),
        (4, "user"), (5, "assistant"),
    ] {
        let ts = format!("2026-04-21T10:00:0{i}Z");
        create_agent_message(&db.node, parent_session, i, role, &format!("msg{i}"), &ts).await;
    }

    let before_messages = fetch_message_snapshots_for_session(&db.node, parent_session).await;
    let before_conv = support::snapshots::fetch_conversation_snapshot(&db.node, parent_session).await;

    let _ = fork(&db.node, ForkParams {
        source_session_id: parent_session,
        fork_at_user_turn: 1,
        caller_agent_did: AGENT_DID,
        target_behavior_id: None,
    }).await.expect("fork succeeds");

    let after_messages = fetch_message_snapshots_for_session(&db.node, parent_session).await;
    let after_conv = support::snapshots::fetch_conversation_snapshot(&db.node, parent_session).await;

    assert_eq!(before_messages, after_messages, "parent AgentMessage rows unchanged");
    assert_eq!(before_conv, after_conv, "parent AgentConversation unchanged");
}
