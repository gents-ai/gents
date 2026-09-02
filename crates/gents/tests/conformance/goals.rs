use gents::goal::{
    decide_goal_continuation, decide_model_goal_create, goal_continuation_materialization_step,
    goal_creation_fingerprint, goal_submission_step, GoalAction, GoalAuditObservation,
    GoalContinuationAction, GoalContinuationPhase, GoalCreateDisposition, GoalCreateRequest,
    GoalCreationFingerprint, GoalDecision, GoalRequestTerminal, GoalState, GoalStatus,
    GoalSubmissionAction, GoalSubmissionState,
};

use crate::lean_vocab_test::{
    assert_state_machine_contract_is_complete, lean_goal_continuation_materialization_cases,
    lean_goal_create_cases, lean_goal_decision_cases, lean_goal_submission_cases,
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

#[test]
fn generated_goal_create_cases_fence_authority_and_idempotency() {
    let cases = lean_goal_create_cases();
    assert_eq!(cases.len(), 14, "the durable-goal creation matrix drifted");
    for case in cases {
        let request = GoalCreateRequest {
            caller: case.caller.clone(),
            current_session: case.current_session.clone(),
            requested_owner: case.requested_owner.clone(),
            requested_session: case.requested_session.clone(),
            objective: case.objective.clone(),
            objective_nonempty: case.objective_nonempty,
            token_budget: case.token_budget,
            goal_tools: case.goal_tools,
            goal_create: case.goal_create,
        };
        let matching = goal_creation_fingerprint(&request);
        let conflicting = GoalCreationFingerprint {
            owner: request.caller.clone(),
            session: request.current_session.clone(),
            objective: format!("{}-conflict", request.objective),
            token_budget: request.token_budget,
        };
        let existing = case.existing.then_some(if case.existing_matches {
            &matching
        } else {
            &conflicting
        });
        let actual = decide_model_goal_create(&request, existing);
        let expected = match case.expected.as_str() {
            "denied" => GoalCreateDisposition::Denied,
            "invalid" => GoalCreateDisposition::Invalid,
            "fresh" => GoalCreateDisposition::Fresh,
            "idempotent" => GoalCreateDisposition::Idempotent,
            "conflict" => GoalCreateDisposition::Conflict,
            other => panic!(
                "unknown create disposition {other:?} in Lean case {}",
                case.name
            ),
        };
        assert_eq!(actual, expected, "Lean case {}", case.name);
    }
}

#[test]
fn generated_goal_submission_cases_fence_atomic_visibility() {
    let cases = lean_goal_submission_cases();
    assert_eq!(cases.len(), 5, "the goal submission matrix drifted");
    for case in cases {
        let action = match case.action.as_str() {
            "stage_goal" => GoalSubmissionAction::StageGoal,
            "stage_request" => GoalSubmissionAction::StageRequest,
            "commit" => GoalSubmissionAction::Commit,
            "abort" => GoalSubmissionAction::Abort,
            "crash" => GoalSubmissionAction::Crash,
            other => panic!(
                "unknown submission action {other:?} in Lean case {}",
                case.name
            ),
        };
        let actual = goal_submission_step(
            GoalSubmissionState {
                durable_goal: case.durable_goal,
                runnable_request: case.runnable_request,
                staged_goal: case.staged_goal,
                staged_request: case.staged_request,
            },
            action,
        );
        assert_eq!(
            actual.durable_goal, case.expected_durable_goal,
            "Lean case {}",
            case.name
        );
        assert_eq!(
            actual.runnable_request, case.expected_runnable_request,
            "Lean case {}",
            case.name
        );
        assert_eq!(
            actual.staged_goal, case.expected_staged_goal,
            "Lean case {}",
            case.name
        );
        assert_eq!(
            actual.staged_request, case.expected_staged_request,
            "Lean case {}",
            case.name
        );
        assert!(
            !actual.runnable_request || actual.durable_goal,
            "Lean case {} exposed a runnable request without its durable goal",
            case.name
        );
    }
}

#[test]
fn generated_goal_continuation_materialization_cases_fence_restart_idempotency() {
    let cases = lean_goal_continuation_materialization_cases();
    assert_eq!(
        cases.len(),
        5,
        "the continuation materialization matrix drifted"
    );
    for case in cases {
        let phase = parse_continuation_phase(&case.phase, &case.name);
        let action = match case.action.as_str() {
            "claim_eligible" => GoalContinuationAction::Claim(true),
            "claim_ineligible" => GoalContinuationAction::Claim(false),
            "materialize" => GoalContinuationAction::Materialize,
            "reconcile" => GoalContinuationAction::Reconcile,
            "crash" => GoalContinuationAction::Crash,
            other => panic!(
                "unknown continuation action {other:?} in Lean case {}",
                case.name
            ),
        };
        let actual = goal_continuation_materialization_step(phase, action);
        let expected = parse_continuation_phase(&case.expected_phase, &case.name);
        assert_eq!(actual, expected, "Lean case {}", case.name);
    }
}

fn parse_continuation_phase(value: &str, case_name: &str) -> GoalContinuationPhase {
    match value {
        "unclaimed" => GoalContinuationPhase::Unclaimed,
        "claimed" => GoalContinuationPhase::Claimed,
        "child_present" => GoalContinuationPhase::ChildPresent,
        other => panic!("unknown continuation phase {other:?} in Lean case {case_name}"),
    }
}
