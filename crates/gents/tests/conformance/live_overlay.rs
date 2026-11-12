use gents_lean_vocab::{lean_queued_steering_guard_cases, lean_queued_steering_trace_cases};

#[test]
fn queued_steering_traces_are_derived_from_connected_owners() {
    let cases = lean_queued_steering_trace_cases();
    assert_eq!(
        cases.len(),
        9,
        "the connected owner traces must be exported"
    );
    for case in cases {
        assert_eq!(case.request_id, case.entry.request_id, "{}", case.name);
        assert_eq!(
            case.capture.request_doc_id, case.request_doc_id,
            "{}",
            case.name
        );
        assert!(
            !case.actions.is_empty(),
            "{} has no generated script",
            case.name
        );
        assert!(
            case.accepted_input,
            "{} lost its signed admission",
            case.name
        );
        if matches!(case.lifecycle_state.as_str(), "failed" | "interrupted") {
            assert_eq!(
                case.queue_active, None,
                "{} left terminal work active",
                case.name
            );
        }
    }
}

#[test]
fn queued_steering_rejects_incoherent_claim_and_interrupted_publication() {
    let cases = lean_queued_steering_guard_cases();
    assert_eq!(cases.len(), 3);
    for case in cases {
        assert!(
            case.prefix_admitted,
            "case {:?} never reached its rejecting stage",
            case.name
        );
        assert!(
            !case.admitted,
            "case {:?} was unexpectedly admitted",
            case.name
        );
    }
}
