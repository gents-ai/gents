//! Client turn observation protocol.
//!
//! Pure-function projection from agent document snapshots to client-visible
//! turn states. Source of truth: `crates/defra-agent/proofs/Proofs/Client.lean`.
//!
//! The derivation checks server terminal states first, then falls through to
//! response status for non-terminal request states. This ordering prevents
//! stale streaming responses from demoting a failed/completed request, and
//! preserves the monotonicity property proven in the Lean model.

/// The 5 client-visible turn states, mirroring `ClientTurnState` in Client.lean.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientTurnState {
    WaitingForClaim,
    Streaming,
    Completed,
    Failed,
    Superseded,
}

impl ClientTurnState {
    /// Monotonic rank for ordering. Terminal states share rank 2.
    pub fn rank(self) -> u32 {
        match self {
            Self::WaitingForClaim => 0,
            Self::Streaming => 1,
            Self::Completed => 2,
            Self::Failed => 2,
            Self::Superseded => 2,
        }
    }

    /// Whether this state is terminal.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Superseded)
    }
}

/// Response status as observed by the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseStatus {
    Streaming,
    Complete,
    Error,
}

/// Snapshot of an AgentRequest, containing only derivation-relevant fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestSnapshot {
    pub lifecycle_state: String,
    pub is_superseded: bool,
}

/// Snapshot of an AgentResponse, containing only derivation-relevant fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponseSnapshot {
    pub status: ResponseStatus,
}

/// A single attempt observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptView {
    pub request: RequestSnapshot,
    pub response: Option<ResponseSnapshot>,
}

/// Derive client turn state from a single attempt.
///
/// Priority:
/// 1. Supersession flag → Superseded
/// 2. Server terminal lifecycle → terminal client state
/// 3. Non-terminal lifecycle + response → trust response
/// 4. Non-terminal lifecycle + no response → WaitingForClaim
pub fn derive_attempt(view: &AttemptView) -> ClientTurnState {
    if view.request.is_superseded {
        return ClientTurnState::Superseded;
    }

    match view.request.lifecycle_state.as_str() {
        "superseded" => ClientTurnState::Superseded,
        "completed" => ClientTurnState::Completed,
        "failed" => ClientTurnState::Failed,
        "dead" => ClientTurnState::Failed,
        // Non-terminal: defer to response
        _ => match &view.response {
            Some(resp) => match resp.status {
                ResponseStatus::Complete => ClientTurnState::Completed,
                ResponseStatus::Error => ClientTurnState::Failed,
                ResponseStatus::Streaming => ClientTurnState::Streaming,
            },
            None => ClientTurnState::WaitingForClaim,
        },
    }
}

/// Derive client turn state from a full retry chain.
///
/// The last element is the tip (most recent attempt). Returns `None`
/// for empty chains.
pub fn derive_turn(attempts: &[AttemptView]) -> Option<ClientTurnState> {
    attempts.last().map(derive_attempt)
}

#[cfg(test)]
mod tests;
