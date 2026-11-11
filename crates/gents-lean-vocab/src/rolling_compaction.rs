use serde::Deserialize;

#[derive(Debug, Deserialize, Clone)]
pub struct LeanRollingCompactionCase {
    pub name: String,
    pub target_messages: usize,
    pub chunk_messages: Vec<usize>,
    pub chunk_pair_closed: Vec<bool>,
    pub chunk_can_dispatch: Vec<bool>,
    pub checkpoint_covered: usize,
    pub plan_valid: bool,
    pub prior_payload: Option<usize>,
    pub next_chunk: Vec<usize>,
    pub step_input: Vec<usize>,
}
