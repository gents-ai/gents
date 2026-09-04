use gents_protocol::row::AgentRequestRow;
use serde::Deserialize;

#[derive(Debug, Clone)]
pub(super) struct DedupPlan {
    pub(super) is_earliest: bool,
    pub(super) blocking_request_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum RequestStatusTransition {
    Updated,
    AlreadyTarget,
    ConflictingTerminal(AgentRequestRow),
}

#[derive(Deserialize)]
pub(super) struct StatusRow {
    pub(super) status: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct ResponseTerminalRow {
    pub(super) status: String,
    #[serde(default)]
    pub(super) error_message: Option<String>,
    #[serde(default)]
    pub(super) interrupted_at: Option<String>,
}
