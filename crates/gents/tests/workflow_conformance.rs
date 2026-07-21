#[path = "../src/lean_vocab_test.rs"]
mod lean_vocab_test;

#[test]
fn barrier_cases_match_projection() {
    let cases = lean_vocab_test::lean_workflow_cases();
    assert!(
        !cases.is_empty(),
        "Lean workflow_cases must include barrier witnesses"
    );

    for case in cases {
        let actual = defra_agent::workflow::workflow_barrier_projection_legal(
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
