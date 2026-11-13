use gents::eval::{classify, subject_causable, EvidenceClass, OutcomeKind, ProviderReason};

pub(super) fn rust_eval_outcome_vocabulary_and_projection_match_lean() {
    for kind in OutcomeKind::ALL {
        assert_eq!(serde_json::json!(kind), kind.as_str());
    }
    for reason in ProviderReason::ALL {
        assert_eq!(serde_json::json!(reason), reason.as_str());
    }
    for class in EvidenceClass::ALL {
        assert_eq!(serde_json::json!(class), class.as_str());
    }

    gents_lean_vocab::families::eval_classification::replay::<
        EvidenceClass,
        OutcomeKind,
        ProviderReason,
    >(|input| classify(input.kind, input.provider_reason));
    gents_lean_vocab::families::eval_subject_causability::replay::<OutcomeKind, ProviderReason>(
        |input| subject_causable(input.kind, input.provider_reason),
    );
}
