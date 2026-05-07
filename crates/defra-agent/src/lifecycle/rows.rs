use serde::Deserialize;

#[derive(Debug, Clone)]
pub(super) struct DedupPlan {
    pub(super) is_earliest: bool,
    pub(super) blocking_request_id: Option<String>,
    pub(super) duplicates_to_suppress: Vec<DedupRow>,
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
    pub(super) status: String,
    #[allow(dead_code)]
    pub(super) created_at: String,
}

#[derive(Deserialize)]
pub(super) struct StatusRow {
    pub(super) status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(super) struct RequestViewRow {
    pub(super) status: String,
    pub(super) lifecycle_state: Option<String>,
    #[allow(dead_code)]
    pub(super) backend_id: Option<String>,
    #[allow(dead_code)]
    pub(super) execution_origin: Option<String>,
}
