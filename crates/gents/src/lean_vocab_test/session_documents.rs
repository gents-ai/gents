//! Lean transport projections use interned IDs and logical time. Runtime
//! consumers map them to canonical documents; these are not storage models.
use super::*;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanSessionDocumentCases {
    pub(crate) document_reference_encoding: String,
    pub(crate) selection: Vec<serde_json::Value>,
    pub(crate) projection: Vec<serde_json::Value>,
    pub(crate) retry: Vec<serde_json::Value>,
    pub(crate) fork: Vec<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanBudgetRehydrationCase {
    pub(crate) name: String,
    pub(crate) request_doc_id: String,
    pub(crate) pinned_limit: Option<u64>,
    pub(crate) rows: Vec<LeanDurableUsageRow>,
    pub(crate) ledger: Option<LeanRehydratedBudget>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanDurableUsageRow {
    pub(crate) request_doc_id: String,
    pub(crate) kind: String,
    pub(crate) prompt_tokens: u64,
    pub(crate) completion_tokens: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanRehydratedBudget {
    pub(crate) limit: u64,
    pub(crate) used: u64,
}
