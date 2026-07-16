use defra_agent::goal::{decide_goal_continuation, GoalDecision, GoalRequestTerminal, GoalStatus};

use crate::lean_vocab_test::{
    assert_state_machine_contract_is_complete, lean_goal_decision_cases, lean_vocabulary_values,
};

#[test]
fn rust_goal_status_vocabulary_and_machine_match_lean_contract() {
    let rust = [
        GoalStatus::Active,
        GoalStatus::Paused,
        GoalStatus::Blocked,
        GoalStatus::UsageLimited,
        GoalStatus::BudgetLimited,
        GoalStatus::Complete,
    ]
    .map(GoalStatus::as_str);
    assert_eq!(lean_vocabulary_values("GoalStatus"), rust);
    assert_state_machine_contract_is_complete("Goal");
}

#[test]
fn generated_goal_decision_cases_fence_runtime_controller() {
    let cases = lean_goal_decision_cases();
    assert_eq!(cases.len(), 10, "the durable-goal decision matrix drifted");
    for case in cases {
        let status = GoalStatus::parse(&case.status)
            .unwrap_or_else(|| panic!("unknown status in Lean case {}", case.name));
        let terminal = GoalRequestTerminal::parse(&case.terminal)
            .unwrap_or_else(|| panic!("unknown terminal in Lean case {}", case.name));
        let actual = decide_goal_continuation(
            status,
            terminal,
            case.session_idle,
            case.child_exists,
            case.budget_reached,
            case.has_activity,
            case.infrastructure_retries,
            case.wrapup_requested,
            case.wrapup_completed,
        );
        let expected = match case.expected_decision.as_str() {
            "none" => GoalDecision::None,
            "continue" => GoalDecision::Continue,
            "retry" => GoalDecision::Retry,
            "pause" => GoalDecision::Pause,
            "wrapup" => GoalDecision::Wrapup,
            other => panic!("unknown decision {other:?} in Lean case {}", case.name),
        };
        assert_eq!(actual, expected, "Lean case {}", case.name);
    }
}
