use gents::goal::{
    decide_goal_continuation, GoalAction, GoalAuditObservation, GoalDecision, GoalRequestTerminal,
    GoalState, GoalStatus,
};

use crate::lean_vocab_test::{
    assert_state_machine_contract_is_complete, lean_goal_decision_cases,
    lean_goal_transition_cases, lean_vocabulary_values,
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
    assert_eq!(cases.len(), 18, "the durable-goal decision matrix drifted");
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
            case.request_is_wrapup,
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
            "abandon_wrapup" => GoalDecision::AbandonWrapup,
            other => panic!("unknown decision {other:?} in Lean case {}", case.name),
        };
        assert_eq!(actual, expected, "Lean case {}", case.name);
    }
}

#[test]
fn generated_goal_transition_cases_fence_runtime_state_machine() {
    let cases = lean_goal_transition_cases();
    assert_eq!(
        cases.len(),
        17,
        "the durable-goal transition matrix drifted"
    );
    for case in cases {
        let pre = GoalState {
            status: GoalStatus::parse(&case.pre_status)
                .unwrap_or_else(|| panic!("unknown pre-status in Lean case {}", case.name)),
            blocked_audits: case.pre_blocked_audits,
            wrapup_requested: case.pre_wrapup_requested,
            wrapup_completed: case.pre_wrapup_completed,
        };
        let action = match case.action.as_str() {
            "pause" => GoalAction::Pause,
            "resume" => GoalAction::Resume,
            "complete" => GoalAction::Complete,
            "blocked_audit_same_request" => {
                GoalAction::BlockedAudit(GoalAuditObservation::SameRequest)
            }
            "blocked_audit_same_condition" => {
                GoalAction::BlockedAudit(GoalAuditObservation::SameCondition)
            }
            "blocked_audit_new_condition" => {
                GoalAction::BlockedAudit(GoalAuditObservation::NewCondition)
            }
            "operator_block" => GoalAction::OperatorBlock,
            "usage_limit" => GoalAction::UsageLimit,
            "budget_exhausted" => GoalAction::BudgetExhausted,
            "wrapup_finished" => GoalAction::WrapupFinished,
            "wrapup_abandoned" => GoalAction::WrapupAbandoned,
            "clean_turn" => GoalAction::CleanTurn,
            other => panic!("unknown action {other:?} in Lean case {}", case.name),
        };
        let actual = pre.step(action);
        assert_eq!(actual.is_some(), case.accepted, "Lean case {}", case.name);
        if let Some(actual) = actual {
            assert_eq!(
                actual.status.as_str(),
                case.expected_status,
                "Lean case {}",
                case.name
            );
            assert_eq!(
                actual.blocked_audits, case.expected_blocked_audits,
                "Lean case {}",
                case.name
            );
            assert_eq!(
                actual.wrapup_requested, case.expected_wrapup_requested,
                "Lean case {}",
                case.name
            );
            assert_eq!(
                actual.wrapup_completed, case.expected_wrapup_completed,
                "Lean case {}",
                case.name
            );
        }
    }
}
