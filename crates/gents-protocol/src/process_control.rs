use serde::{Deserialize, Serialize};

pub const PROCESS_CONTROL_PATH: &str = "/processes/control";

/// A displayed native invocation is addressed by both its physical row and
/// logical identity; neither a session identifier nor a process identifier
/// alone grants control.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct OperatorProcessControlRequest {
    pub node_did: String,
    pub session_id: String,
    pub tool_call_id: String,
    pub tool_call_doc_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum ProcessControlOutcome {
    Queued { intent_doc_id: String },
    Cancelled { live_execution_cancelled: bool },
    Lost,
    Unverified,
    AlreadyTerminal { state: String },
    NotBackground,
    NotFound,
}

/// Client-signed control scope. The transport authenticates the same author,
/// and the selected agent verifies this payload again before claiming it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ToolControlIntent {
    pub version: u8,
    pub intent_id: String,
    pub client_did: String,
    pub node_did: String,
    pub agent_did: String,
    pub session_id: String,
    pub session_doc_id: String,
    pub request_doc_id: String,
    pub requester_did: Option<String>,
    pub tool_call_id: String,
    pub tool_call_doc_id: String,
    pub action: String,
    pub signature: Vec<u8>,
}

impl ToolControlIntent {
    pub fn signing_payload(&self) -> Result<Vec<u8>, ToolControlIntentError> {
        let mut unsigned = self.clone();
        unsigned.signature.clear();
        let json = serde_json::to_string(&unsigned)?;
        Ok(crate::enrollment::canonical_domain_payload(
            "gents-tool-control-intent-v1",
            [json.as_str()],
        ))
    }

    pub fn validate_shape(&self) -> Result<(), ToolControlIntentError> {
        if self.version != 1
            || self.signature.len() != 64
            || [
                self.intent_id.as_str(),
                self.client_did.as_str(),
                self.node_did.as_str(),
                self.agent_did.as_str(),
                self.session_id.as_str(),
                self.session_doc_id.as_str(),
                self.request_doc_id.as_str(),
                self.tool_call_id.as_str(),
                self.tool_call_doc_id.as_str(),
            ]
            .iter()
            .any(|value| value.trim().is_empty())
            || !matches!(
                self.action.as_str(),
                "process_stop" | "job_stop" | "job_pause"
            )
        {
            return Err(ToolControlIntentError::Invalid);
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ToolControlIntentError {
    #[error("invalid signed tool control intent")]
    Invalid,
    #[error("cannot encode tool control intent")]
    Encoding(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ToolControlReceiptOutcome {
    Process(ProcessControlOutcome),
    Job(crate::job::JobControlOutcome),
    Rejected,
    Unconfirmed,
}

/// The selected agent authenticates the physical intent and invocation along
/// with its result. A detached copy is meaningful only after signature and
/// expected-scope validation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ToolControlReceipt {
    pub version: u8,
    pub intent_doc_id: String,
    pub client_did: String,
    pub agent_did: String,
    pub node_did: String,
    pub session_id: String,
    pub session_doc_id: String,
    pub request_doc_id: String,
    pub tool_call_id: String,
    pub tool_call_doc_id: String,
    pub action: String,
    pub outcome: ToolControlReceiptOutcome,
    pub signature: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub enum ToolControlReceiptError {
    #[error("invalid tool control receipt")]
    Invalid,
    #[error("cannot encode tool control receipt")]
    Encoding(#[from] serde_json::Error),
}

impl ToolControlReceipt {
    pub fn signing_payload(&self) -> Result<Vec<u8>, ToolControlReceiptError> {
        let mut unsigned = self.clone();
        unsigned.signature.clear();
        let json = serde_json::to_string(&unsigned)?;
        Ok(crate::enrollment::canonical_domain_payload(
            "gents-tool-control-receipt-v1",
            [json.as_str()],
        ))
    }

    pub fn validate_for(&self, expected: &Self) -> Result<(), ToolControlReceiptError> {
        let mut scope = self.clone();
        scope.outcome = expected.outcome.clone();
        scope.signature = expected.signature.clone();
        if self.version != 1 || self.signature.len() != 64 || scope != *expected {
            return Err(ToolControlReceiptError::Invalid);
        }
        match (&self.action[..], &self.outcome) {
            (
                "process_stop",
                ToolControlReceiptOutcome::Process(ProcessControlOutcome::Queued { .. }),
            )
            | (
                "job_stop" | "job_pause",
                ToolControlReceiptOutcome::Job(crate::job::JobControlOutcome::Queued { .. }),
            ) => Err(ToolControlReceiptError::Invalid),
            ("process_stop", ToolControlReceiptOutcome::Process(_))
            | ("job_stop" | "job_pause", ToolControlReceiptOutcome::Job(_))
            | (
                "process_stop" | "job_stop" | "job_pause",
                ToolControlReceiptOutcome::Rejected | ToolControlReceiptOutcome::Unconfirmed,
            ) => Ok(()),
            _ => Err(ToolControlReceiptError::Invalid),
        }
    }
}
