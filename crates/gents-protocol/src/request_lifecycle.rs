use std::error::Error;
use std::fmt::{Display, Formatter};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The TypeScript union is derived from the variant names, not from the
/// manual serde impls; `typescript_union_matches_wire_tokens` binds it to `as_str`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(rename_all = "camelCase"))]
pub enum RequestLifecycleState {
    WorkspaceBindingPending,
    Pending,
    Claimed,
    Processing,
    Completed,
    Failed,
    Superseded,
    Dead,
    Interrupted,
}

impl RequestLifecycleState {
    pub const ALL: [Self; 9] = [
        Self::WorkspaceBindingPending,
        Self::Pending,
        Self::Claimed,
        Self::Processing,
        Self::Completed,
        Self::Failed,
        Self::Superseded,
        Self::Dead,
        Self::Interrupted,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WorkspaceBindingPending => "workspaceBindingPending",
            Self::Pending => "pending",
            Self::Claimed => "claimed",
            Self::Processing => "processing",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Superseded => "superseded",
            Self::Dead => "dead",
            Self::Interrupted => "interrupted",
        }
    }

    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Superseded | Self::Dead | Self::Interrupted
        )
    }

    /// A live terminal owner can certify no provider work only before the
    /// Processing fence, which precedes title dispatch and compaction as well
    /// as inference. Mirrors Lean `Request.Projection.stoppedBeforeRunning`.
    pub const fn certifies_stopped_before_running(self, target: Self, owned: bool) -> bool {
        owned && matches!(self, Self::Claimed) && matches!(target, Self::Interrupted)
    }

    pub const fn is_claimable(self) -> bool {
        matches!(self, Self::Pending)
    }

    pub const fn is_active_runtime(self) -> bool {
        matches!(self, Self::Pending | Self::Claimed | Self::Processing)
    }

    pub fn parse(value: &str) -> Result<Self, InvalidRequestLifecycleState> {
        match value {
            "workspaceBindingPending" => Ok(Self::WorkspaceBindingPending),
            "pending" => Ok(Self::Pending),
            "claimed" => Ok(Self::Claimed),
            "processing" => Ok(Self::Processing),
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

    pub fn parse_opt(value: Option<&str>) -> Option<Self> {
        value.and_then(|value| Self::parse(value).ok())
    }

    pub fn is_terminal_str(value: Option<&str>) -> bool {
        Self::parse_opt(value).is_some_and(Self::is_terminal)
    }

    pub fn graphql_list(states: impl IntoIterator<Item = Self>) -> String {
        let quoted: Vec<String> = states
            .into_iter()
            .map(|state| format!("\"{}\"", state.as_str()))
            .collect();
        format!("[{}]", quoted.join(", "))
    }

    pub fn terminal_graphql_list() -> String {
        Self::graphql_list(Self::ALL.into_iter().filter(|state| state.is_terminal()))
    }

    pub fn active_runtime_graphql_list() -> String {
        Self::graphql_list(
            Self::ALL
                .into_iter()
                .filter(|state| state.is_active_runtime()),
        )
    }

    pub fn nonterminal_graphql_list() -> String {
        Self::graphql_list(Self::ALL.into_iter().filter(|state| !state.is_terminal()))
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
        Self::parse(value)
    }
}

impl Display for RequestLifecycleState {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl Serialize for RequestLifecycleState {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for RequestLifecycleState {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone)]
pub struct EnqueuedAgentRequest {
    pub doc_id: String,
    pub request_id: String,
    pub session_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestHopCause {
    Root,
    CrossSession { cause_hop: u32 },
    Return,
    Continuation,
}

pub fn next_request_hop(cause: RequestHopCause, own: u32) -> u32 {
    match cause {
        RequestHopCause::Root => 0,
        RequestHopCause::CrossSession { cause_hop } => own.max(cause_hop.saturating_add(1)),
        RequestHopCause::Return | RequestHopCause::Continuation => own,
    }
}

pub fn request_hop_within_bound(max_request_hop: Option<u32>, hop: u32) -> bool {
    max_request_hop.is_none_or(|bound| hop <= bound)
}

/// Lean `CausalHop` observation of a Node's `max_request_hop`: only a Node the
/// reader can see supplies a cap, so a hidden or absent Node is unavailable
/// rather than unset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestHopCapObservation {
    Unavailable,
    Visible(Option<u32>),
}

impl RequestHopCapObservation {
    /// The cap a reader observed: `None` when it could not read the Node.
    pub fn of(observed: Option<Option<u32>>) -> Self {
        observed.map_or(Self::Unavailable, Self::Visible)
    }

    /// Lean `CausalHop.admitObservedHop`: no decision without a visible Node.
    pub fn admit(self, hop: u32) -> Option<bool> {
        match self {
            Self::Unavailable => None,
            Self::Visible(cap) => Some(request_hop_within_bound(cap, hop)),
        }
    }

    /// Lean `CausalHop.observedHopBound`.
    pub fn bound(self) -> Option<u32> {
        match self {
            Self::Unavailable => Some(0),
            Self::Visible(cap) => cap,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SessionMessageCause {
    pub caller_node_did: String,
    pub caller_agent_did: String,
    pub caller_session_id: String,
    pub caller_requester_did: Option<String>,
    pub caller_request_id: String,
    pub caller_request_doc_id: String,
    pub caller_hop: u32,
    pub tool_call_id: String,
    pub tool_call_doc_id: String,
    pub correlation: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SessionMessageTarget {
    pub node_did: String,
    pub agent_did: String,
    pub session_id: String,
}

#[cfg(all(test, feature = "typescript"))]
mod tests {
    use super::RequestLifecycleState;
    use ts_rs::TS;

    #[test]
    fn typescript_union_matches_wire_tokens() {
        let expected = RequestLifecycleState::ALL
            .iter()
            .map(|state| format!("\"{}\"", state.as_str()))
            .collect::<Vec<_>>()
            .join(" | ");
        assert_eq!(RequestLifecycleState::inline(), expected);
    }
}
