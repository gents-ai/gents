use serde::Deserialize;

#[derive(Debug, Deserialize, Clone)]
pub struct LeanDurableReductionCase {
    pub name: String,
    pub request_doc_id: u64,
    pub turn_index: usize,
    pub ordinal: usize,
    pub checkpoint: u64,
    pub claim_commit: u64,
    pub prior_checkpoint: Option<u64>,
    pub prior_claim_commit: Option<u64>,
    pub pair_closed: bool,
    pub inference_cites: bool,
    pub inference_supported: bool,
    pub title_cites: bool,
    pub outcome: String,
    pub durable_after: bool,
    pub send_permitted: bool,
    pub consumed: bool,
}
