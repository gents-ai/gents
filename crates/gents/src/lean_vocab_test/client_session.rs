use super::*;

#[derive(Debug, Deserialize)]
pub(crate) struct LeanClientShellCase {
    pub(crate) name: String,
    pub(crate) property: String,
    pub(crate) input: String,
    pub(crate) pre_selection_session: Option<usize>,
    pub(crate) post_selection_session: Option<usize>,
    pub(crate) pre_workflow_kind: String,
    pub(crate) pre_workflow_request: Option<usize>,
    pub(crate) post_workflow_kind: String,
    pub(crate) post_workflow_request: Option<usize>,
    pub(crate) selection_preserved: bool,
    pub(crate) workflow_advanced: bool,
    pub(crate) transport_noop: bool,
    pub(crate) can_submit_before: bool,
    pub(crate) can_submit_after: bool,
    pub(crate) send_decision: String,
    pub(crate) send_blocked_reason: Option<String>,
    pub(crate) frontend_expected_workflow_kind: String,
    pub(crate) frontend_expected_send_status: String,
    pub(crate) frontend_expected_send_blocked_reason: Option<String>,
    pub(crate) frontend_expected_active_request_id: Option<usize>,
    pub(crate) desktop_selected_session_id: Option<usize>,
    pub(crate) desktop_snapshot_present: bool,
    pub(crate) desktop_preferred_request_id: Option<usize>,
    pub(crate) desktop_observed_request_id: Option<usize>,
    pub(crate) desktop_observed_turn_state: Option<String>,
    pub(crate) desktop_queued_request_ids: Vec<usize>,
    pub(crate) desktop_folded_request_ids: Vec<usize>,
    pub(crate) desktop_rows: Vec<LeanSessionTurnRow>,
    pub(crate) desktop_expected_latest_request_id: Option<usize>,
    pub(crate) desktop_expected_turn_state: Option<String>,
    pub(crate) desktop_expect_pending_turn: Option<bool>,
}

/// `ClientShell.SessionTurn.Row`: one request row of a session.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanSessionTurnRow {
    pub(crate) doc: usize,
    pub(crate) request: usize,
    pub(crate) requester: usize,
    pub(crate) state: String,
    pub(crate) folded_into: Option<usize>,
    pub(crate) queued_after: Option<usize>,
    pub(crate) retry_parent: Option<usize>,
}

/// `SessionTurnCases`: rows in arrival order, the last being newest, and the
/// turn, queue and folded requests `SessionTurn` computes from them.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanSessionTurnCase {
    pub(crate) name: String,
    pub(crate) rows: Vec<LeanSessionTurnRow>,
    pub(crate) expected_turn_doc: usize,
    pub(crate) expected_queued_docs: Vec<usize>,
    pub(crate) expected_folded_requests: Vec<usize>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LeanSessionRecoveryCase {
    pub(crate) name: String,
    pub(crate) action: String,
    pub(crate) legal: bool,
    pub(crate) pre_latest_state: String,
    pub(crate) pre_failed_state: String,
    pub(crate) post_latest_state: String,
    pub(crate) post_failed_state: String,
    pub(crate) post_new_state: String,
    pub(crate) pre_latest_admission: String,
    pub(crate) post_latest_admission: String,
    pub(crate) pre_failed_admission: String,
    pub(crate) post_failed_admission: String,
    pub(crate) post_new_admission: String,
    pub(crate) pre_origin: String,
    pub(crate) post_new_origin: String,
    pub(crate) failed_id: usize,
    pub(crate) new_id: usize,
    pub(crate) pre_latest_id: usize,
    pub(crate) post_latest_id: usize,
    pub(crate) pre_session_id: usize,
    pub(crate) post_session_id: usize,
    pub(crate) pre_agent_id: usize,
    pub(crate) post_agent_id: usize,
    pub(crate) pre_request_count: usize,
    pub(crate) post_request_count: usize,
    pub(crate) pre_retry_count: usize,
    pub(crate) post_retry_count: usize,
    pub(crate) max_retries: usize,
    pub(crate) pre_deadline_exceeded: bool,
    pub(crate) post_deadline_exceeded: bool,
    pub(crate) pre_failed_is_latest: bool,
    pub(crate) post_failed_is_latest: bool,
    pub(crate) post_new_is_latest: bool,
    pub(crate) pre_request_ids: Vec<usize>,
    pub(crate) pre_failed_exists: bool,
    pub(crate) pre_latest_exists: bool,
    pub(crate) pre_new_request_exists: bool,
    pub(crate) old_request_retained: bool,
    pub(crate) new_request_inserted: bool,
    pub(crate) origin_preserved: bool,
}
