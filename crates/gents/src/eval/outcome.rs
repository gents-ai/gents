//! The eval outcome vocabulary and its projection onto evidence classes.
//!
//! `Proofs/Eval.lean` owns the rule; this module is its transcription, and
//! `conformance::rust_eval_outcome_vocabulary_and_projection_match_lean` drives
//! every generated case through [`classify`] and [`subject_causable`]. The
//! governing rule: anything the subject under evaluation could cause counts
//! against it; only what it cannot cause is excluded.

use serde::{Deserialize, Serialize};

/// Frozen into every run's origin and embedded in reports.
pub const TAXONOMY_VERSION: &str = "1";

/// `Proofs/Eval.lean` `OutcomeKind`. Order matches the Lean vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeKind {
    Passed,
    ModelAcceptance,
    Deadline,
    Tool,
    Runtime,
    SkippedPrerequisite,
    Provider,
    Infrastructure,
    Inconclusive,
    Grader,
    Unknown,
}

impl OutcomeKind {
    pub const ALL: [OutcomeKind; 11] = [
        Self::Passed,
        Self::ModelAcceptance,
        Self::Deadline,
        Self::Tool,
        Self::Runtime,
        Self::SkippedPrerequisite,
        Self::Provider,
        Self::Infrastructure,
        Self::Inconclusive,
        Self::Grader,
        Self::Unknown,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::ModelAcceptance => "model_acceptance",
            Self::Deadline => "deadline",
            Self::Tool => "tool",
            Self::Runtime => "runtime",
            Self::SkippedPrerequisite => "skipped_prerequisite",
            Self::Provider => "provider",
            Self::Infrastructure => "infrastructure",
            Self::Inconclusive => "inconclusive",
            Self::Grader => "grader",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == value)
    }
}

/// `Proofs/Eval.lean` `ProviderReason`: why a provider call did not produce a
/// usable answer. A `provider` outcome needs one, because a candidate prompt
/// can cause a context overflow but not an outage.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderReason {
    /// A 4xx such as context overflow or a content-policy refusal.
    Rejected,
    /// A 5xx, a connection failure, rate limiting.
    Unavailable,
}

impl ProviderReason {
    pub const ALL: [ProviderReason; 2] = [Self::Rejected, Self::Unavailable];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rejected => "rejected",
            Self::Unavailable => "unavailable",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|reason| reason.as_str() == value)
    }
}

/// `Proofs/Eval.lean` `EvidenceClass`: what one outcome is evidence of.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceClass {
    Pass,
    Fail,
    NotEvidence,
    Unknown,
}

impl EvidenceClass {
    pub const ALL: [EvidenceClass; 4] = [Self::Pass, Self::Fail, Self::NotEvidence, Self::Unknown];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::NotEvidence => "not_evidence",
            Self::Unknown => "unknown",
        }
    }
}

/// `Eval.classify`. Anything the subject could cause counts against it.
pub fn classify(kind: OutcomeKind, reason: Option<ProviderReason>) -> EvidenceClass {
    use OutcomeKind::*;
    match (kind, reason) {
        (Passed, _) => EvidenceClass::Pass,
        (ModelAcceptance | Deadline | Tool | Runtime | SkippedPrerequisite, _) => {
            EvidenceClass::Fail
        }
        (Provider, Some(ProviderReason::Rejected)) => EvidenceClass::Fail,
        (Provider, Some(ProviderReason::Unavailable)) => EvidenceClass::NotEvidence,
        (Provider, None) => EvidenceClass::Unknown,
        (Infrastructure, _) => EvidenceClass::NotEvidence,
        (Inconclusive | Grader | Unknown, _) => EvidenceClass::Unknown,
    }
}

/// `Eval.subjectCausable`.
///
/// Exhaustive by construction, with no wildcard arm: adding an outcome kind or a
/// provider reason must not silently default to "not the subject's fault", so
/// the compiler is what forces the new case to be decided here.
pub fn subject_causable(kind: OutcomeKind, reason: Option<ProviderReason>) -> bool {
    use OutcomeKind::*;
    use ProviderReason::{Rejected, Unavailable};
    match (kind, reason) {
        // Failures the subject under evaluation can itself produce.
        (
            ModelAcceptance | Deadline | Tool | Runtime | SkippedPrerequisite,
            None | Some(Rejected | Unavailable),
        ) => true,
        // A candidate prompt can cause a 4xx; it cannot cause an outage, and a
        // provider outcome with no reason says nothing about the subject.
        (Provider, Some(Rejected)) => true,
        (Provider, Some(Unavailable) | None) => false,
        // A pass is not a failure; the rest are not evidence about the subject.
        (
            Passed | Infrastructure | Inconclusive | Grader | Unknown,
            None | Some(Rejected | Unavailable),
        ) => false,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn no_subject_causable_outcome_is_excluded() {
        for kind in OutcomeKind::ALL {
            for reason in [
                None,
                Some(ProviderReason::Rejected),
                Some(ProviderReason::Unavailable),
            ] {
                if subject_causable(kind, reason) {
                    assert_eq!(
                        classify(kind, reason),
                        EvidenceClass::Fail,
                        "{kind:?} {reason:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn names_round_trip() {
        for kind in OutcomeKind::ALL {
            assert_eq!(OutcomeKind::parse(kind.as_str()), Some(kind));
            assert_eq!(serde_json::to_value(kind).unwrap(), kind.as_str());
            assert_eq!(
                serde_json::from_value::<OutcomeKind>(json!(kind.as_str())).unwrap(),
                kind,
                "{kind:?} must deserialize from its own wire name"
            );
        }
        for reason in ProviderReason::ALL {
            assert_eq!(ProviderReason::parse(reason.as_str()), Some(reason));
            assert_eq!(serde_json::to_value(reason).unwrap(), reason.as_str());
            assert_eq!(
                serde_json::from_value::<ProviderReason>(json!(reason.as_str())).unwrap(),
                reason,
                "{reason:?} must deserialize from its own wire name"
            );
        }
        for class in EvidenceClass::ALL {
            assert_eq!(serde_json::to_value(class).unwrap(), class.as_str());
            assert_eq!(
                serde_json::from_value::<EvidenceClass>(json!(class.as_str())).unwrap(),
                class,
                "{class:?} must deserialize from its own wire name"
            );
        }
    }
}
