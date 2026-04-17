use crate::chat::domain::submission::{ChatBlockedReason, ChatWorkflowState};

use super::context::RequestContext;

pub(super) fn project_workflow(
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
