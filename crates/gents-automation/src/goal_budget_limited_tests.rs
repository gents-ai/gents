
#[test]
fn update_goal_cannot_resume_from_budget_limited() {
    use crate::goal::{GoalAction, GoalState};
    use gents_protocol::goal_status::GoalStatus;

    // The state machine itself must have no Resume edge out of
    // budget_limited, independent of any help text.
    let budget_limited = GoalState {
        status: GoalStatus::BudgetLimited,
        blocked_audits: 0,
        wrapup_requested: true,
        wrapup_completed: false,
    };
    assert_eq!(
        budget_limited.step(GoalAction::Resume),
        None,
        "Resume from budget_limited must be an illegal transition"
    );
    // And the operator transition helper must reject it too.
    assert!(
        crate::goal::apply_operator_status_transition(budget_limited, GoalStatus::Active)
            .is_err(),
        "update_goal to active from budget_limited must be rejected"
    );
}
