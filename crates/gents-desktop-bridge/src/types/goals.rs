use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct SessionGoalView {
    pub node_did: String,
    pub session_id: String,
    pub session_doc_id: String,
    pub observed_at: String,
    pub goal: Option<GoalControlView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub waiting_question_ids: Option<Vec<String>>,
}

/// Decimal strings preserve the runtime's signed 64-bit budgets and counters
/// through JavaScript display and form editing.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct GoalControlView {
    pub goal_id: String,
    pub revision: gents_protocol::goal_control::GoalRevision,
    pub objective: String,
    pub status: String,
    pub token_budget: Option<String>,
    pub tokens_used: String,
    pub active_time_seconds: String,
    pub continuation_sequence: String,
    pub last_blocked_reason: Option<String>,
    pub last_failure: Option<String>,
    pub completion_evidence: Option<String>,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[serde(deny_unknown_fields)]
pub enum GoalControlAction {
    Create {
        objective: String,
        #[serde(rename = "tokenBudget")]
        token_budget: Option<String>,
    },
    Edit {
        objective: Option<String>,
        #[serde(rename = "tokenBudget")]
        token_budget: Option<String>,
    },
    Pause {},
    Resume {},
    Stop {},
}

#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GoalControlRequest {
    #[serde(default)]
    #[ts(optional = nullable)]
    pub client_did: Option<String>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub command_id: Option<String>,
    pub node_did: String,
    pub session_id: Option<String>,
    pub session_doc_id: Option<String>,
    pub expected: Option<gents_protocol::goal_control::GoalRevision>,
    pub agent_id: Option<String>,
    pub action: GoalControlAction,
    pub cwd: Option<String>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub captured_context: Option<Vec<gents_protocol::request_input::CapturedContext>>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct GoalControlResult {
    pub goal_id: String,
    pub session_id: String,
    pub request_id: Option<String>,
    /// Present only after the runtime confirms the signed opening assignment.
    /// A resumed request is a continuation, even when it has a new request ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub started_goal_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub submission_receipt: Option<GoalSubmissionReceipt>,
}

#[derive(Debug, Clone, Deserialize, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GoalSubmissionQuery {
    pub client_did: String,
    pub node_did: String,
    pub command_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GoalSubmissionReceipt {
    pub client_did: String,
    pub node_did: String,
    pub session_id: String,
    pub reference: gents_desktop_core::client::GoalSubmissionReference,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum GoalSubmissionOutcome {
    Applied { result: Box<GoalControlResult> },
    Rejected { reason: String },
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum GoalSubmissionRecovery {
    Unconfirmed {
        #[serde(rename = "nodeDid")]
        node_did: String,
        #[serde(rename = "commandId")]
        command_id: String,
    },
    Prepared {
        receipt: GoalSubmissionReceipt,
        outcome: Option<GoalSubmissionOutcome>,
        acknowledged: bool,
    },
}
