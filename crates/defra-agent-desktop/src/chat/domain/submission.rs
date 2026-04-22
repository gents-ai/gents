use defra_agent_protocol::client_protocol::ClientTurnState;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatWorkflowState {
    Ready,
    CreatingConversation {
        agent_did: String,
    },
    SubmittingRequest {
        agent_did: String,
        session_id: Option<String>,
    },
    AwaitingObservation {
        session_id: String,
        request_id: String,
    },
    TurnInProgress {
        session_id: String,
        request_id: Option<String>,
        turn_state: ClientTurnState,
    },
    Blocked {
        reason: ChatBlockedReason,
    },
}

impl Default for ChatWorkflowState {
    fn default() -> Self {
        Self::Ready
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendStatus {
    Ready,
    Disabled(ChatBlockedReason),
}

impl SendStatus {
    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready)
    }

    pub fn is_disabled(&self) -> bool {
        !self.is_ready()
    }

    pub fn blocked_reason(&self) -> Option<&ChatBlockedReason> {
        match self {
            Self::Ready => None,
            Self::Disabled(reason) => Some(reason),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatBlockedReason {
    ClientOffline,
    AgentNotSelected,
    ComposerEmpty,
    CreatingConversation,
    SubmittingRequest,
    WaitingForRequestObservation,
    ConversationMissingFromSnapshot,
    SessionBehaviorMismatch { requested: String, existing: String },
    AwaitingTurnTerminality(ClientTurnState),
    InconsistentTurnObservation,
}

impl ChatBlockedReason {
    pub fn hint(&self) -> String {
        match self {
            Self::ClientOffline => "Client offline".to_string(),
            Self::AgentNotSelected => "Select an agent before sending".to_string(),
            Self::ComposerEmpty => "Type a message to send".to_string(),
            Self::CreatingConversation => "Creating conversation".to_string(),
            Self::SubmittingRequest => "Submitting request".to_string(),
            Self::WaitingForRequestObservation => "Waiting for request observation".to_string(),
            Self::ConversationMissingFromSnapshot => {
                "Conversation missing from snapshot".to_string()
            }
            Self::SessionBehaviorMismatch {
                requested,
                existing,
            } => format!("Session behavior mismatch: requested={requested} existing={existing}"),
            Self::AwaitingTurnTerminality(ClientTurnState::WaitingForClaim) => {
                "Waiting for the active turn to start".to_string()
            }
            Self::AwaitingTurnTerminality(ClientTurnState::Streaming) => {
                "Turn still streaming".to_string()
            }
            Self::AwaitingTurnTerminality(ClientTurnState::Completed)
            | Self::AwaitingTurnTerminality(ClientTurnState::Failed)
            | Self::AwaitingTurnTerminality(ClientTurnState::Superseded)
            | Self::AwaitingTurnTerminality(ClientTurnState::Interrupted) => {
                // TODO(ui-polish): distinguish interrupted from failed in display text/icons.
                // For now treat identically — both mean "no complete response."
                "Waiting for terminal turn reconciliation".to_string()
            }
            Self::InconsistentTurnObservation => {
                "Waiting for consistent turn observation".to_string()
            }
        }
    }
}
