//! Tool-call lifecycle state machine.
//!
//! Mirrors `crates/defra-agent/src/lifecycle.rs` (`RequestLifecycle`) for tool
//! calls. Defines the persisted vocabulary, failure-class enum, and the
//! `ToolCallLifecycle` struct that owns every persistence write.
//!
//! Lifecycle is daemon-visible only; subprocess kill mechanics, output
//! streaming, and persistent processes are out of scope.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolCallState {
    Pending,
    Running,
    Completed,
    Failed,
    TimedOut,
    Cancelled,
}

impl ToolCallState {
    pub(crate) const ALL: [Self; 6] = [
        Self::Pending,
        Self::Running,
        Self::Completed,
        Self::Failed,
        Self::TimedOut,
        Self::Cancelled,
    ];

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::TimedOut => "timedOut",
            Self::Cancelled => "cancelled",
        }
    }

    pub(crate) fn from_persisted(value: &str) -> Option<Self> {
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

    pub(crate) const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::TimedOut | Self::Cancelled
        )
    }

    pub(crate) const fn is_cancellable(self) -> bool {
        matches!(self, Self::Pending | Self::Running)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FailureClass {
    ArgumentInvalid,
    ServiceUnavailable,
    Transport,
    ToolReturnedError,
    External,
}

impl FailureClass {
    pub const ALL: [Self; 5] = [
        Self::ArgumentInvalid,
        Self::ServiceUnavailable,
        Self::Transport,
        Self::ToolReturnedError,
        Self::External,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ArgumentInvalid => "argumentInvalid",
            Self::ServiceUnavailable => "serviceUnavailable",
            Self::Transport => "transport",
            Self::ToolReturnedError => "toolReturnedError",
            Self::External => "external",
        }
    }

    pub fn from_persisted(value: &str) -> Option<Self> {
        match value {
            "argumentInvalid" => Some(Self::ArgumentInvalid),
            "serviceUnavailable" => Some(Self::ServiceUnavailable),
            "transport" => Some(Self::Transport),
            "toolReturnedError" => Some(Self::ToolReturnedError),
            "external" => Some(Self::External),
            _ => None,
        }
    }
}

use std::sync::Arc;

use defra_node::EmbeddedNode;

pub(crate) mod query;
mod transition;

pub use transition::IllegalToolCallTransition;

/// State machine struct for an individual tool call. Mirrors `RequestLifecycle`
/// from `lifecycle.rs:189-204`. Owns every persistence write for a single
/// AgentToolCall row.
pub struct ToolCallLifecycle {
    node: Arc<EmbeddedNode>,
    session_id: String,
    tool_call_id: String,
    message_sequence: u32,
    tool_name: String,
    args: String,
    doc_id: Option<String>,
    state: ToolCallState,
    started_at: Option<chrono::DateTime<chrono::Utc>>,
    failure_class: Option<FailureClass>,
}

impl ToolCallLifecycle {
    /// Construct a new lifecycle. Does NOT persist; the first transition
    /// method (`start_running`) creates the DefraDB row.
    pub fn new(
        node: Arc<EmbeddedNode>,
        session_id: String,
        tool_call_id: String,
        message_sequence: u32,
        tool_name: String,
        args: String,
    ) -> Self {
        Self {
            node,
            session_id,
            tool_call_id,
            message_sequence,
            tool_name,
            args,
            doc_id: None,
            state: ToolCallState::Pending,
            started_at: None,
            failure_class: None,
        }
    }

    /// Test-only accessor for the current in-memory state.
    #[cfg(test)]
    pub(crate) fn state_for_test(&self) -> ToolCallState {
        self.state
    }

    pub(crate) fn set_doc_id(&mut self, doc_id: Option<String>) {
        self.doc_id = doc_id;
    }

    pub(crate) fn set_state(&mut self, state: ToolCallState) {
        self.state = state;
    }

    pub(crate) fn set_started_at(&mut self, t: Option<chrono::DateTime<chrono::Utc>>) {
        self.started_at = t;
    }

    pub(crate) fn set_failure_class(&mut self, fc: Option<FailureClass>) {
        self.failure_class = fc;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_persisted_vocabulary() {
        for state in ToolCallState::ALL {
            assert_eq!(ToolCallState::from_persisted(state.as_str()), Some(state));
        }
        assert_eq!(ToolCallState::from_persisted("called"), None);
        assert_eq!(ToolCallState::from_persisted("unknown"), None);
    }

    #[test]
    fn cancellable_iff_non_terminal() {
        for state in ToolCallState::ALL {
            assert_eq!(state.is_cancellable(), !state.is_terminal());
        }
    }

    #[test]
    fn all_lists_six_states() {
        assert_eq!(ToolCallState::ALL.len(), 6);
    }

    #[test]
    fn failure_class_round_trip_persisted_vocabulary() {
        for fc in FailureClass::ALL {
            assert_eq!(FailureClass::from_persisted(fc.as_str()), Some(fc));
        }
        assert_eq!(FailureClass::from_persisted("unknown"), None);
    }

    #[test]
    fn failure_class_all_lists_five_variants() {
        assert_eq!(FailureClass::ALL.len(), 5);
    }

    #[test]
    fn lifecycle_new_signature_compiles() {
        // Compile-only sanity test: behavior verified in Bucket 3 integration tests.
        let _: fn(
            std::sync::Arc<defra_node::EmbeddedNode>,
            String,
            String,
            u32,
            String,
            String,
        ) -> ToolCallLifecycle = ToolCallLifecycle::new;
    }
}
