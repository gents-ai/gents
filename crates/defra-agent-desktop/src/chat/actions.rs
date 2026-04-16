use crate::chat::domain::submission::{ChatBlockedReason, ChatWorkflowState};
use crate::chat::projection::ChatProjection;
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
    ProjectionApplied {
        projection: ChatProjection,
    },
}

pub fn reduce(chat: &mut ChatState, action: ChatAction) {
    match action {
        ChatAction::SelectDeployment { peer_id, agent_did } => {
            chat.selected_peer_id = Some(peer_id);
            chat.selected_agent_did = Some(agent_did);
            chat.selected_session_id = None;
            chat.suppress_session_autoselect = true;
            chat.last_submission_error = None;
            chat.workflow = ChatWorkflowState::Ready;
        }
        ChatAction::SelectConversation { session_id } => {
            chat.selected_session_id = Some(session_id);
            chat.suppress_session_autoselect = false;
            chat.last_submission_error = None;
        }
        ChatAction::CreateConversationStarted { agent_did } => {
            chat.last_submission_error = None;
            chat.last_action_message = None;
            chat.last_export_payload = None;
            chat.workflow = ChatWorkflowState::CreatingConversation { agent_did };
        }
        ChatAction::ConversationCreated { session_id } => {
            chat.selected_session_id = Some(session_id);
            chat.suppress_session_autoselect = false;
            chat.last_submission_error = None;
            chat.workflow = ChatWorkflowState::Ready;
            chat.transcript_stick_to_bottom = true;
        }
        ChatAction::SubmitRequestStarted {
            agent_did,
            session_id,
        } => {
            chat.last_submission_error = None;
            chat.last_action_message = None;
            chat.last_export_payload = None;
            chat.workflow = ChatWorkflowState::SubmittingRequest {
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
            chat.selected_session_id = Some(session_id.clone());
            chat.suppress_session_autoselect = false;
            chat.last_submission_error = None;
            chat.last_action_message = None;
            chat.last_export_payload = None;
            chat.composer_text.clear();
            chat.transcript_stick_to_bottom = true;
            chat.workflow = ChatWorkflowState::AwaitingObservation {
                session_id,
                request_id,
            };
        }
        ChatAction::RetryStarted {
            agent_did,
            session_id,
        } => {
            chat.last_submission_error = None;
            chat.last_action_message = None;
            chat.last_export_payload = None;
            chat.workflow = ChatWorkflowState::SubmittingRequest {
                agent_did,
                session_id: Some(session_id),
            };
            chat.transcript_stick_to_bottom = true;
        }
        ChatAction::MutationFailed {
            error,
            blocked_reason,
        } => {
            chat.last_submission_error = Some(error);
            chat.workflow = blocked_reason
                .map(|reason| ChatWorkflowState::Blocked { reason })
                .unwrap_or(ChatWorkflowState::Ready);
        }
        ChatAction::ProjectionApplied { projection } => {
            chat.selected_peer_id = projection.selected_peer_id;
            chat.selected_agent_did = projection.selected_agent_did;
            chat.selected_session_id = projection.selected_session_id;
            if chat.selected_session_id.is_some() {
                chat.suppress_session_autoselect = false;
            }
            chat.workflow = projection.workflow;
        }
    }
}
