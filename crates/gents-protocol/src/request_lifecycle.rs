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
