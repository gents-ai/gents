//! Client turn observation protocol.
//! response status for non-terminal request states. This ordering prevents

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

pub fn derive_attempt(view: &AttemptView) -> ClientTurnState {
    if view.request.is_superseded {
        return ClientTurnState::Superseded;
    }

    match view.request.lifecycle_state {
        RequestLifecycleState::Superseded => ClientTurnState::Superseded,
        RequestLifecycleState::Completed => ClientTurnState::Completed,
        RequestLifecycleState::Failed | RequestLifecycleState::Dead => ClientTurnState::Failed,
        RequestLifecycleState::Interrupted => ClientTurnState::Interrupted,
        RequestLifecycleState::Pending
        | RequestLifecycleState::Claimed
        | RequestLifecycleState::Processing
        | RequestLifecycleState::InputRequired => match &view.response {
            Some(resp) => match resp.status {
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
            // attempts. Fall back to a deterministic request_id ordering rather
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
