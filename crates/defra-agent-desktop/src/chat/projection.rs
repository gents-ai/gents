use defra_agent_protocol::client_protocol::ClientTurnState;

use crate::chat::domain::submission::{ChatBlockedReason, ChatWorkflowState, SendStatus};
use crate::client::ClientStore;
use crate::state::ChatState;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatProjection {
    pub turn_state: Option<ClientTurnState>,
    pub send_status: SendStatus,
    pub show_first_conversation_nudge: bool,
    pub session_trustworthy_for_follow_up: bool,
    pub workflow: ChatWorkflowState,
}

pub fn project_chat(
    state: &ChatState,
    store: &ClientStore,
    client_available: bool,
) -> ChatProjection {
    let selected_agent_did = state.shell.selected_agent_did.as_deref();
    let selected_session_id = state.shell.selected_session_id.as_deref();

    let request_context = request_context(state, store, selected_session_id, selected_agent_did);
    let show_first_conversation_nudge = selected_agent_did.is_some_and(|agent_did| {
        selected_session_id.is_none() && store.conversation_rows(agent_did).is_empty()
    });
    let session_trustworthy_for_follow_up =
        session_trustworthy_for_follow_up(&request_context, selected_session_id);
    let send_status = project_send_status(
        state,
        client_available,
        selected_agent_did,
        selected_session_id,
        &request_context,
    );
    let workflow = project_workflow(
        &state.shell.workflow,
        &request_context,
        selected_session_id,
        selected_agent_did,
        client_available,
    );

    ChatProjection {
        turn_state: request_context.turn_state,
        send_status,
        show_first_conversation_nudge,
        session_trustworthy_for_follow_up,
        workflow,
    }
}

fn project_send_status(
    state: &ChatState,
    client_available: bool,
    selected_agent_did: Option<&str>,
    selected_session_id: Option<&str>,
    request_context: &RequestContext,
) -> SendStatus {
    if !client_available {
        return SendStatus::Disabled(ChatBlockedReason::ClientOffline);
    }
    if selected_agent_did.is_none() {
        return SendStatus::Disabled(ChatBlockedReason::AgentNotSelected);
    }
    if state.editor.composer_text.trim().is_empty() {
        return SendStatus::Disabled(ChatBlockedReason::ComposerEmpty);
    }

    match &state.shell.workflow {
        ChatWorkflowState::CreatingConversation { .. } => {
            return SendStatus::Disabled(ChatBlockedReason::CreatingConversation);
        }
        ChatWorkflowState::SubmittingRequest { .. } => {
            return SendStatus::Disabled(ChatBlockedReason::SubmittingRequest);
        }
        ChatWorkflowState::AwaitingObservation {
            session_id,
            request_id,
        } if selected_session_id == Some(session_id.as_str())
            && !request_context.observed_request_ids.contains(request_id) =>
        {
            return SendStatus::Disabled(ChatBlockedReason::WaitingForRequestObservation);
        }
        _ => {}
    }

    if let Some(reason) = request_context.behavior_mismatch.clone() {
        return SendStatus::Disabled(reason);
    }

    let Some(_session_id) = selected_session_id else {
        return SendStatus::Ready;
    };

    if !request_context.observation.is_observed() {
        return SendStatus::Disabled(ChatBlockedReason::ConversationMissingFromSnapshot);
    }
    if !request_context.observation.has_turn_rows() {
        return SendStatus::Ready;
    }

    match request_context.turn_state {
        Some(turn_state) if !turn_state.is_terminal() => {
            SendStatus::Disabled(ChatBlockedReason::AwaitingTurnTerminality(turn_state))
        }
        Some(_) => SendStatus::Ready,
        None => SendStatus::Disabled(ChatBlockedReason::InconsistentTurnObservation),
    }
}

fn project_workflow(
    local_workflow: &ChatWorkflowState,
    request_context: &RequestContext,
    selected_session_id: Option<&str>,
    selected_agent_did: Option<&str>,
    client_available: bool,
) -> ChatWorkflowState {
    match local_workflow {
        ChatWorkflowState::CreatingConversation { agent_did } => {
            if selected_session_id.is_some() {
                ChatWorkflowState::Ready
            } else {
                ChatWorkflowState::CreatingConversation {
                    agent_did: agent_did.clone(),
                }
            }
        }
        ChatWorkflowState::SubmittingRequest {
            agent_did,
            session_id,
        } => ChatWorkflowState::SubmittingRequest {
            agent_did: agent_did.clone(),
            session_id: session_id.clone(),
        },
        ChatWorkflowState::AwaitingObservation {
            session_id,
            request_id,
        } => {
            if request_context.observed_request_ids.contains(request_id) {
                match request_context.turn_state {
                    Some(turn_state) if !turn_state.is_terminal() => {
                        ChatWorkflowState::TurnInProgress {
                            session_id: session_id.clone(),
                            request_id: Some(request_id.clone()),
                            turn_state,
                        }
                    }
                    Some(_) => ChatWorkflowState::Ready,
                    None => ChatWorkflowState::Blocked {
                        reason: ChatBlockedReason::InconsistentTurnObservation,
                    },
                }
            } else {
                ChatWorkflowState::AwaitingObservation {
                    session_id: session_id.clone(),
                    request_id: request_id.clone(),
                }
            }
        }
        ChatWorkflowState::TurnInProgress {
            session_id,
            request_id,
            ..
        } => match request_id
            .as_deref()
            .and_then(|request_id| request_context.turn_state_for_request(request_id))
        {
            Some(turn_state) if !turn_state.is_terminal() => ChatWorkflowState::TurnInProgress {
                session_id: session_id.clone(),
                request_id: request_id.clone(),
                turn_state,
            },
            Some(_) => ChatWorkflowState::Ready,
            None => ChatWorkflowState::Blocked {
                reason: ChatBlockedReason::InconsistentTurnObservation,
            },
        },
        ChatWorkflowState::Blocked { .. } | ChatWorkflowState::Ready => {
            if !client_available {
                return ChatWorkflowState::Blocked {
                    reason: ChatBlockedReason::ClientOffline,
                };
            }
            if let Some(reason) = request_context.behavior_mismatch.clone() {
                return ChatWorkflowState::Blocked { reason };
            }
            if let Some(session_id) = selected_session_id {
                if !request_context.observation.is_observed()
                    && request_context.targets_selected_session
                {
                    return ChatWorkflowState::Blocked {
                        reason: ChatBlockedReason::ConversationMissingFromSnapshot,
                    };
                }
                if let Some(turn_state) = request_context.turn_state {
                    if !turn_state.is_terminal() {
                        return ChatWorkflowState::TurnInProgress {
                            session_id: session_id.to_string(),
                            request_id: request_context.active_request_id.clone(),
                            turn_state,
                        };
                    }
                } else if request_context.observation.has_turn_rows() {
                    return ChatWorkflowState::Blocked {
                        reason: ChatBlockedReason::InconsistentTurnObservation,
                    };
                }
            }
            if selected_agent_did.is_some() {
                ChatWorkflowState::Ready
            } else {
                local_workflow.clone()
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SessionObservation {
    has_conversation: bool,
    has_session_row: bool,
    has_requests: bool,
    has_responses: bool,
    has_messages: bool,
    has_tool_calls: bool,
    has_tool_results: bool,
}

impl SessionObservation {
    fn is_observed(self) -> bool {
        self.has_conversation
            || self.has_session_row
            || self.has_requests
            || self.has_responses
            || self.has_messages
            || self.has_tool_calls
            || self.has_tool_results
    }

    fn has_turn_rows(self) -> bool {
        self.has_requests || self.has_responses
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RequestContext {
    observation: SessionObservation,
    active_request_id: Option<String>,
    observed_request_ids: Vec<String>,
    turn_state: Option<ClientTurnState>,
    behavior_mismatch: Option<ChatBlockedReason>,
    targets_selected_session: bool,
}

impl RequestContext {
    fn turn_state_for_request(&self, request_id: &str) -> Option<ClientTurnState> {
        if self
            .active_request_id
            .as_deref()
            .is_some_and(|active_request_id| active_request_id == request_id)
        {
            return self.turn_state;
        }

        None
    }
}

fn request_context(
    state: &ChatState,
    store: &ClientStore,
    selected_session_id: Option<&str>,
    selected_agent_did: Option<&str>,
) -> RequestContext {
    let Some(session_id) = selected_session_id else {
        return RequestContext {
            observation: SessionObservation {
                has_conversation: false,
                has_session_row: false,
                has_requests: false,
                has_responses: false,
                has_messages: false,
                has_tool_calls: false,
                has_tool_results: false,
            },
            active_request_id: None,
            observed_request_ids: Vec::new(),
            turn_state: None,
            behavior_mismatch: None,
            targets_selected_session: false,
        };
    };

    let observation = observe_session(store, session_id);
    let tracked_request_id = tracked_request_id_for_session(&state.shell.workflow, session_id);
    let observed_request_ids = store
        .requests_for_session(session_id)
        .iter()
        .map(|row| row.request_id.clone())
        .collect::<Vec<_>>();
    let active_request_id = tracked_request_id
        .filter(|request_id| {
            observed_request_ids
                .iter()
                .any(|observed| observed == request_id)
        })
        .or_else(|| store.latest_request_id_for_session(session_id));
    let turn_state = active_request_id
        .as_deref()
        .and_then(|request_id| store.derive_turn_for_request(request_id))
        .or_else(|| store.derive_turn(session_id));
    let behavior_mismatch = selected_agent_did.and_then(|agent_did| {
        session_behavior_mismatch(
            state.editor.selected_behavior_override.as_deref(),
            store,
            session_id,
            agent_did,
        )
    });

    RequestContext {
        observation,
        active_request_id,
        observed_request_ids,
        turn_state,
        behavior_mismatch,
        targets_selected_session: true,
    }
}

fn tracked_request_id_for_session(
    workflow: &ChatWorkflowState,
    session_id: &str,
) -> Option<String> {
    match workflow {
        ChatWorkflowState::AwaitingObservation {
            session_id: tracked_session_id,
            request_id,
        } if tracked_session_id == session_id => Some(request_id.clone()),
        ChatWorkflowState::TurnInProgress {
            session_id: tracked_session_id,
            request_id,
            ..
        } if tracked_session_id == session_id => request_id.clone(),
        _ => None,
    }
}

fn session_trustworthy_for_follow_up(
    request_context: &RequestContext,
    selected_session_id: Option<&str>,
) -> bool {
    selected_session_id.is_none()
        || (request_context.observation.is_observed()
            && (!request_context.observation.has_turn_rows()
                || request_context.turn_state.is_some())
            && request_context.behavior_mismatch.is_none())
}

fn observe_session(store: &ClientStore, session_id: &str) -> SessionObservation {
    let transcript = store.transcript(session_id);
    SessionObservation {
        has_conversation: store
            .conversations
            .iter()
            .any(|row| row.session_id == session_id),
        has_session_row: store
            .sessions
            .iter()
            .any(|row| row.session_id == session_id),
        has_requests: !store.requests_for_session(session_id).is_empty(),
        has_responses: store
            .responses
            .iter()
            .any(|row| row.session_id.as_deref() == Some(session_id)),
        has_messages: !transcript.messages.is_empty(),
        has_tool_calls: !transcript.tool_calls.is_empty(),
        has_tool_results: !transcript.tool_results.is_empty(),
    }
}

fn session_behavior_mismatch(
    requested_behavior_id: Option<&str>,
    store: &ClientStore,
    session_id: &str,
    agent_did: &str,
) -> Option<ChatBlockedReason> {
    let requested = normalize_optional_string(requested_behavior_id)?;
    let existing = store
        .conversations
        .iter()
        .find(|row| row.session_id == session_id && row.agent_did.as_deref() == Some(agent_did))
        .and_then(|row| normalize_optional_string(row.behavior_id.as_deref()))
        .or_else(|| {
            store
                .sessions
                .iter()
                .find(|row| row.session_id == session_id)
                .and_then(|row| normalize_optional_string(row.behavior_id.as_deref()))
        })?;

    if existing == requested {
        return None;
    }

    Some(ChatBlockedReason::SessionBehaviorMismatch {
        requested,
        existing,
    })
}

fn normalize_optional_string(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    use defra_agent_protocol::row::{
        AgentConversationRow, AgentPrincipalRow, AgentRequestRow, AgentResponseRow, AgentSessionRow,
    };

    use crate::client::ClientStoreRows;
    use crate::state::ChatState;

    #[test]
    fn projection_preserves_pending_session_until_snapshot_catches_up() {
        let state = ChatState {
            shell: crate::state::ChatShellState {
                selected_agent_did: Some("did:defra:amy".to_string()),
                selected_session_id: Some("session-pending".to_string()),
                ..crate::state::ChatShellState::default()
            },
            editor: crate::state::ChatEditorState {
                composer_text: "follow up".to_string(),
                ..crate::state::ChatEditorState::default()
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
            shell: crate::state::ChatShellState {
                selected_agent_did: Some("did:defra:amy".to_string()),
                selected_session_id: Some("session-missing".to_string()),
                ..crate::state::ChatShellState::default()
            },
            editor: crate::state::ChatEditorState {
                composer_text: "follow up".to_string(),
                ..crate::state::ChatEditorState::default()
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
            shell: crate::state::ChatShellState {
                selected_agent_did: Some("did:defra:amy".to_string()),
                selected_session_id: Some("session-1".to_string()),
                ..crate::state::ChatShellState::default()
            },
            editor: crate::state::ChatEditorState {
                composer_text: "follow up".to_string(),
                ..crate::state::ChatEditorState::default()
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
            shell: crate::state::ChatShellState {
                selected_agent_did: Some("did:defra:amy".to_string()),
                selected_session_id: Some("session-1".to_string()),
                ..crate::state::ChatShellState::default()
            },
            editor: crate::state::ChatEditorState {
                composer_text: "follow up".to_string(),
                ..crate::state::ChatEditorState::default()
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
            shell: crate::state::ChatShellState {
                selected_agent_did: Some("did:defra:amy".to_string()),
                selected_session_id: Some("session-1".to_string()),
                ..crate::state::ChatShellState::default()
            },
            editor: crate::state::ChatEditorState {
                composer_text: "follow up".to_string(),
                ..crate::state::ChatEditorState::default()
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
            shell: crate::state::ChatShellState {
                selected_agent_did: Some("did:defra:amy".to_string()),
                selected_session_id: Some("session-1".to_string()),
                workflow: ChatWorkflowState::AwaitingObservation {
                    session_id: "session-1".to_string(),
                    request_id: "req-new".to_string(),
                },
                ..crate::state::ChatShellState::default()
            },
            editor: crate::state::ChatEditorState {
                composer_text: "follow up".to_string(),
                ..crate::state::ChatEditorState::default()
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
            shell: crate::state::ChatShellState {
                selected_agent_did: Some("did:defra:amy".to_string()),
                selected_session_id: Some("session-1".to_string()),
                ..crate::state::ChatShellState::default()
            },
            editor: crate::state::ChatEditorState {
                composer_text: "follow up".to_string(),
                ..crate::state::ChatEditorState::default()
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
            shell: crate::state::ChatShellState {
                selected_agent_did: Some("did:defra:amy".to_string()),
                selected_session_id: Some("session-1".to_string()),
                ..crate::state::ChatShellState::default()
            },
            editor: crate::state::ChatEditorState {
                selected_behavior_override: Some("amy-alt".to_string()),
                composer_text: "follow up".to_string(),
                ..crate::state::ChatEditorState::default()
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
            created_at: Some("2026-04-14T00:04:01Z".to_string()),
            completed_at: Some("2026-04-14T00:04:02Z".to_string()),
        }
    }
}
