use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LeanRetrievalMessage {
    pub(crate) sequence: u32,
    pub(crate) settled: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LeanSessionIndex {
    pub(crate) indexed: Vec<u32>,
    pub(crate) watermark: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LeanRetrievalIndexCase {
    pub(crate) name: String,
    pub(crate) messages: Vec<LeanRetrievalMessage>,
    pub(crate) state: LeanSessionIndex,
    pub(crate) next: LeanSessionIndex,
    pub(crate) index: Vec<u32>,
    pub(crate) delete: Vec<u32>,
    pub(crate) pending_after: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LeanRetrievalCandidate {
    #[serde(rename = "ref")]
    pub(crate) reference: u32,
    pub(crate) score: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LeanRetrievalHitCase {
    pub(crate) name: String,
    pub(crate) candidates: Vec<LeanRetrievalCandidate>,
    pub(crate) authorized: Vec<u32>,
    pub(crate) limit: usize,
    pub(crate) hits: Vec<LeanRetrievalCandidate>,
}

pub(crate) fn lean_retrieval_index_cases() -> &'static [LeanRetrievalIndexCase] {
    &super::lean_contract_snapshot().retrieval_index_cases
}

pub(crate) fn lean_retrieval_hit_cases() -> &'static [LeanRetrievalHitCase] {
    &super::lean_contract_snapshot().retrieval_hit_cases
}
