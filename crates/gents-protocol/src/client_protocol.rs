//! Client turn observation protocol.
//!
//! Pure-function projection from agent document snapshots to client-visible
//! turn states. Source of truth: `crates/gents/proofs/Proofs/Client.lean`.
//!
//! The derivation checks server terminal states first, then falls through to
//! response status for non-terminal request states. This ordering prevents
//! stale streaming responses from demoting a failed/completed request, and
//! preserves the monotonicity property proven in the Lean model.

use std::collections::HashSet;
use std::error::Error;
use std::fmt::{Display, Formatter};

/// The 6 client-visible turn states, mirroring `ClientTurnState` in Client.lean.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientTurnState {
    WaitingForClaim,
    Streaming,
    Completed,
    Failed,
    Superseded,
    Interrupted,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestLifecycleState {
    Pending,
    Claimed,
    Processing,
    InputRequired,
    Completed,
    Failed,
    Superseded,
    Dead,
    Interrupted,
}

impl RequestLifecycleState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Claimed => "claimed",
            Self::Processing => "processing",
            Self::InputRequired => "inputRequired",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Superseded => "superseded",
            Self::Dead => "dead",
            Self::Interrupted => "interrupted",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidRequestLifecycleState {
    state: String,
}

impl InvalidRequestLifecycleState {
    pub fn value(&self) -> &str {
        &self.state
    }
}

impl Display for InvalidRequestLifecycleState {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid request lifecycle state: {}", self.state)
    }
}

impl Error for InvalidRequestLifecycleState {}

impl TryFrom<&str> for RequestLifecycleState {
    type Error = InvalidRequestLifecycleState;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "pending" => Ok(Self::Pending),
            "claimed" => Ok(Self::Claimed),
            "processing" => Ok(Self::Processing),
            "inputRequired" => Ok(Self::InputRequired),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "superseded" => Ok(Self::Superseded),
            "dead" => Ok(Self::Dead),
            "interrupted" => Ok(Self::Interrupted),
            _ => Err(InvalidRequestLifecycleState {
                state: value.to_string(),
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseStatus {
    Streaming,
    Complete,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidResponseStatus {
    status: String,
}

impl InvalidResponseStatus {
    pub fn value(&self) -> &str {
        &self.status
    }
}

impl Display for InvalidResponseStatus {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid response status: {}", self.status)
    }
}

impl Error for InvalidResponseStatus {}

impl TryFrom<&str> for ResponseStatus {
    type Error = InvalidResponseStatus;

    fn try_from(value: &str) -> Result<Self, InvalidResponseStatus> {
        match value {
            "streaming" => Ok(Self::Streaming),
            "complete" | "completed" => Ok(Self::Complete),
            "error" | "failed" | "failure" => Ok(Self::Error),
            _ => Err(InvalidResponseStatus {
                status: value.to_string(),
            }),
        }
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
pub struct ResponseSnapshot {
    pub status: ResponseStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptView {
    pub request: RequestSnapshot,
    pub response: Option<ResponseSnapshot>,
}

/// Response-aware state for the current request at the head of a client turn.
///
/// `turn_state` is the durable outcome projection. `request_state` preserves
/// the request-side detail that clients need for presentation distinctions
/// such as pending versus running and waiting for user input.
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

pub fn project_attempt(view: &AttemptView) -> ClientHeadProjection {
    ClientHeadProjection {
        turn_state: derive_observation(
            view.request.lifecycle_state,
            view.request.is_superseded,
            view.response.as_ref().map(|response| response.status),
        ),
        request_state: view.request.lifecycle_state,
    }
}

pub fn derive_attempt(view: &AttemptView) -> ClientTurnState {
    project_attempt(view).turn_state
}

pub fn project_persisted_attempt(
    lifecycle_state: &str,
    is_superseded: bool,
    response_status: Option<&str>,
) -> Option<ClientHeadProjection> {
    let lifecycle_state = RequestLifecycleState::try_from(lifecycle_state.trim()).ok()?;
    let response_status =
        response_status.and_then(|status| ResponseStatus::try_from(status.trim()).ok());
    Some(ClientHeadProjection {
        turn_state: derive_observation(lifecycle_state, is_superseded, response_status),
        request_state: lifecycle_state,
    })
}

pub fn derive_persisted_attempt(
    lifecycle_state: &str,
    is_superseded: bool,
    response_status: Option<&str>,
) -> Option<ClientTurnState> {
    project_persisted_attempt(lifecycle_state, is_superseded, response_status)
        .map(|head| head.turn_state)
}

fn derive_observation(
    lifecycle_state: RequestLifecycleState,
    is_superseded: bool,
    response_status: Option<ResponseStatus>,
) -> ClientTurnState {
    if is_superseded {
        return ClientTurnState::Superseded;
    }

    match lifecycle_state {
        RequestLifecycleState::Superseded => ClientTurnState::Superseded,
        RequestLifecycleState::Completed => ClientTurnState::Completed,
        RequestLifecycleState::Failed | RequestLifecycleState::Dead => ClientTurnState::Failed,
        RequestLifecycleState::Interrupted => ClientTurnState::Interrupted,
        RequestLifecycleState::Pending
        | RequestLifecycleState::Claimed
        | RequestLifecycleState::Processing
        | RequestLifecycleState::InputRequired => match response_status {
            Some(status) => match status {
                ResponseStatus::Complete => ClientTurnState::Completed,
                ResponseStatus::Error => ClientTurnState::Failed,
                ResponseStatus::Streaming => ClientTurnState::Streaming,
            },
            None => ClientTurnState::WaitingForClaim,
        },
    }
}

fn resolve_tip(attempts: &[AttemptView]) -> Option<&AttemptView> {
    if attempts.is_empty() {
        return None;
    }

    let referenced_request_ids: HashSet<&str> = attempts
        .iter()
        .filter_map(|attempt| attempt.request.retry_parent_request.as_deref())
        .filter(|request_id| !request_id.is_empty())
        .collect();

    attempts
        .iter()
        .filter(|attempt| !referenced_request_ids.contains(attempt.request.request_id.as_str()))
        .max_by(|left, right| left.request.request_id.cmp(&right.request.request_id))
        .or_else(|| {
            // Malformed observations can contain cycles or multiple disconnected
            // attempts. Fall back to a deterministic request_id ordering rather
            // than reintroducing slice-order dependence.
            attempts
                .iter()
                .max_by(|left, right| left.request.request_id.cmp(&right.request.request_id))
        })
}

pub fn derive_turn(attempts: &[AttemptView]) -> Option<ClientTurnState> {
    resolve_tip(attempts).map(derive_attempt)
}

#[cfg(test)]
mod tests;
