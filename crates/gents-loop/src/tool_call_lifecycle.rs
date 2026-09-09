//! The loop's slice of tool-call lifecycle vocabulary.
//!
//! `gents::tool_call_lifecycle` owns the full persisted state machine
//! (`ToolCallLifecycle`, DefraDB-backed); only the failure classification and
//! the typed dispatch outcome are shared with the loop, so they live here and
//! are re-exported natively.

use serde::{Deserialize, Serialize};

pub mod runtime;
pub use runtime::ToolOutcome;

/// Retry-relevant classification of why a tool call failed. Mirrors the
/// persisted `AgentToolCall.failure_class` vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FailureClass {
    ApprovalDenied,
    ArgumentInvalid,
    ServiceUnavailable,
    Transport,
    ToolReturnedError,
    PolicyDenied,
    External,
}

impl FailureClass {
    pub const ALL: [Self; 7] = [
        Self::ApprovalDenied,
        Self::ArgumentInvalid,
        Self::ServiceUnavailable,
        Self::Transport,
        Self::ToolReturnedError,
        Self::PolicyDenied,
        Self::External,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ApprovalDenied => "approvalDenied",
            Self::ArgumentInvalid => "argumentInvalid",
            Self::ServiceUnavailable => "serviceUnavailable",
            Self::Transport => "transport",
            Self::ToolReturnedError => "toolReturnedError",
            Self::PolicyDenied => "policyDenied",
            Self::External => "external",
        }
    }

    pub fn from_persisted(value: &str) -> Option<Self> {
        match value {
            "approvalDenied" => Some(Self::ApprovalDenied),
            "argumentInvalid" => Some(Self::ArgumentInvalid),
            "serviceUnavailable" => Some(Self::ServiceUnavailable),
            "transport" => Some(Self::Transport),
            "toolReturnedError" => Some(Self::ToolReturnedError),
            "policyDenied" => Some(Self::PolicyDenied),
            "external" => Some(Self::External),
            _ => None,
        }
    }
}
