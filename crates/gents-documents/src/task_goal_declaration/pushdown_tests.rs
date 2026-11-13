use super::validate_task_goal_declaration;

#[test]
fn task_declaration_treats_omitted_budget_as_uncapped_and_still_rejects_bad_caps() {
    // Omitted budget with an objective is valid: uncapped by default.
    validate_task_goal_declaration(Some("ship it"), None)
        .expect("an objective without a budget is a valid uncapped goal");

    // No objective and no budget is also a valid non-goal task.
    validate_task_goal_declaration(None, None)
        .expect("a task without a goal declaration needs no budget");

    // A budget cannot exist without an objective (this is what rules out a
    // huge sentinel budget smuggled onto a non-goal task).
    let error = validate_task_goal_declaration(None, Some(i64::MAX))
        .expect_err("a budget without an objective must be rejected");
    assert!(
        error.to_string().contains("requires a goal objective"),
        "unexpected rejection reason: {error}"
    );

    // Caps stay bounded and positive; no sentinel escape hatch.
    assert!(validate_task_goal_declaration(Some("ship it"), Some(0)).is_err());
    assert!(validate_task_goal_declaration(Some("ship it"), Some(-1)).is_err());
    assert!(validate_task_goal_declaration(Some("ship it"), Some(i64::MAX)).is_ok());
}

#[test]
fn task_goal_declarations_follow_the_lean_publication_model() {
    gents_lean_vocab::families::contracts_task_goal_declaration::replay(|case| {
        let budget = case
            .goal_token_budget
            .as_ref()
            .map(|budget| budget.0.as_i64());
        gents_lean_vocab::families::contracts_task_goal_declaration::TaskGoalDeclarationOutput {
            declaration_valid: match budget {
                Some(None) => false,
                Some(Some(budget)) => {
                    validate_task_goal_declaration(case.goal_objective.as_deref(), Some(budget))
                        .is_ok()
                }
                None => {
                    validate_task_goal_declaration(case.goal_objective.as_deref(), None).is_ok()
                }
            },
        }
    });
}
