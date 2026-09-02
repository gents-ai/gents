use serde::Deserialize;

/// One shared compaction decision/outcome row emitted by
/// `Compaction.ReductionEngine`.
#[derive(Debug, Deserialize, Clone)]
pub(crate) struct LeanReductionEngineCase {
    pub(crate) name: String,
    pub(crate) source: Vec<usize>,
    pub(crate) input_tokens: usize,
    pub(crate) effective_input_budget: usize,
    pub(crate) can_fit: bool,
    pub(crate) prefix_length: usize,
    pub(crate) checkpoint: usize,
    pub(crate) threshold_decision: String,
    pub(crate) decision: String,
    pub(crate) outcome: String,
    pub(crate) not_needed_messages: Vec<usize>,
    pub(crate) compacted_prefix: Vec<usize>,
    pub(crate) retained_suffix: Vec<usize>,
    pub(crate) outcome_checkpoint: Option<usize>,
    pub(crate) exact: bool,
}
