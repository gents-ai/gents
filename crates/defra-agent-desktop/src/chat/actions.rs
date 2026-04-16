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
            chat.editor.last_submission_error = None;
            chat.shell.workflow = ChatWorkflowState::Ready;
        }
        ChatAction::SelectConversation { session_id } => {
            chat.shell.selected_session_id = Some(session_id);
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
