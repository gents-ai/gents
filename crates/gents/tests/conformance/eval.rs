use gents::eval::{classify, subject_causable, EvidenceClass, OutcomeKind, ProviderReason};

use gents_lean_vocab::lean_vocabulary_values;

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

    gents_lean_vocab::families::eval_classification::replay::<
        EvidenceClass,
        OutcomeKind,
        ProviderReason,
    >(|input| classify(input.kind, input.provider_reason));
    gents_lean_vocab::families::eval_subject_causability::replay::<OutcomeKind, ProviderReason>(
        |input| subject_causable(input.kind, input.provider_reason),
    );
}
