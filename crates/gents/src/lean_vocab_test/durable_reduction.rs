use serde::Deserialize;

/// One create-and-compare delivery evaluated by
/// `Compaction.DurableReduction.persist` in Lean.
#[derive(Debug, Deserialize, Clone)]
pub(crate) struct LeanDurableReductionCase {
    pub(crate) name: String,
    pub(crate) request_doc_id: u64,
    pub(crate) turn_index: usize,
    pub(crate) ordinal: usize,
    pub(crate) checkpoint: u64,
    pub(crate) prior_checkpoint: Option<u64>,
    pub(crate) pair_closed: bool,
    pub(crate) outcome: String,
    pub(crate) durable_after: bool,
    pub(crate) send_permitted: bool,
}
