use defra_agent::session::{fork, ForkParams};

mod support;

use support::snapshots::fetch_message_snapshots_for_session;
use support::{
    create_agent_behavior, create_agent_conversation, create_agent_message,
    create_agent_session, test_db, AGENT_DID, AGENT_NAME,
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
