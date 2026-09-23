//! Deserialization types for the Lean eval outcome projection rows
//! (`Eval.classify` and `Eval.subjectCausable` in `Proofs/Eval.lean`, emitted
//! as `eval_outcome_cases` by `Proofs/Conformance/Eval.lean`). Field names and
//! types mirror the emitted JSON exactly and decode strictly: contract drift
//! must fail loudly here instead of being masked by serde defaults.

use serde::Deserialize;

/// One row of `Proofs/Eval.lean` `classify` and `subjectCausable`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanEvalOutcomeCase {
    pub(crate) kind: String,
    pub(crate) provider_reason: Option<String>,
    pub(crate) class: String,
    pub(crate) subject_causable: bool,
}
