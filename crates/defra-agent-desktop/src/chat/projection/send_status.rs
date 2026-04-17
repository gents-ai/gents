use crate::chat::domain::submission::{ChatBlockedReason, ChatWorkflowState, SendStatus};
use crate::state::ChatState;

use super::context::RequestContext;

pub(super) fn project_send_status(
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
