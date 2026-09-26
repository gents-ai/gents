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
    pub(crate) claim_commit: u64,
    pub(crate) prior_checkpoint: Option<u64>,
    pub(crate) prior_claim_commit: Option<u64>,
    pub(crate) pair_closed: bool,
    pub(crate) inference_cites: bool,
    pub(crate) inference_supported: bool,
    pub(crate) title_cites: bool,
    pub(crate) outcome: String,
    pub(crate) durable_after: bool,
    pub(crate) send_permitted: bool,
    pub(crate) consumed: bool,
}
