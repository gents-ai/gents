#[path = "../src/lean_vocab_test/support.rs"]
mod lean_vocab_test;

#[test]
fn barrier_cases_match_projection() {
    let cases = lean_vocab_test::lean_workflow_cases();
    assert!(
        !cases.is_empty(),
        "Lean workflow_cases must include barrier witnesses"
    );

    for case in cases {
        let actual = gents::workflow::workflow_barrier_projection_legal(
            case.group_terminal_states.iter().map(String::as_str),
            case.synthesis_present,
        );
        assert_eq!(
            actual, case.legal,
            "workflow case {} disagreed with projection",
            case.name
        );
    }
}

/// #837: Lean composite-interrupt cleanup cases pin that after bounded
/// interrupt cleanup (or terminal-parent recovery) the outer composite is
/// not eligible as active and carries a consistent interrupt cancel cause.
#[test]
fn composite_interrupt_cases_pin_cleanup_invariant() {
    let cases = lean_vocab_test::lean_workflow_composite_interrupt_cases();
    assert!(
        !cases.is_empty(),
        "Lean workflow_composite_interrupt_cases must include interrupt witnesses"
    );

    let required_phases = [
        "fanOutSpawn",
        "fanOutBarrier",
        "synthesisSpawn",
        "synthesisRun",
        "resultPersist",
    ];
    for phase in required_phases {
        assert!(
            cases.iter().any(|case| case.phase == phase),
            "missing composite interrupt phase witness {phase}"
        );
    }
    assert!(
        cases
            .iter()
            .any(|case| case.name == "duplicate_interrupt_delivery"),
        "missing duplicate interrupt witness"
    );
    assert!(
        cases
            .iter()
            .any(|case| case.name.contains("recover_terminal_parent")),
        "missing terminal-parent recovery witness"
    );

    for case in cases {
        assert!(
            !case.post_outer_eligible_active,
            "case {} must leave outer not eligible as active",
            case.name
        );
        assert!(
            !case.post_continuation_owned,
            "case {} must release continuation ownership",
            case.name
        );
        if case.outer_state == "running" || case.outer_state == "pending" {
            if case.parent_state == "interrupted" || case.parent_state == "processing" {
                assert_eq!(
                    case.post_outer_state, "cancelled",
                    "case {} should cancel a still-eligible outer under interrupt",
                    case.name
                );
                assert_eq!(
                    case.post_outer_cancel_cause.as_deref(),
                    Some("interrupted"),
                    "case {} should project cancel_cause=interrupted",
                    case.name
                );
            } else {
                assert!(
                    case.post_outer_state == "cancelled" || case.post_outer_state == "failed",
                    "case {} must terminalize eligible outer, got {}",
                    case.name,
                    case.post_outer_state
                );
            }
        }
        if case.outer_state == "cancelled" {
            assert_eq!(
                case.post_outer_state, "cancelled",
                "duplicate interrupt must keep cancelled outer for {}",
                case.name
            );
            assert_eq!(
                case.post_outer_cancel_cause.as_deref(),
                Some("interrupted"),
                "duplicate interrupt must keep cause for {}",
                case.name
            );
        }
    }
}
