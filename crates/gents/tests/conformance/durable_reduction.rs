use crate::lean_vocab_test::lean_durable_reduction_cases;
use gents::provider_context_reduction::reduction_key;

#[test]
fn generated_durable_reduction_cases_pin_identity_and_persist_before_send() {
    let cases = lean_durable_reduction_cases();
    assert!(!cases.is_empty(), "Lean emitted no durable-reduction cases");

    for case in cases {
        let key = reduction_key(
            "did:key:agent",
            "session-11",
            &format!("request-doc-{}", case.request_doc_id),
            case.turn_index,
            case.ordinal,
        )
        .unwrap();
        let redelivery_key = reduction_key(
            "did:key:agent",
            "session-11",
            &format!("request-doc-{}", case.request_doc_id),
            case.turn_index,
            case.ordinal,
        )
        .unwrap();
        assert_eq!(key, redelivery_key, "{} key is not idempotent", case.name);

        let rust_outcome = match (case.pair_closed, case.prior_checkpoint) {
            (false, _) => "pair_open",
            (true, None) => "fresh",
            (true, Some(prior)) if prior == case.checkpoint => "idempotent",
            (true, Some(_)) => "conflict",
        };
        assert_eq!(rust_outcome, case.outcome, "{} outcome drifted", case.name);
        let durable_after = matches!(rust_outcome, "fresh" | "idempotent");
        assert_eq!(
            durable_after, case.durable_after,
            "{} durability drifted",
            case.name
        );
        assert_eq!(
            durable_after && case.pair_closed,
            case.send_permitted,
            "{} send fence drifted",
            case.name
        );
    }

    let first = &cases[0];
    let first_key = reduction_key(
        "did:key:agent",
        "session-11",
        &format!("request-doc-{}", first.request_doc_id),
        first.turn_index,
        first.ordinal,
    )
    .unwrap();
    for case in &cases[1..] {
        if case.request_doc_id != first.request_doc_id
            || case.turn_index != first.turn_index
            || case.ordinal != first.ordinal
        {
            let other = reduction_key(
                "did:key:agent",
                "session-11",
                &format!("request-doc-{}", case.request_doc_id),
                case.turn_index,
                case.ordinal,
            )
            .unwrap();
            assert_ne!(first_key, other, "{} collapsed a distinct fact", case.name);
        }
    }
}
