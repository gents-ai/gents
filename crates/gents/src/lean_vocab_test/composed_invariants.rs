use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanComposedInvariantWitness {
    pub(crate) theorem_name: String,
    pub(crate) witness_kind: String,
    pub(crate) scenario: String,
    pub(crate) rust_path: String,
    pub(crate) trace_step_count: usize,
    pub(crate) transition_path: Vec<String>,
    pub(crate) pre_request_state: String,
    pub(crate) pre_request_admission: String,
    pub(crate) tool_pre_state: String,
    pub(crate) tool_post_state: String,
    pub(crate) request_id: usize,
    pub(crate) tool_request_id: usize,
    pub(crate) tool_call_id: usize,
    pub(crate) request_deadline: usize,
    pub(crate) request_current_time: usize,
    pub(crate) tool_deadline: usize,
    pub(crate) tool_current_time: usize,
    pub(crate) deadline_exceeded: bool,
    pub(crate) well_formed_source: String,
    pub(crate) pre_tool_persisted: bool,
    pub(crate) cancel_cause: Option<String>,
}
