use gents_protocol::request_lifecycle::RequestLifecycleState;
use serde::Deserialize;

#[derive(Debug, Clone)]
pub(super) struct DedupPlan {
    pub(super) is_earliest: bool,
    pub(super) blocking_request_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RequestStatusTransition {
    Updated,
    AlreadyTarget,
    ConflictingTerminal(RequestViewRow),
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct DedupRow {
    #[serde(rename = "_docID")]
    pub(super) doc_id: String,
    pub(super) request_id: String,
    #[serde(default)]
    pub(super) lifecycle_state: Option<RequestLifecycleState>,
    #[allow(dead_code)]
    pub(super) created_at: String,
}

impl DedupRow {
    pub(super) fn is_pending(&self) -> bool {
        self.lifecycle_state == Some(RequestLifecycleState::Pending)
    }
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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(super) struct RequestViewRow {
    #[serde(default)]
    pub(super) lifecycle_state: Option<RequestLifecycleState>,
    #[allow(dead_code)]
    pub(super) backend_id: Option<String>,
    #[allow(dead_code)]
    pub(super) execution_origin: Option<String>,
}
