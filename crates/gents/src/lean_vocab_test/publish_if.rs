//! Deserialization types for the Lean guarded-publication rows emitted by
//! `Proofs/ApplyReconcile/ContractCases.lean` from the `publishIf` model in
//! `Proofs/ApplyReconcile/Publication.lean`. Field names and types mirror the
//! emitted JSON exactly and decode strictly: contract drift must fail loudly
//! here instead of being masked by serde defaults.

use serde::Deserialize;

use super::{LeanApplyDesiredRow, LeanApplyDocRef};

/// One expectation row of a guarded publication. `content: None` means the
/// document must be absent.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanPublishIfExpectation {
    #[serde(rename = "ref")]
    pub(crate) target: LeanApplyDocRef,
    pub(crate) content: Option<String>,
}

/// `Publication.lean` `publishIf`: applied iff every expectation equals the
/// prior desired fields; otherwise the prior state is returned unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanPublishIfCase {
    pub(crate) name: String,
    pub(crate) expected: Vec<LeanPublishIfExpectation>,
    pub(crate) pre_desired: Vec<LeanApplyDesiredRow>,
    pub(crate) candidate: Vec<LeanApplyDesiredRow>,
    pub(crate) applied: bool,
    pub(crate) expected_after_desired: Vec<LeanApplyDesiredRow>,
}
