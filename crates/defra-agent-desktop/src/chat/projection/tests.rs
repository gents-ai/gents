use defra_agent_protocol::client_protocol::ClientTurnState;
use defra_agent_protocol::row::{
    AgentConversationRow, AgentPrincipalRow, AgentRequestRow, AgentResponseRow, AgentSessionRow,
};

use crate::chat::domain::submission::{ChatBlockedReason, ChatWorkflowState, SendStatus};
use crate::client::{ClientStore, ClientStoreRows};
use crate::state::{ChatEditorState, ChatShellState, ChatState};

use super::project_chat;

#[test]
fn projection_preserves_pending_session_until_snapshot_catches_up() {
    let state = ChatState {
        shell: ChatShellState {
            selected_agent_did: Some("did:defra:amy".to_string()),
            selected_session_id: Some("session-pending".to_string()),
            ..ChatShellState::default()
        },
        editor: ChatEditorState {
            composer_text: "follow up".to_string(),
            ..ChatEditorState::default()
        },
        ..ChatState::default()
    };
    let store = ClientStore::from_rows(ClientStoreRows {
        agent_principals: vec![principal("did:defra:amy")],
        ..ClientStoreRows::default()
    });

    let projection = project_chat(&state, &store, true);

    assert_eq!(
        projection.send_status,
        SendStatus::Disabled(ChatBlockedReason::ConversationMissingFromSnapshot)
    );
    assert!(!projection.show_first_conversation_nudge);
    assert!(!projection.session_trustworthy_for_follow_up);
}

#[test]
fn projection_preserves_stale_session_selection_until_explicitly_changed() {
    let state = ChatState {
        shell: ChatShellState {
            selected_agent_did: Some("did:defra:amy".to_string()),
            selected_session_id: Some("session-missing".to_string()),
            ..ChatShellState::default()
        },
        editor: ChatEditorState {
            composer_text: "follow up".to_string(),
            ..ChatEditorState::default()
        },
        ..ChatState::default()
    };
    let store = ClientStore::from_rows(ClientStoreRows {
        agent_principals: vec![principal("did:defra:amy")],
        conversations: vec![
            conversation(
                "session-older",
                "did:defra:amy",
                None,
                "2026-04-14T00:01:00Z",
            ),
            conversation(
                "session-latest",
                "did:defra:amy",
                None,
                "2026-04-14T00:05:00Z",
            ),
        ],
        ..ClientStoreRows::default()
    });

    let projection = project_chat(&state, &store, true);

    assert_eq!(
        projection.send_status,
        SendStatus::Disabled(ChatBlockedReason::ConversationMissingFromSnapshot)
    );
}

#[test]
fn projection_blocks_follow_up_while_turn_is_streaming() {
    let state = ChatState {
        shell: ChatShellState {
            selected_agent_did: Some("did:defra:amy".to_string()),
            selected_session_id: Some("session-1".to_string()),
            ..ChatShellState::default()
        },
        editor: ChatEditorState {
            composer_text: "follow up".to_string(),
            ..ChatEditorState::default()
        },
        ..ChatState::default()
    };
    let store = ClientStore::from_rows(ClientStoreRows {
        agent_principals: vec![principal("did:defra:amy")],
        conversations: vec![conversation(
            "session-1",
            "did:defra:amy",
            Some("req-streaming"),
            "2026-04-14T00:05:00Z",
        )],
        requests: vec![request(
            "req-streaming",
            "session-1",
            "did:defra:amy",
            "processing",
            "2026-04-14T00:04:00Z",
        )],
        responses: vec![response(
            "resp-streaming",
            "req-streaming",
            "session-1",
            "streaming",
            None,
        )],
        ..ClientStoreRows::default()
    });

    let projection = project_chat(&state, &store, true);

    assert_eq!(projection.turn_state, Some(ClientTurnState::Streaming));
    assert_eq!(
        projection.send_status,
        SendStatus::Disabled(ChatBlockedReason::AwaitingTurnTerminality(
            ClientTurnState::Streaming,
        ))
    );
}

#[test]
fn projection_blocks_inconsistent_turn_observation_when_latest_request_is_missing() {
    let state = ChatState {
        shell: ChatShellState {
            selected_agent_did: Some("did:defra:amy".to_string()),
            selected_session_id: Some("session-1".to_string()),
            ..ChatShellState::default()
        },
        editor: ChatEditorState {
            composer_text: "follow up".to_string(),
            ..ChatEditorState::default()
        },
        ..ChatState::default()
    };
    let store = ClientStore::from_rows(ClientStoreRows {
        agent_principals: vec![principal("did:defra:amy")],
        conversations: vec![conversation(
            "session-1",
            "did:defra:amy",
            Some("req-missing"),
            "2026-04-14T00:05:00Z",
        )],
        requests: vec![request(
            "req-observed",
            "session-1",
            "did:defra:amy",
            "completed",
            "2026-04-14T00:04:00Z",
        )],
        ..ClientStoreRows::default()
    });

    let projection = project_chat(&state, &store, true);

    assert_eq!(projection.turn_state, None);
    assert_eq!(
        projection.send_status,
        SendStatus::Disabled(ChatBlockedReason::InconsistentTurnObservation)
    );
}

#[test]
fn projection_allows_follow_up_after_terminal_turn() {
    let state = ChatState {
        shell: ChatShellState {
            selected_agent_did: Some("did:defra:amy".to_string()),
            selected_session_id: Some("session-1".to_string()),
            ..ChatShellState::default()
        },
        editor: ChatEditorState {
            composer_text: "follow up".to_string(),
            ..ChatEditorState::default()
        },
        ..ChatState::default()
    };
    let store = ClientStore::from_rows(ClientStoreRows {
        agent_principals: vec![principal("did:defra:amy")],
        conversations: vec![conversation(
            "session-1",
            "did:defra:amy",
            Some("req-complete"),
            "2026-04-14T00:05:00Z",
        )],
        requests: vec![request(
            "req-complete",
            "session-1",
            "did:defra:amy",
            "completed",
            "2026-04-14T00:04:00Z",
        )],
        responses: vec![response(
            "resp-complete",
            "req-complete",
            "session-1",
            "completed",
            Some("done"),
        )],
        ..ClientStoreRows::default()
    });

    let projection = project_chat(&state, &store, true);

    assert_eq!(projection.turn_state, Some(ClientTurnState::Completed));
    assert_eq!(projection.send_status, SendStatus::Ready);
    assert!(projection.session_trustworthy_for_follow_up);
}

#[test]
fn projection_uses_tracked_request_before_conversation_latest_request_catches_up() {
    let state = ChatState {
        shell: ChatShellState {
            selected_agent_did: Some("did:defra:amy".to_string()),
            selected_session_id: Some("session-1".to_string()),
            workflow: ChatWorkflowState::AwaitingObservation {
                session_id: "session-1".to_string(),
                request_id: "req-new".to_string(),
            },
            ..ChatShellState::default()
        },
        editor: ChatEditorState {
            composer_text: "follow up".to_string(),
            ..ChatEditorState::default()
        },
        ..ChatState::default()
    };
    let store = ClientStore::from_rows(ClientStoreRows {
        agent_principals: vec![principal("did:defra:amy")],
        conversations: vec![conversation(
            "session-1",
            "did:defra:amy",
            Some("req-old"),
            "2026-04-14T00:05:00Z",
        )],
        requests: vec![
            request(
                "req-old",
                "session-1",
                "did:defra:amy",
                "completed",
                "2026-04-14T00:03:00Z",
            ),
            request(
                "req-new",
                "session-1",
                "did:defra:amy",
                "processing",
                "2026-04-14T00:04:00Z",
            ),
        ],
        responses: vec![response(
            "resp-new",
            "req-new",
            "session-1",
            "streaming",
            None,
        )],
        ..ClientStoreRows::default()
    });

    let projection = project_chat(&state, &store, true);

    assert_eq!(projection.turn_state, Some(ClientTurnState::Streaming));
    assert!(matches!(
        projection.workflow,
        ChatWorkflowState::TurnInProgress { .. }
    ));
}

#[test]
fn projection_allows_follow_up_when_conversation_row_is_missing_but_request_is_observed() {
    let state = ChatState {
        shell: ChatShellState {
            selected_agent_did: Some("did:defra:amy".to_string()),
            selected_session_id: Some("session-1".to_string()),
            ..ChatShellState::default()
        },
        editor: ChatEditorState {
            composer_text: "follow up".to_string(),
            ..ChatEditorState::default()
        },
        ..ChatState::default()
    };
    let store = ClientStore::from_rows(ClientStoreRows {
        agent_principals: vec![principal("did:defra:amy")],
        sessions: vec![AgentSessionRow {
            session_id: "session-1".to_string(),
            agent_name: Some("Amy".to_string()),
            behavior_id: Some("amy-default".to_string()),
            started: Some("2026-04-14T00:00:00Z".to_string()),
            ended: None,
            status: Some("active".to_string()),
        }],
        requests: vec![request(
            "req-complete",
            "session-1",
            "did:defra:amy",
            "completed",
            "2026-04-14T00:04:00Z",
        )],
        responses: vec![response(
            "resp-complete",
            "req-complete",
            "session-1",
            "completed",
            Some("done"),
        )],
        ..ClientStoreRows::default()
    });

    let projection = project_chat(&state, &store, true);

    assert_eq!(projection.send_status, SendStatus::Ready);
    assert!(projection.session_trustworthy_for_follow_up);
}

#[test]
fn projection_detects_session_behavior_mismatch() {
    let state = ChatState {
        shell: ChatShellState {
            selected_agent_did: Some("did:defra:amy".to_string()),
            selected_session_id: Some("session-1".to_string()),
            ..ChatShellState::default()
        },
        editor: ChatEditorState {
            selected_behavior_override: Some("amy-alt".to_string()),
            composer_text: "follow up".to_string(),
            ..ChatEditorState::default()
        },
        ..ChatState::default()
    };
    let store = ClientStore::from_rows(ClientStoreRows {
        agent_principals: vec![principal("did:defra:amy")],
        conversations: vec![conversation(
            "session-1",
            "did:defra:amy",
            None,
            "2026-04-14T00:05:00Z",
        )],
        ..ClientStoreRows::default()
    });

    let projection = project_chat(&state, &store, true);

    assert_eq!(
        projection.send_status,
        SendStatus::Disabled(ChatBlockedReason::SessionBehaviorMismatch {
            requested: "amy-alt".to_string(),
            existing: "amy-default".to_string(),
        })
    );
}

fn principal(agent_did: &str) -> AgentPrincipalRow {
    AgentPrincipalRow {
        agent_did: agent_did.to_string(),
        display_name: Some("Amy".to_string()),
        default_behavior_id: Some("amy-default".to_string()),
        enabled: Some(true),
        created_at: None,
        created_by: None,
    }
}

fn conversation(
    session_id: &str,
    agent_did: &str,
    latest_request_id: Option<&str>,
    updated_at: &str,
) -> AgentConversationRow {
    AgentConversationRow {
        session_id: session_id.to_string(),
        agent_name: Some("Amy".to_string()),
        agent_did: Some(agent_did.to_string()),
        behavior_id: Some("amy-default".to_string()),
        title: Some("Conversation".to_string()),
        preview_text: None,
        status: Some("active".to_string()),
        created_at: Some("2026-04-14T00:00:00Z".to_string()),
        updated_at: Some(updated_at.to_string()),
        latest_request_id: latest_request_id.map(str::to_string),
    }
}

fn request(
    request_id: &str,
    session_id: &str,
    agent_did: &str,
    lifecycle_state: &str,
    created_at: &str,
) -> AgentRequestRow {
    AgentRequestRow {
        request_id: request_id.to_string(),
        agent_did: Some(agent_did.to_string()),
        behavior_id: Some("amy-default".to_string()),
        session_id: Some(session_id.to_string()),
        retry_parent_request: None,
        retry_root_request: None,
        superseded_by_request: None,
        content: Some("hello".to_string()),
        status: Some(lifecycle_state.to_string()),
        lifecycle_state: Some(lifecycle_state.to_string()),
        backend_id: None,
        execution_origin: Some("interactive".to_string()),
        failure_reason: None,
        created_at: Some(created_at.to_string()),
        claimed_at: None,
        deadline: None,
        retry_count: Some(0),
        max_retries: Some(3),
        caused_by_trigger_id: None,
        caused_by_trigger_kind: None,
    }
}

fn response(
    response_key: &str,
    request_id: &str,
    session_id: &str,
    status: &str,
    content: Option<&str>,
) -> AgentResponseRow {
    AgentResponseRow {
        response_key: response_key.to_string(),
        request_id: Some(request_id.to_string()),
        agent_did: Some("did:defra:amy".to_string()),
        behavior_id: Some("amy-default".to_string()),
        session_id: Some(session_id.to_string()),
        content: content.map(str::to_string),
        reasoning: None,
        status: Some(status.to_string()),
        error_message: None,
        token_count: None,
        progress_seq: Some(1),
        materialized_message_sequence: None,
        materialized_at: None,
        created_at: Some("2026-04-14T00:04:01Z".to_string()),
        completed_at: Some("2026-04-14T00:04:02Z".to_string()),
    }
}
