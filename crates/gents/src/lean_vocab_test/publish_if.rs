//! Deserialization types for the Lean guarded-publication rows emitted from
//! `Proofs/ApplyReconcile`. Contract drift must fail loudly here instead of
//! being masked by serde defaults.

use serde::Deserialize;

use super::{LeanApplyDesiredRow, LeanApplyDocRef};

/// `content: None` means the document must be absent.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanPublishIfExpectation {
    #[serde(rename = "ref")]
    pub(crate) target: LeanApplyDocRef,
    pub(crate) content: Option<String>,
}

/// Applied iff every expectation equals the prior desired fields; the
/// `expected` rows project `content` only because the Rust digest domain at
/// this refinement boundary is the document content.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanPublishIfCase {
    pub(crate) name: String,
    pub(crate) expected: Vec<LeanPublishIfExpectation>,
    pub(crate) pre_desired: Vec<LeanApplyDesiredRow>,
    pub(crate) candidate: Vec<LeanApplyDesiredRow>,
    pub(crate) applied: bool,
    pub(crate) expected_after_desired: Vec<LeanApplyDesiredRow>,
}
