use gents_lean_vocab::lean_goal_capability_resolution_cases;

#[test]
fn generated_goal_capability_resolution_matches_rust_decoder() {
    let cases = lean_goal_capability_resolution_cases();
    assert_eq!(cases.len(), 4, "goal capability resolution matrix drifted");
    for case in cases {
        let (goal_tools, goal_creation) = crate::surface::resolve_goal_capabilities(
            case.explicit_goal_tools,
            case.explicit_goal_create,
        );
        assert_eq!(
            (goal_tools, goal_creation),
            (case.expected_goal_tools, case.expected_goal_create),
            "Lean capability resolution case {}",
            case.name
        );
    }
}
