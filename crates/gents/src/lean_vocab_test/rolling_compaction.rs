use serde::Deserialize;

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct LeanRollingCompactionCase {
    pub(crate) name: String,
    pub(crate) target_messages: usize,
    pub(crate) chunk_messages: Vec<usize>,
    pub(crate) chunk_pair_closed: Vec<bool>,
    pub(crate) chunk_can_dispatch: Vec<bool>,
    pub(crate) checkpoint_covered: usize,
    pub(crate) plan_valid: bool,
    pub(crate) prior_payload: Option<usize>,
    pub(crate) next_chunk: Vec<usize>,
    pub(crate) step_input: Vec<usize>,
}
