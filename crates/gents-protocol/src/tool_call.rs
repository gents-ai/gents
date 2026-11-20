#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AwaitMode {
    #[default]
    Foreground,
    Background,
}

impl AwaitMode {
    pub fn as_str(self) -> &'static str {
        match self {
            AwaitMode::Foreground => "foreground",
            AwaitMode::Background => "background",
        }
    }

    pub fn from_persisted(s: &str) -> Option<Self> {
        match s {
            "foreground" => Some(AwaitMode::Foreground),
            "background" => Some(AwaitMode::Background),
            _ => None,
        }
    }

    pub const ALL: &'static [AwaitMode] = &[AwaitMode::Foreground, AwaitMode::Background];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolCallState {
    Pending,
    Running,
    Completed,
    Failed,
    TimedOut,
    Cancelled,
}

impl ToolCallState {
    pub const ALL: [Self; 6] = [
        Self::Pending,
        Self::Running,
        Self::Completed,
        Self::Failed,
        Self::TimedOut,
        Self::Cancelled,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::TimedOut => "timedOut",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn from_persisted(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "running" => Some(Self::Running),
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            "timedOut" => Some(Self::TimedOut),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }

    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::TimedOut | Self::Cancelled
        )
    }

    pub fn terminal() -> impl Iterator<Item = Self> {
        Self::ALL.into_iter().filter(|state| state.is_terminal())
    }

    pub fn nonterminal() -> impl Iterator<Item = Self> {
        Self::ALL.into_iter().filter(|state| !state.is_terminal())
    }

    pub fn graphql_list<I>(states: I) -> String
    where
        I: IntoIterator<Item = Self>,
    {
        let tokens: Vec<String> = states
            .into_iter()
            .map(|state| state.as_str().to_owned())
            .collect();
        crate::graphql::graphql_string_list_literal(&tokens)
    }

    pub fn terminal_graphql_list() -> String {
        Self::graphql_list(Self::terminal())
    }

    pub fn nonterminal_graphql_list() -> String {
        Self::graphql_list(Self::nonterminal())
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CancelCause {
    Interrupted,
    Deadline,
    UserCancelled,
}

impl CancelCause {
    pub fn as_str(self) -> &'static str {
        match self {
            CancelCause::Interrupted => "interrupted",
            CancelCause::Deadline => "deadline",
            CancelCause::UserCancelled => "userCancelled",
        }
    }

    pub fn from_persisted(s: &str) -> Option<Self> {
        match s {
            "interrupted" => Some(CancelCause::Interrupted),
            "deadline" => Some(CancelCause::Deadline),
            "userCancelled" => Some(CancelCause::UserCancelled),
            _ => None,
        }
    }

    pub const ALL: &'static [CancelCause] = &[
        CancelCause::Interrupted,
        CancelCause::Deadline,
        CancelCause::UserCancelled,
    ];
}

#[cfg(test)]
mod conformance {
    use super::{AwaitMode, CancelCause, ToolCallState};

    #[test]
    fn await_modes_are_the_lean_await_modes() {
        gents_lean_vocab::families::tool_execution_await_mode_vocabulary::replay(AwaitMode::ALL);
    }

    #[test]
    fn tool_call_states_are_the_lean_tool_call_states() {
        gents_lean_vocab::families::tool_execution_tool_call_state_vocabulary::replay(
            &ToolCallState::ALL,
        );
    }

    #[test]
    fn tool_call_terminal_partition_matches_the_lean_tool_call_states() {
        gents_lean_vocab::families::tool_execution_tool_call_state_terminal::replay::<ToolCallState>(
            |i| i.state.is_terminal(),
        );
    }

    #[test]
    fn cancel_causes_are_the_lean_cancel_causes() {
        gents_lean_vocab::families::tool_execution_cancel_cause_vocabulary::replay(
            CancelCause::ALL,
        );
    }
}
