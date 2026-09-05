//! `RequestLifecycleState`: the single Rust owner of `AgentRequest` state.
//!
//! One column (`lifecycle_state`), one enum. Source of truth: the Lean
//! `RequestState` model in `crates/gents/proofs/`.

use std::error::Error;
use std::fmt::{Display, Formatter};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RequestLifecycleState {
    WorkspaceBindingPending,
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
    pub const ALL: [Self; 10] = [
        Self::WorkspaceBindingPending,
        Self::Pending,
        Self::Claimed,
        Self::Processing,
        Self::InputRequired,
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
            Self::InputRequired => "inputRequired",
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

/// Strict: an unknown value is a hard deserialization error naming the
/// offending value, not a silent `None`. Wire code that wants leniency
/// should deserialize into `Option<RequestLifecycleState>` and rely on the
/// field being absent/null, not on this succeeding for garbage strings.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_str_round_trips_for_every_state() {
        for state in RequestLifecycleState::ALL {
            assert_eq!(RequestLifecycleState::parse(state.as_str()), Ok(state));
        }
    }

    #[test]
    fn terminal_partition_matches_lean() {
        use RequestLifecycleState::*;
        let terminal: Vec<_> = RequestLifecycleState::ALL
            .iter()
            .copied()
            .filter(|s| s.is_terminal())
            .collect();
        assert_eq!(
            terminal,
            vec![Completed, Failed, Superseded, Dead, Interrupted]
        );
        assert!(!WorkspaceBindingPending.is_terminal());
        assert!(!WorkspaceBindingPending.is_claimable());
        assert!(Pending.is_claimable());
    }

    #[test]
    fn legacy_status_vocabulary_is_rejected() {
        for legacy in [
            "complete",
            "error",
            "streaming",
            "workspace_binding_pending",
            "timedOut",
            "cancelled",
        ] {
            assert!(RequestLifecycleState::parse(legacy).is_err(), "{legacy}");
        }
        assert!(!RequestLifecycleState::is_terminal_str(Some("error")));
        assert!(RequestLifecycleState::is_terminal_str(Some("failed")));
        assert!(!RequestLifecycleState::is_terminal_str(None));
    }

    #[test]
    fn serde_round_trips_for_every_state() {
        for state in RequestLifecycleState::ALL {
            let json = serde_json::to_string(&state).expect("serialize");
            assert_eq!(json, format!("\"{}\"", state.as_str()));
            let round: RequestLifecycleState = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(round, state);
        }
    }

    #[test]
    fn serde_rejects_unknown_value_naming_it() {
        let err = serde_json::from_str::<RequestLifecycleState>(r#""bogus""#)
            .expect_err("unknown value must be rejected");
        assert!(
            err.to_string().contains("bogus"),
            "error should name the offending value: {err}"
        );
    }

    #[test]
    fn graphql_lists_are_quoted_arrays() {
        assert_eq!(
            RequestLifecycleState::active_runtime_graphql_list(),
            r#"["pending", "claimed", "processing"]"#
        );
        assert_eq!(
            RequestLifecycleState::terminal_graphql_list(),
            r#"["completed", "failed", "superseded", "dead", "interrupted"]"#
        );
    }
}
