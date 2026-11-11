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
    #[cfg(any(test, feature = "test-fixtures"))]
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
    use super::{CancelCause, ToolCallState};

    #[test]
    fn tool_call_states_are_the_lean_tool_call_states() {
        gents_lean_vocab::families::tool_execution_tool_call_state_vocabulary::replay(
            &ToolCallState::ALL,
        );
    }

    #[test]
    fn cancel_causes_are_the_lean_cancel_causes() {
        gents_lean_vocab::families::tool_execution_cancel_cause_vocabulary::replay(
            CancelCause::ALL,
        );
    }
}
