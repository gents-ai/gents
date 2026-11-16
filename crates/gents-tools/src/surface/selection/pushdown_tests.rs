#[test]
fn goal_capability_resolution_matches_the_lean_model() {
    gents_lean_vocab::families::contracts_goal_capability_resolution::replay(|i| {
        let (goal_tools, goal_create) = crate::surface::resolve_goal_capabilities(
            i.explicit_goal_tools,
            i.explicit_goal_create,
        );
        gents_lean_vocab::families::contracts_goal_capability_resolution::GoalCapabilityResolutionOutput {
            goal_tools,
            goal_create,
        }
    });
}
