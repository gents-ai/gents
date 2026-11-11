use serde::Deserialize;

#[derive(Debug, Deserialize, Clone)]
pub struct LeanReductionEngineCase {
    pub name: String,
    pub source: Vec<usize>,
    pub input_tokens: usize,
    pub effective_input_budget: usize,
    pub can_fit: bool,
    pub prefix_length: usize,
    pub checkpoint: usize,
    pub threshold_decision: String,
    pub decision: String,
    pub outcome: String,
    pub not_needed_messages: Vec<usize>,
    pub compacted_prefix: Vec<usize>,
    pub retained_suffix: Vec<usize>,
    pub outcome_checkpoint: Option<usize>,
    pub exact: bool,
}
