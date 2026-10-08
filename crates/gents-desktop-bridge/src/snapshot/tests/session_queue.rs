use super::*;
use gents_protocol::request_input::{QueuePolicy, QueueSource, RequestInput, RequestQueue};
use gents_protocol::request_lifecycle::RequestLifecycleState;

fn session_observing(newest: &str, state: RequestLifecycleState) -> AgentSession {
    AgentSession {
        session_id: "sess-1".into(),
        agent_did: "did:test:amy".into(),
        requester_did: None,
        behavior_id: "default".into(),
        created_at: "2026-04-21T12:00:00Z".into(),
        closed_at: None,
        title: None,
        tags: Vec::new(),
        provenance: None,
        observation: Some(SessionObservation {
            last_activity_at: "2026-04-21T12:00:09Z".into(),
            preview: None,
            latest_request: Some(SessionRequestObservation {
                request_doc_id: format!("doc-{newest}"),
                request_id: newest.into(),
                lifecycle_state: state,
            }),
        }),
    }
}

fn request(id: &str, state: RequestLifecycleState, second: u32) -> AgentRequestRow {
    AgentRequestRow {
        purpose: Some(gents_protocol::request_admission::RequestPurpose::Normal),
        doc_id: Some(format!("doc-{id}")),
        request_id: id.into(),
        agent_did: Some("did:test:amy".into()),
        behavior_id: Some("default".into()),
        session_id: Some("sess-1".into()),
        content: Some(format!("{id} text")),
        lifecycle_state: Some(state),
        execution_origin: Some("interactive".into()),
        created_at: Some(format!("2026-04-21T12:00:{second:02}Z")),
        ..Default::default()
    }
}

fn queued(id: &str, after: &str, state: RequestLifecycleState, second: u32) -> AgentRequestRow {
    AgentRequestRow {
        input: Some(RequestInput {
            queue: Some(RequestQueue {
                source: QueueSource::User,
                policy: QueuePolicy::Append,
                key: None,
                queued_after_request_id: Some(after.into()),
                interrupted_request_id: None,
                background_completion_wake_version: None,
            }),
            ..Default::default()
        }),
        ..request(id, state, second)
    }
}

fn folded_into(id: &str, after: &str, head: &str, second: u32) -> AgentRequestRow {
    AgentRequestRow {
        superseded_by_request: Some(head.into()),
        superseded_by_request_doc_id: Some(format!("doc-{head}")),
        failure_reason: Some(gents::lifecycle::FOLDED_REASON.into()),
        ..queued(id, after, RequestLifecycleState::Superseded, second)
    }
}

fn user_contents(snapshot: &crate::types::DesktopSessionSnapshot) -> Vec<String> {
    snapshot
        .timeline_items
        .iter()
        .filter_map(|item| match item {
            RenderedTimelineItem::UserMessage { content, .. } => content.clone(),
            RenderedTimelineItem::PendingUserTurn { content, .. } => {
                Some(format!("pending:{content}"))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn messages_queued_behind_a_running_turn_wait_outside_the_transcript_in_queue_order() {
    let mut rows = ClientStoreRows {
        sessions: vec![session_observing("third", RequestLifecycleState::Pending)],
        requests: vec![
            request("running", RequestLifecycleState::Processing, 0),
            queued("first", "running", RequestLifecycleState::Pending, 1),
            queued("second", "first", RequestLifecycleState::Pending, 2),
            queued("third", "second", RequestLifecycleState::Pending, 3),
        ],
        ..ClientStoreRows::default()
    };
    push_canonical_text_message(
        &mut rows,
        "authored:doc-running:prompt",
        "sess-1",
        Some("doc-running"),
        1,
        MessageRole::User,
        "running text",
    );
    let snapshot =
        build_session_snapshot_from_store(&ClientStore::from_rows(rows), "sess-1", Some("third"))
            .expect("snapshot");

    assert_eq!(snapshot.latest_request_id.as_deref(), Some("running"));
    assert_eq!(snapshot.turn_state.as_deref(), Some("running"));
    assert_eq!(user_contents(&snapshot), ["running text"]);
    assert_eq!(
        snapshot
            .queued_turns
            .iter()
            .map(|turn| (turn.request_id.as_str(), turn.content.as_str()))
            .collect::<Vec<_>>(),
        [
            ("first", "first text"),
            ("second", "second text"),
            ("third", "third text")
        ]
    );
}

#[test]
fn folded_messages_render_once_as_the_claimed_turns_own_user_entries() {
    let mut rows = ClientStoreRows {
        sessions: vec![session_observing(
            "later",
            RequestLifecycleState::Superseded,
        )],
        requests: vec![
            request("done", RequestLifecycleState::Completed, 0),
            queued("head", "done", RequestLifecycleState::Processing, 1),
            folded_into("sooner", "head", "head", 2),
            folded_into("later", "sooner", "head", 3),
        ],
        ..ClientStoreRows::default()
    };
    for (key, request_doc, sequence, text) in [
        ("authored:doc-done:prompt", "doc-done", 1, "done text"),
        ("authored:doc-head:prompt", "doc-head", 3, "head text"),
        (
            "authored:doc-head:folded:doc-sooner",
            "doc-head",
            4,
            "sooner text",
        ),
        (
            "authored:doc-head:folded:doc-later",
            "doc-head",
            5,
            "later text",
        ),
    ] {
        push_canonical_text_message(
            &mut rows,
            key,
            "sess-1",
            Some(request_doc),
            sequence,
            MessageRole::User,
            text,
        );
    }
    push_canonical_text_message(
        &mut rows,
        "answer-done",
        "sess-1",
        Some("doc-done"),
        2,
        MessageRole::Assistant,
        "first answer",
    );
    let store = ClientStore::from_rows(rows);
    for preferred in [None, Some("later"), Some("head")] {
        let snapshot =
            build_session_snapshot_from_store(&store, "sess-1", preferred).expect("snapshot");
        assert_eq!(snapshot.latest_request_id.as_deref(), Some("head"));
        assert_eq!(snapshot.turn_state.as_deref(), Some("running"));
        assert_eq!(
            user_contents(&snapshot),
            ["done text", "head text", "sooner text", "later text"],
            "{preferred:?}"
        );
        assert!(snapshot.queued_turns.is_empty());
    }
}

#[test]
fn a_queued_message_the_runtime_ends_unclaimed_keeps_its_terminal_state() {
    let mut rows = ClientStoreRows {
        sessions: vec![session_observing(
            "dropped",
            RequestLifecycleState::Interrupted,
        )],
        requests: vec![
            request("done", RequestLifecycleState::Completed, 0),
            queued("dropped", "done", RequestLifecycleState::Interrupted, 1),
        ],
        ..ClientStoreRows::default()
    };
    push_canonical_text_message(
        &mut rows,
        "authored:doc-done:prompt",
        "sess-1",
        Some("doc-done"),
        1,
        MessageRole::User,
        "done text",
    );
    let snapshot = build_session_snapshot_from_store(&ClientStore::from_rows(rows), "sess-1", None)
        .expect("snapshot");
    assert!(snapshot.queued_turns.is_empty());
    assert!(matches!(
        snapshot.timeline_items.last(),
        Some(RenderedTimelineItem::PendingUserTurn { request_id, lifecycle_state, .. })
            if request_id == "dropped" && lifecycle_state.as_deref() == Some("interrupted")
    ));
}

#[test]
fn a_folded_message_stays_pending_under_its_turn_until_that_turn_publishes_it() {
    let mut rows = ClientStoreRows {
        sessions: vec![session_observing(
            "later",
            RequestLifecycleState::Superseded,
        )],
        requests: vec![
            request("head", RequestLifecycleState::Claimed, 1),
            folded_into("later", "head", "head", 2),
        ],
        ..ClientStoreRows::default()
    };
    let store = ClientStore::from_rows(rows.clone());
    let before = build_session_snapshot_from_store(&store, "sess-1", None).expect("snapshot");
    assert_eq!(before.latest_request_id.as_deref(), Some("head"));
    assert_eq!(
        user_contents(&before),
        ["pending:head text", "pending:later text"]
    );
    assert!(before.timeline_items.iter().any(|item| matches!(
        item,
        RenderedTimelineItem::PendingUserTurn { request_id, folded_into_request_id, .. }
            if request_id == "later" && folded_into_request_id.as_deref() == Some("head")
    )));

    for (key, sequence, text) in [
        ("authored:doc-head:prompt", 1, "head text"),
        ("authored:doc-head:folded:doc-later", 2, "later text"),
    ] {
        push_canonical_text_message(
            &mut rows,
            key,
            "sess-1",
            Some("doc-head"),
            sequence,
            MessageRole::User,
            text,
        );
    }
    let after = build_session_snapshot_from_store(&ClientStore::from_rows(rows), "sess-1", None)
        .expect("snapshot");
    assert_eq!(user_contents(&after), ["head text", "later text"]);
}

#[test]
fn the_session_list_reports_the_turn_a_folded_or_queued_head_belongs_to() {
    let requests = vec![
        request("done", RequestLifecycleState::Completed, 0),
        queued("head", "done", RequestLifecycleState::Processing, 1),
        folded_into("later", "head", "head", 2),
    ];
    let summaries = session_summaries(
        &[session_observing("later", RequestLifecycleState::Superseded)],
        &requests,
        "did:test:amy",
        &[],
        &[],
    );
    assert_eq!(summaries[0].latest_request_id.as_deref(), Some("head"));
    assert_eq!(summaries[0].turn_state.as_deref(), Some("running"));
}
