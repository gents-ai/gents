//! Client turn observation contract (#1571).
//!
//! Execution status comes only from request lifecycle and supersession. Output
//! visibility comes separately from shared output reconstruction: missing replicated
//! dependencies do not demote a terminal request, and visible bytes do not complete
//! an active one. `Running` does not assert that bytes are currently arriving.
//!
//! The response-aware projection is retired. Update `Proofs/Client.lean`, then
//! generated conformance, before implementing the request-only projection here.
//! See `contracts/canonical-output.md` for the target mapping and deletion owners.

pub use crate::request_lifecycle::{InvalidRequestLifecycleState, RequestLifecycleState};

/// Client execution indicators, independent of output availability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientTurnState {
    WaitingForClaim,
    Running,
    Completed,
    Failed,
    Superseded,
    Interrupted,
}

impl ClientTurnState {
    /// Terminal states share rank 2. Transition properties belong to Client.lean.
    pub fn rank(self) -> u32 {
        match self {
            Self::WaitingForClaim => 0,
            Self::Running => 1,
            Self::Completed => 2,
            Self::Failed => 2,
            Self::Superseded => 2,
            Self::Interrupted => 2,
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Superseded | Self::Interrupted
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestSnapshot {
    pub request_id: String,
    pub retry_parent_request: Option<String>,
    pub lifecycle_state: RequestLifecycleState,
    pub is_superseded: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptView {
    pub request: RequestSnapshot,
}

/// Request-only state for the current request at the head of a client turn.
///
/// `turn_state` is the execution indicator. `request_state` preserves detail
/// such as workspace binding and waiting for user input. Output readiness,
/// live previews and message completeness are not execution states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientHeadProjection {
    pub turn_state: ClientTurnState,
    pub request_state: RequestLifecycleState,
}

impl ClientHeadProjection {
    pub fn is_terminal(self) -> bool {
        self.turn_state.is_terminal()
    }

    pub fn is_active(self) -> bool {
        !self.is_terminal()
    }

    pub fn waiting_on_user_input(self) -> bool {
        self.is_active() && self.request_state == RequestLifecycleState::InputRequired
    }
}

// Existing cases remain as handoff evidence; migrate after Client.lean, not by
// restoring ResponseStatus or adapting the old response-precedence table.
#[cfg(test)]
mod tests;
