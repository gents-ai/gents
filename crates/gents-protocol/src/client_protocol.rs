use rapidhash::{HashSetExt, RapidHashSet};

pub use crate::request_lifecycle::{InvalidRequestLifecycleState, RequestLifecycleState};

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

pub fn derive_persisted_attempt(
    lifecycle_state: &str,
    is_superseded: bool,
) -> Option<ClientTurnState> {
    project_persisted_attempt(lifecycle_state, is_superseded).map(|view| view.turn_state)
}

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

pub fn derive_turn(attempts: &[AttemptView]) -> Option<ClientTurnState> {
    let mut seen = RapidHashSet::new();
    for attempt in attempts {
        if !seen.insert(attempt.request.request_id.as_str()) {
            return None;
        }
    }

    let parents: RapidHashSet<_> = attempts
        .iter()
        .filter_map(|attempt| attempt.request.retry_parent_request.as_deref())
        .collect();
    let mut tips = attempts
        .iter()
        .filter(|attempt| !parents.contains(attempt.request.request_id.as_str()));

    match (tips.next(), tips.next()) {
        (Some(tip), None) => Some(derive_attempt(tip)),
        _ => None,
    }
}

/// Signal 2 of `deriveSessionDelivery` in `Proofs/Client/Types.lean`: whether
/// the client-to-runtime pairing for the target node replicates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairingSignal {
    Connected,
    NotConnected,
}

/// Signal 4: the canonical `AgentSession` rows a selected-session read found
/// for its exact node/session scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionRowObservation {
    Absent,
    Canonical,
    Duplicate,
    OutOfScope,
}

impl SessionRowObservation {
    pub fn observe(rows: usize, all_in_scope: bool) -> Self {
        match rows {
            _ if !all_in_scope => Self::OutOfScope,
            0 => Self::Absent,
            1 => Self::Canonical,
            _ => Self::Duplicate,
        }
    }
}

/// Signals 1 and 3: the `AgentRequest` this client wrote, with the node
/// runtime's replicated `claimed_at`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmittedAttempt {
    pub attempt: AttemptView,
    pub claimed: bool,
}

impl SubmittedAttempt {
    /// Signal 3: only the node runtime claims a submitted request or moves
    /// its lifecycle past the pending states it is written in.
    pub fn node_has_it(&self) -> bool {
        self.claimed
            || !matches!(
                self.attempt.request.lifecycle_state,
                RequestLifecycleState::WorkspaceBindingPending | RequestLifecycleState::Pending
            )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionDeliveryView {
    pub submitted: Option<SubmittedAttempt>,
    pub pairing: PairingSignal,
    pub session_row: SessionRowObservation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionDelivery {
    Visible,
    Accepted,
    Delivering,
    NotConnected,
    NoRequest,
    IntegrityFault,
}

pub fn derive_session_delivery(view: &SessionDeliveryView) -> SessionDelivery {
    match (view.session_row, view.submitted.as_ref()) {
        (SessionRowObservation::Canonical, _) => SessionDelivery::Visible,
        (SessionRowObservation::Duplicate | SessionRowObservation::OutOfScope, _) => {
            SessionDelivery::IntegrityFault
        }
        (SessionRowObservation::Absent, None) => SessionDelivery::NoRequest,
        (SessionRowObservation::Absent, Some(attempt)) if attempt.node_has_it() => {
            SessionDelivery::Accepted
        }
        (SessionRowObservation::Absent, Some(_)) => match view.pairing {
            PairingSignal::Connected => SessionDelivery::Delivering,
            PairingSignal::NotConnected => SessionDelivery::NotConnected,
        },
    }
}

#[cfg(test)]
mod tests;
