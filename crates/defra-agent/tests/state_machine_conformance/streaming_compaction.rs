use super::*;

pub(super) fn generated_streaming_response_cases_pin_lifecycle_contract() {
    let cases = lean_response_transition_cases();
    assert_eq!(cases.len(), 12);
    let expected = [
        (
            "begin_emits_streaming_empty",
            "normal",
            "begin",
            true,
            "streaming",
            "streaming",
            "empty",
            "empty",
            0,
            0,
            None,
            None,
            None,
            None,
            None,
        ),
        (
            "write_tokens_advances_progress",
            "normal",
            "write_tokens",
            true,
            "streaming",
            "streaming",
            "empty",
            "nonEmpty",
            0,
            5,
            None,
            None,
            None,
            None,
            None,
        ),
        (
            "write_reasoning_no_token_bump",
            "normal",
            "write_reasoning",
            true,
            "streaming",
            "streaming",
            "empty",
            "nonEmpty",
            0,
            0,
            None,
            None,
            None,
            None,
            None,
        ),
        (
            "flush_pending_is_abstract_noop",
            "normal",
            "flush",
            true,
            "streaming",
            "streaming",
            "nonEmpty",
            "nonEmpty",
            3,
            3,
            None,
            None,
            None,
            None,
            None,
        ),
        (
            "reset_tail_clears_but_preserves_tokens",
            "normal",
            "reset_tail",
            true,
            "streaming",
            "streaming",
            "nonEmpty",
            "empty",
            7,
            7,
            None,
            None,
            None,
            None,
            None,
        ),
        (
            "finalize_complete_clears_and_materializes",
            "normal",
            "finalize_complete",
            true,
            "streaming",
            "complete",
            "nonEmpty",
            "empty",
            10,
            10,
            None,
            None,
            Some(42),
            Some("completed"),
            Some("committed"),
        ),
        (
            "finalize_error_inference_failed_clears",
            "normal",
            "finalize_error",
            true,
            "streaming",
            "error",
            "nonEmpty",
            "empty",
            8,
            8,
            Some("inferenceFailed"),
            None,
            None,
            Some("failed"),
            Some("committed"),
        ),
        (
            "finalize_error_idle_timeout_requires_deadline",
            "normal",
            "finalize_error",
            true,
            "streaming",
            "error",
            "nonEmpty",
            "empty",
            4,
            4,
            Some("streamIdleTimeout"),
            None,
            None,
            Some("failed"),
            Some("committed"),
        ),
        (
            "recover_interrupted_keeps_content",
            "recovery",
            "recover_interrupted",
            true,
            "streaming",
            "error",
            "nonEmpty",
            "nonEmpty",
            6,
            6,
            Some("daemonRestartRecovery"),
            None,
            None,
            Some("failed"),
            Some("committed"),
        ),
        (
            "observe_idempotent_finalize_is_noop",
            "idempotent",
            "observe_idempotent_finalize",
            true,
            "complete",
            "complete",
            "empty",
            "empty",
            12,
            12,
            None,
            Some(99),
            Some(99),
            None,
            None,
        ),
        (
            "set_interrupted_at_does_not_change_status",
            "boundary",
            "set_interrupted_at",
            true,
            "streaming",
            "streaming",
            "nonEmpty",
            "nonEmpty",
            2,
            2,
            None,
            None,
            None,
            None,
            None,
        ),
        (
            "bridge_completed_pairs_request_committed",
            "bridge",
            "finalize_complete",
            true,
            "streaming",
            "complete",
            "nonEmpty",
            "empty",
            15,
            15,
            None,
            None,
            Some(88),
            Some("completed"),
            Some("committed"),
        ),
    ];

    let names = cases
        .iter()
        .map(|case| case.name.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        names,
        expected.iter().map(|case| case.0).collect::<BTreeSet<_>>()
    );
    for case in expected {
        assert_response_transition_case(case);
    }

    for case in cases {
        assert!(case.legal, "streaming case {} should be legal", case.name);
        assert!(
            case.post_token_count >= case.pre_token_count,
            "streaming case {} should not decrease token count",
            case.name
        );
    }
}

type ResponseTransitionExpectation = (
    &'static str,
    &'static str,
    &'static str,
    bool,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    usize,
    usize,
    Option<&'static str>,
    Option<usize>,
    Option<usize>,
    Option<&'static str>,
    Option<&'static str>,
);

fn assert_response_transition_case(expectation: ResponseTransitionExpectation) {
    let (
        name,
        group,
        action,
        legal,
        pre_status,
        post_status,
        pre_live_tail,
        post_live_tail,
        pre_token_count,
        post_token_count,
        error_reason,
        pre_materialized_seq,
        post_materialized_seq,
        expected_request_state,
        expected_request_persistence,
    ) = expectation;
    let case = lean_response_transition_case(name);
    assert_eq!(case.group.as_str(), group);
    assert_eq!(case.action.as_str(), action);
    assert_eq!(case.legal, legal);
    assert_eq!(case.pre_status.as_str(), pre_status);
    assert_eq!(case.post_status.as_str(), post_status);
    assert_eq!(case.pre_live_tail.as_str(), pre_live_tail);
    assert_eq!(case.post_live_tail.as_str(), post_live_tail);
    assert_eq!(case.pre_token_count, pre_token_count);
    assert_eq!(case.post_token_count, post_token_count);
    assert_eq!(case.error_reason.as_deref(), error_reason);
    assert_eq!(case.pre_materialized_seq, pre_materialized_seq);
    assert_eq!(case.post_materialized_seq, post_materialized_seq);
    assert_eq!(
        case.expected_request_state.as_deref(),
        expected_request_state
    );
    assert_eq!(
        case.expected_request_persistence.as_deref(),
        expected_request_persistence
    );
}

pub(super) fn generated_compaction_reducer_cases_pin_contract() {
    let cases = lean_compaction_reducer_cases();
    assert_eq!(cases.len(), 10);
    let expected = [
        (
            "identity_reducer_is_no_op",
            "witness",
            "identity",
            true,
            0,
            0,
            true,
            true,
            false,
            true,
            true,
        ),
        (
            "identity_preserves_pair_atomicity",
            "witness",
            "identity",
            true,
            2,
            2,
            true,
            true,
            false,
            true,
            true,
        ),
        (
            "identity_preserves_message_order",
            "witness",
            "identity",
            true,
            3,
            3,
            true,
            true,
            false,
            true,
            true,
        ),
        (
            "strip_preserves_pair_atomicity",
            "witness",
            "strip_tool_results",
            true,
            2,
            2,
            true,
            true,
            true,
            true,
            true,
        ),
        (
            "strip_preserves_message_order",
            "witness",
            "strip_tool_results",
            true,
            3,
            3,
            true,
            true,
            true,
            true,
            true,
        ),
        (
            "strip_is_strictly_idempotent",
            "witness",
            "strip_tool_results",
            true,
            2,
            2,
            true,
            true,
            true,
            true,
            true,
        ),
        (
            "reduction_blocked_when_response_streaming",
            "streaming",
            "any_valid",
            true,
            1,
            1,
            true,
            true,
            true,
            false,
            true,
        ),
        (
            "reduction_allowed_when_response_terminal",
            "streaming",
            "any_valid",
            true,
            1,
            1,
            true,
            true,
            true,
            true,
            false,
        ),
        (
            "no_orphaned_tool_results_after_strip",
            "contract",
            "strip_tool_results",
            true,
            2,
            2,
            true,
            true,
            true,
            true,
            true,
        ),
        (
            "reapply_preserves_view_coherent",
            "contract",
            "any_valid",
            true,
            2,
            2,
            true,
            true,
            true,
            true,
            true,
        ),
    ];

    let names = cases
        .iter()
        .map(|case| case.name.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        names,
        expected.iter().map(|case| case.0).collect::<BTreeSet<_>>()
    );
    for case in expected {
        assert_compaction_reducer_case(case);
    }
    for case in cases {
        assert!(case.legal, "compaction case {} should be legal", case.name);
        assert!(
            case.preserves_pairs && case.preserves_order,
            "compaction case {} should preserve transcript shape",
            case.name
        );
    }
}

type CompactionReducerExpectation = (
    &'static str,
    &'static str,
    &'static str,
    bool,
    usize,
    usize,
    bool,
    bool,
    bool,
    bool,
    bool,
);

fn assert_compaction_reducer_case(expectation: CompactionReducerExpectation) {
    let (
        name,
        group,
        reducer,
        legal,
        pre_message_count,
        post_message_count,
        preserves_pairs,
        preserves_order,
        gate_open,
        safe_to_reduce,
        reducer_is_identity,
    ) = expectation;
    let case = lean_compaction_reducer_case(name);
    assert_eq!(case.group.as_str(), group);
    assert_eq!(case.reducer.as_str(), reducer);
    assert_eq!(case.legal, legal);
    assert_eq!(case.pre_message_count, pre_message_count);
    assert_eq!(case.post_message_count, post_message_count);
    assert_eq!(case.preserves_pairs, preserves_pairs);
    assert_eq!(case.preserves_order, preserves_order);
    assert_eq!(case.gate_open, gate_open);
    assert_eq!(case.safe_to_reduce, safe_to_reduce);
    assert_eq!(case.reducer_is_identity, reducer_is_identity);
}
