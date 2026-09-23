use super::*;
use crate::types::ReconstructionState;
use gents_protocol::request_lifecycle::RequestLifecycleState;

fn stale_session(state: RequestLifecycleState) -> AgentSession {
    AgentSession {
        session_id: "session-1".into(),
        agent_did: "did:test:amy".into(),
        requester_did: None,
        behavior_id: "amy-default".into(),
        created_at: "2026-04-21T12:00:00Z".into(),
        closed_at: None,
        title: None,
        tags: Vec::new(),
        provenance: None,
        observation: Some(SessionObservation {
            last_activity_at: "2026-04-21T12:02:00Z".into(),
            preview: Some("done".into()),
            latest_request: Some(SessionRequestObservation {
                request_doc_id: "req-1".into(),
                request_id: "req-1".into(),
                lifecycle_state: state,
            }),
        }),
    }
}

fn stale_request(state: RequestLifecycleState) -> AgentRequestRow {
    AgentRequestRow {
        doc_id: Some("req-1".into()),
        request_id: "req-1".into(),
        agent_did: Some("did:test:amy".into()),
        behavior_id: Some("amy-default".into()),
        session_id: Some("session-1".into()),
        content: Some("do the work".into()),
        lifecycle_state: Some(state),
        execution_origin: Some("interactive".into()),
        created_at: Some("2026-04-21T12:00:00Z".into()),
        ..Default::default()
    }
}

#[test]
fn session_observation_advances_an_exact_stale_request_to_terminal() {
    let store = ClientStore::from_rows(ClientStoreRows {
        sessions: vec![stale_session(RequestLifecycleState::Completed)],
        requests: vec![stale_request(RequestLifecycleState::Processing)],
        ..ClientStoreRows::default()
    });
    let snapshot =
        build_session_snapshot_from_store(&store, "session-1", Some("req-1")).expect("snapshot");
    assert_eq!(snapshot.turn_state.as_deref(), Some("completed"));
}

#[test]
fn terminal_request_has_no_live_overlay_and_keeps_canonical_transcript() {
    let mut rows = ClientStoreRows {
        sessions: vec![stale_session(RequestLifecycleState::Completed)],
        requests: vec![stale_request(RequestLifecycleState::Completed)],
        ..ClientStoreRows::default()
    };
    push_canonical_text_message(
        &mut rows,
        "user",
        "session-1",
        Some("req-1"),
        1,
        MessageRole::User,
        "turn one",
    );
    push_canonical_text_message(
        &mut rows,
        "assistant",
        "session-1",
        Some("req-1"),
        2,
        MessageRole::Assistant,
        "final answer",
    );
    let snapshot = build_session_snapshot_from_store(
        &ClientStore::from_rows(rows),
        "session-1",
        Some("req-1"),
    )
    .expect("snapshot");
    assert_eq!(snapshot.turn_state.as_deref(), Some("completed"));
    assert!(!snapshot
        .timeline_items
        .iter()
        .any(|item| matches!(item, RenderedTimelineItem::LiveAssistant { .. })));
    assert!(snapshot.timeline_items.iter().any(|item| matches!(item, RenderedTimelineItem::AssistantMessage { content, .. } if content.as_deref() == Some("final answer"))));
}

#[test]
fn missing_canonical_dependency_is_rendered_as_loading_not_empty_message() {
    let mut rows = ClientStoreRows {
        sessions: vec![stale_session(RequestLifecycleState::Processing)],
        requests: vec![stale_request(RequestLifecycleState::Processing)],
        ..ClientStoreRows::default()
    };
    push_canonical_text_message(
        &mut rows,
        "partial",
        "session-1",
        Some("req-1"),
        1,
        MessageRole::Assistant,
        "not available yet",
    );
    rows.output_segments.clear();
    let snapshot = build_session_snapshot_from_store(
        &ClientStore::from_rows(rows),
        "session-1",
        Some("req-1"),
    )
    .expect("snapshot");
    assert!(snapshot.timeline_items.iter().any(|item| matches!(item, RenderedTimelineItem::AssistantMessage { reconstruction, content, .. } if reconstruction.state == ReconstructionState::Loading && content.is_none())));
}

#[test]
fn session_snapshot_hides_live_overlay_once_turn_is_terminal_even_if_response_is_stale() {
    let mut rows = ClientStoreRows {
        sessions: vec![stale_session(RequestLifecycleState::Completed)],
        requests: vec![stale_request(RequestLifecycleState::Completed)],
        ..ClientStoreRows::default()
    };
    push_canonical_text_message(
        &mut rows,
        "final",
        "session-1",
        Some("req-1"),
        2,
        MessageRole::Assistant,
        "final answer",
    );
    let snapshot = build_session_snapshot_from_store(
        &ClientStore::from_rows(rows),
        "session-1",
        Some("req-1"),
    )
    .expect("snapshot");
    assert_eq!(snapshot.turn_state.as_deref(), Some("completed"));
    assert!(!snapshot
        .timeline_items
        .iter()
        .any(|item| matches!(item, RenderedTimelineItem::LiveAssistant { .. })));
}

#[test]
fn session_snapshot_hides_live_overlay_once_response_is_interrupted() {
    let store = ClientStore::from_rows(ClientStoreRows {
        sessions: vec![stale_session(RequestLifecycleState::Interrupted)],
        requests: vec![stale_request(RequestLifecycleState::Interrupted)],
        ..ClientStoreRows::default()
    });
    let snapshot =
        build_session_snapshot_from_store(&store, "session-1", Some("req-1")).expect("snapshot");
    assert_eq!(snapshot.turn_state.as_deref(), Some("interrupted"));
    assert!(!snapshot
        .timeline_items
        .iter()
        .any(|item| matches!(item, RenderedTimelineItem::LiveAssistant { .. })));
}

#[test]
fn session_snapshot_stays_renderable_across_three_turns_with_stale_conversation_rows() {
    let mut rows = ClientStoreRows {
        sessions: vec![stale_session(RequestLifecycleState::Processing)],
        requests: vec![
            stale_request(RequestLifecycleState::Completed),
            AgentRequestRow {
                doc_id: Some("req-2".into()),
                request_id: "req-2".into(),
                agent_did: Some("did:test:amy".into()),
                behavior_id: Some("amy-default".into()),
                session_id: Some("session-1".into()),
                lifecycle_state: Some(RequestLifecycleState::Completed),
                ..Default::default()
            },
            AgentRequestRow {
                doc_id: Some("req-3".into()),
                request_id: "req-3".into(),
                agent_did: Some("did:test:amy".into()),
                behavior_id: Some("amy-default".into()),
                session_id: Some("session-1".into()),
                content: Some("turn three".into()),
                lifecycle_state: Some(RequestLifecycleState::Processing),
                ..Default::default()
            },
        ],
        ..ClientStoreRows::default()
    };
    rows.sessions[0]
        .observation
        .as_mut()
        .expect("observation")
        .latest_request = Some(SessionRequestObservation {
        request_doc_id: "req-3".into(),
        request_id: "req-3".into(),
        lifecycle_state: RequestLifecycleState::Processing,
    });
    for (sequence, request_id, role, text) in [
        (1, "req-1", MessageRole::User, "turn one"),
        (2, "req-1", MessageRole::Assistant, "answer one"),
        (3, "req-2", MessageRole::User, "turn two"),
        (4, "req-2", MessageRole::Assistant, "answer two"),
    ] {
        push_canonical_text_message(
            &mut rows,
            &format!("message-{sequence}"),
            "session-1",
            Some(request_id),
            sequence,
            role,
            text,
        );
    }
    let snapshot = build_session_snapshot_from_store(
        &ClientStore::from_rows(rows),
        "session-1",
        Some("req-3"),
    )
    .expect("snapshot");
    assert_eq!(snapshot.latest_request_id.as_deref(), Some("req-3"));
    assert_eq!(snapshot.messages.len(), 4);
    assert_eq!(
        snapshot
            .pending_turn
            .as_ref()
            .map(|turn| turn.request_id.as_str()),
        Some("req-3")
    );
}
