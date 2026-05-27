use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanCodexShimProjectionCase {
    pub(crate) witness: String,
    pub(crate) lean_theorems: Vec<String>,
    pub(crate) request_state: String,
    pub(crate) response_status: Option<String>,
    pub(crate) local_interrupt_acked: bool,
    pub(crate) projected_phase: String,
    pub(crate) terminal: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanCodexShimSteeringCase {
    pub(crate) witness: String,
    pub(crate) lean_theorems: Vec<String>,
    pub(crate) active_turn_id: String,
    pub(crate) expected_turn_id: String,
    pub(crate) active_request_id: String,
    pub(crate) emits_turn_started: bool,
    pub(crate) emits_turn_completed: bool,
    pub(crate) preserves_active_turn: bool,
    pub(crate) clears_active_turn: bool,
    pub(crate) terminal_status: Option<String>,
    pub(crate) committed_user_message_delta: usize,
    pub(crate) queue_source: Option<String>,
    pub(crate) queue_policy: Option<String>,
    pub(crate) queued_after_request_id: Option<String>,
    pub(crate) forwards_request_interrupt: bool,
    pub(crate) requires_request_transition_before_ack: bool,
    pub(crate) request_transition: Option<String>,
    pub(crate) request_from: Option<String>,
    pub(crate) request_to: Option<String>,
}
