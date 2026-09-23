//! Drive the generated eval outcome projection (`Proofs/Eval.lean`, emitted by
//! `Proofs/Conformance/Eval.lean` as `eval_outcome_cases`) through the
//! production classifier. The vocabularies and the projection table live in
//! Lean; `gents::eval::outcome` is the only Rust owner, so this module compares
//! against it instead of restating the mapping.

use std::collections::BTreeSet;

use gents::eval::{classify, subject_causable, EvidenceClass, OutcomeKind, ProviderReason};

use crate::lean_vocab_test::{lean_eval_outcome_cases, lean_vocabulary_values};

pub(super) fn rust_eval_outcome_vocabulary_and_projection_match_lean() {
    assert_eq!(
        lean_vocabulary_values("EvalOutcomeKind"),
        OutcomeKind::ALL.map(OutcomeKind::as_str)
    );
    assert_eq!(
        lean_vocabulary_values("EvalProviderReason"),
        ProviderReason::ALL.map(ProviderReason::as_str)
    );
    assert_eq!(
        lean_vocabulary_values("EvalEvidenceClass"),
        EvidenceClass::ALL.map(EvidenceClass::as_str)
    );

    let cases = lean_eval_outcome_cases();
    // The count pins the table's size; the set pins its contents, so a duplicated
    // row can no longer stand in for a missing pair.
    assert_eq!(
        cases.len(),
        OutcomeKind::ALL.len() * (ProviderReason::ALL.len() + 1),
        "Lean must emit every (outcome kind, optional provider reason) pair"
    );
    let emitted: BTreeSet<(&str, Option<&str>)> = cases
        .iter()
        .map(|case| (case.kind.as_str(), case.provider_reason.as_deref()))
        .collect();
    let expected: BTreeSet<(&str, Option<&str>)> = OutcomeKind::ALL
        .into_iter()
        .flat_map(|kind| {
            ProviderReason::ALL
                .into_iter()
                .map(ProviderReason::as_str)
                .map(Some)
                .chain(std::iter::once(None))
                .map(move |reason| (kind.as_str(), reason))
        })
        .collect();
    assert_eq!(
        emitted, expected,
        "the emitted table must be exactly OutcomeKind::ALL x (ProviderReason::ALL + none)"
    );

    for case in cases {
        let kind = OutcomeKind::parse(&case.kind).expect("kind");
        let reason = case
            .provider_reason
            .as_deref()
            .map(|reason| ProviderReason::parse(reason).expect("reason"));
        assert_eq!(classify(kind, reason).as_str(), case.class, "{case:?}");
        assert_eq!(
            subject_causable(kind, reason),
            case.subject_causable,
            "{case:?}"
        );
    }
}
