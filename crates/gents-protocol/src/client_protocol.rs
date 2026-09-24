//! Client turn observation contract (#1571).
//!
//! Execution status comes only from request lifecycle and supersession. Output
//! visibility comes separately from shared output reconstruction: missing replicated
//! dependencies do not demote a terminal request, and visible bytes do not complete
//! an active one. `Running` does not assert that bytes are currently arriving.
//!
//! Implements the request-only projection in `Proofs/Client/Types.lean`.

use std::collections::HashSet;

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
/// such as workspace binding. Output readiness,
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
}

pub fn derive_attempt(view: &AttemptView) -> ClientTurnState {
    use RequestLifecycleState as Request;
    if view.request.is_superseded {
        return ClientTurnState::Superseded;
    }
    match view.request.lifecycle_state {
        Request::WorkspaceBindingPending | Request::Pending => ClientTurnState::WaitingForClaim,
        Request::Claimed | Request::Processing => ClientTurnState::Running,
        Request::Completed => ClientTurnState::Completed,
        Request::Failed | Request::Dead => ClientTurnState::Failed,
        Request::Superseded => ClientTurnState::Superseded,
        Request::Interrupted => ClientTurnState::Interrupted,
    }
}

pub fn project_attempt(view: &AttemptView) -> ClientHeadProjection {
    ClientHeadProjection {
        turn_state: derive_attempt(view),
        request_state: view.request.lifecycle_state,
    }
}

/// Request-only projection of one persisted attempt.
///
/// Two arguments only: the persisted lifecycle state and whether the request is
/// superseded. Persisted responses carry no execution facts, so there is no
/// response argument. Returns `None` when the persisted state is not a valid
/// lifecycle value.
pub fn derive_persisted_attempt(
    lifecycle_state: &str,
    is_superseded: bool,
) -> Option<ClientTurnState> {
    project_persisted_attempt(lifecycle_state, is_superseded).map(|view| view.turn_state)
}

/// Request-only head projection of one persisted attempt.
///
/// Same two arguments as [`derive_persisted_attempt`]; see there for why there
/// is no response argument. Returns `None` when the persisted state is not a
/// valid lifecycle value.
pub fn project_persisted_attempt(
    lifecycle_state: &str,
    is_superseded: bool,
) -> Option<ClientHeadProjection> {
    let lifecycle_state = RequestLifecycleState::parse(lifecycle_state).ok()?;
    Some(project_attempt(&AttemptView {
        request: RequestSnapshot {
            request_id: String::new(),
            retry_parent_request: None,
            lifecycle_state,
            is_superseded,
        },
    }))
}

/// Unordered retry-tip resolution over already-authorized attempts.
///
/// The caller supplies the scoped candidate set (agent/session scoping is the
/// caller's authorization decision, not re-derived here). The turn head is the
/// exactly one candidate that no other candidate references as its retry
/// parent. Duplicate request IDs and zero or multiple tips return `None`.
/// Closed cycles have no tip; this is not complete graph validation.
pub fn derive_turn(attempts: &[AttemptView]) -> Option<ClientTurnState> {
    let mut seen = HashSet::new();
    for attempt in attempts {
        if !seen.insert(attempt.request.request_id.as_str()) {
            return None;
        }
    }

    // Candidate tips are exactly the attempts whose ID is not named as a retry
    // parent by another attempt (Proofs/Client/Types.lean `retryTips`).
    let parents: HashSet<_> = attempts
        .iter()
        .filter_map(|attempt| attempt.request.retry_parent_request.as_deref())
        .collect();
    let mut tips = attempts
        .iter()
        .filter(|attempt| !parents.contains(attempt.request.request_id.as_str()));

    // Exactly one tip, or fail closed: zero tips (including a closed cycle,
    // where every member is referenced as a parent) and multiple-tip ambiguity
    // are both rejected. This is a projection, not complete graph validation.
    match (tips.next(), tips.next()) {
        (Some(tip), None) => Some(derive_attempt(tip)),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
