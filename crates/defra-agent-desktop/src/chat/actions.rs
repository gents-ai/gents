use crate::chat::domain::submission::{ChatBlockedReason, ChatWorkflowState};
use crate::state::ChatState;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatAction {
    SelectDeployment {
        peer_id: String,
        agent_did: String,
    },
    SelectConversation {
        session_id: String,
    },
    StartNewConversationDraft,
    SelectBehaviorOverride {
        behavior_id: Option<String>,
    },
    CreateConversationStarted {
        agent_did: String,
    },
    ConversationCreated {
        session_id: String,
    },
    SubmitRequestStarted {
        agent_did: String,
        session_id: Option<String>,
    },
    RequestSubmitted {
        session_id: String,
        request_id: String,
    },
    RetryStarted {
        agent_did: String,
        session_id: String,
    },
    RetrySubmitted {
        session_id: String,
        request_id: String,
    },
    MutationFailed {
        error: String,
        blocked_reason: Option<ChatBlockedReason>,
    },
    SnapshotWorkflowApplied {
        workflow: ChatWorkflowState,
    },
}

pub fn reduce(chat: &mut ChatState, action: ChatAction) {
    match action {
        ChatAction::SelectDeployment { peer_id, agent_did } => {
            chat.shell.selected_peer_id = Some(peer_id);
            chat.shell.selected_agent_did = Some(agent_did);
            chat.shell.selected_session_id = None;
            chat.editor.selected_behavior_override = None;
            chat.editor.last_submission_error = None;
            chat.shell.workflow = ChatWorkflowState::Ready;
        }
        ChatAction::SelectConversation { session_id } => {
            chat.shell.selected_session_id = Some(session_id);
            chat.editor.selected_behavior_override = None;
            chat.editor.last_submission_error = None;
        }
        ChatAction::StartNewConversationDraft => {
            chat.shell.selected_session_id = None;
            chat.editor.selected_behavior_override = None;
            chat.editor.last_submission_error = None;
            chat.editor.last_action_message = Some("New conversation ready.".to_string());
            chat.shell.workflow = ChatWorkflowState::Ready;
        }
        ChatAction::SelectBehaviorOverride { behavior_id } => {
            chat.editor.selected_behavior_override = behavior_id;
            chat.editor.last_submission_error = None;
        }
        ChatAction::CreateConversationStarted { agent_did } => {
            chat.editor.last_submission_error = None;
            chat.editor.last_action_message = None;
            chat.editor.last_export_payload = None;
            chat.shell.workflow = ChatWorkflowState::CreatingConversation { agent_did };
        }
        ChatAction::ConversationCreated { session_id } => {
            chat.shell.selected_session_id = Some(session_id);
            chat.editor.last_submission_error = None;
            chat.shell.workflow = ChatWorkflowState::Ready;
            chat.editor.transcript_stick_to_bottom = true;
        }
        ChatAction::SubmitRequestStarted {
            agent_did,
            session_id,
        } => {
            chat.editor.last_submission_error = None;
            chat.editor.last_action_message = None;
            chat.editor.last_export_payload = None;
            chat.shell.workflow = ChatWorkflowState::SubmittingRequest {
                agent_did,
                session_id,
            };
        }
        ChatAction::RequestSubmitted {
            session_id,
            request_id,
        }
        | ChatAction::RetrySubmitted {
            session_id,
            request_id,
        } => {
            chat.shell.selected_session_id = Some(session_id.clone());
            chat.editor.last_submission_error = None;
            chat.editor.last_action_message = None;
            chat.editor.last_export_payload = None;
            chat.editor.composer_text.clear();
            chat.editor.transcript_stick_to_bottom = true;
            chat.shell.workflow = ChatWorkflowState::AwaitingObservation {
                session_id,
                request_id,
            };
        }
        ChatAction::RetryStarted {
            agent_did,
            session_id,
        } => {
            chat.editor.last_submission_error = None;
            chat.editor.last_action_message = None;
            chat.editor.last_export_payload = None;
            chat.shell.workflow = ChatWorkflowState::SubmittingRequest {
                agent_did,
                session_id: Some(session_id),
            };
            chat.editor.transcript_stick_to_bottom = true;
        }
        ChatAction::MutationFailed {
            error,
            blocked_reason,
        } => {
            chat.editor.last_submission_error = Some(error);
            chat.shell.workflow = blocked_reason
                .map(|reason| ChatWorkflowState::Blocked { reason })
                .unwrap_or(ChatWorkflowState::Ready);
        }
        ChatAction::SnapshotWorkflowApplied { workflow } => {
            chat.shell.workflow = workflow;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{ChatEditorState, ChatShellState};

    #[test]
    fn selecting_existing_conversation_clears_behavior_override() {
        let mut chat = ChatState {
            shell: ChatShellState {
                selected_agent_did: Some("did:defra:amy".to_string()),
                ..ChatShellState::default()
            },
            editor: ChatEditorState {
                selected_behavior_override: Some("amy-alt".to_string()),
                ..ChatEditorState::default()
            },
        };

        reduce(
            &mut chat,
            ChatAction::SelectConversation {
                session_id: "session-1".to_string(),
            },
        );

        assert_eq!(chat.shell.selected_session_id.as_deref(), Some("session-1"));
        assert_eq!(chat.editor.selected_behavior_override, None);
    }

    #[test]
    fn starting_new_conversation_draft_unlocks_behavior_selection() {
        let mut chat = ChatState {
            shell: ChatShellState {
                selected_agent_did: Some("did:defra:amy".to_string()),
                selected_session_id: Some("session-1".to_string()),
                workflow: ChatWorkflowState::TurnInProgress {
                    session_id: "session-1".to_string(),
                    request_id: Some("req-1".to_string()),
                    turn_state: defra_agent_protocol::client_protocol::ClientTurnState::Streaming,
                },
                ..ChatShellState::default()
            },
            editor: ChatEditorState {
                selected_behavior_override: Some("amy-alt".to_string()),
                ..ChatEditorState::default()
            },
        };

        reduce(&mut chat, ChatAction::StartNewConversationDraft);

        assert_eq!(chat.shell.selected_session_id, None);
        assert_eq!(chat.editor.selected_behavior_override, None);
        assert_eq!(chat.shell.workflow, ChatWorkflowState::Ready);
    }
}
