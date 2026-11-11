use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanComposedInvariantWitness {
    pub theorem_name: String,
    pub witness_kind: String,
    pub scenario: String,
    pub rust_path: String,
    pub trace_step_count: usize,
    pub transition_path: Vec<String>,
    pub pre_request_state: String,
    pub pre_request_admission: String,
    pub tool_pre_state: String,
    pub tool_post_state: String,
    pub request_id: usize,
    pub tool_request_id: usize,
    pub tool_call_id: usize,
    pub request_deadline: usize,
    pub request_current_time: usize,
    pub tool_deadline: usize,
    pub tool_current_time: usize,
    pub deadline_exceeded: bool,
    pub well_formed_source: String,
    pub pre_tool_persisted: bool,
    pub cancel_cause: Option<String>,
}
