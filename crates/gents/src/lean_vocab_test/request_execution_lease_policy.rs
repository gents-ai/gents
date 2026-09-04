use super::*;
use crate::lean_vocab_test::lean_request_execution_lease_cases;
use crate::lifecycle::execution_policy::{
    authorize_execution_revocation, authorize_live_execution, ExecutionObservation,
    ExecutionOperation,
};
use gents_protocol::request_lifecycle::RequestLifecycleState as RequestState;

fn request_state(phase: LeanRequestExecutionRequestPhase) -> RequestState {
    match phase {
        LeanRequestExecutionRequestPhase::Pending => RequestState::Pending,
        LeanRequestExecutionRequestPhase::Claimed => RequestState::Claimed,
        LeanRequestExecutionRequestPhase::Processing => RequestState::Processing,
        LeanRequestExecutionRequestPhase::Completed => RequestState::Completed,
        LeanRequestExecutionRequestPhase::Failed => RequestState::Failed,
        LeanRequestExecutionRequestPhase::Interrupted => RequestState::Interrupted,
        LeanRequestExecutionRequestPhase::Dead => RequestState::Dead,
        LeanRequestExecutionRequestPhase::Superseded => RequestState::Superseded,
    }
}

fn outcome_state(outcome: LeanRequestExecutionOutcome) -> RequestState {
    match outcome {
        LeanRequestExecutionOutcome::Completed => RequestState::Completed,
        LeanRequestExecutionOutcome::Failed => RequestState::Failed,
        LeanRequestExecutionOutcome::Interrupted => RequestState::Interrupted,
        LeanRequestExecutionOutcome::Dead => RequestState::Dead,
        LeanRequestExecutionOutcome::Superseded => RequestState::Superseded,
    }
}

/// Consumes the Lean one-step observations through the actual production
/// authorization seam. Abstract claim/recovery history and terminal-effect
/// bookkeeping are deliberately not simulated by a second Rust machine.
#[test]
fn generated_request_execution_lease_cases_fence_production_policy() {
    let cases = lean_request_execution_lease_cases();
    assert_eq!(cases.len(), 34);
    let mut checked = 0;
    for case in cases {
        let owner = case.pre.lease.generation.unwrap_or_default().to_string();
        let observed = ExecutionObservation {
            request: request_state(case.pre.request),
            response_streaming: match case.pre.response {
                LeanRequestExecutionResponsePhase::Absent => None,
                LeanRequestExecutionResponsePhase::Streaming => Some(true),
                _ => Some(false),
            },
            generation: &owner,
            deadline: case.pre.lease.deadline.unwrap_or_default() as i64,
            progress_seq: case.pre.progress_seq,
        };
        let (generation, operation) = match case.action {
            LeanRequestExecutionAction::Begin { generation } => {
                (generation, ExecutionOperation::Begin)
            }
            LeanRequestExecutionAction::PersistProgress {
                generation,
                deadline,
                ..
            } => (
                generation,
                ExecutionOperation::Progress {
                    new_deadline: deadline as i64,
                },
            ),
            LeanRequestExecutionAction::Finalize {
                generation,
                outcome,
            } => (
                generation,
                ExecutionOperation::Finalize {
                    completed: outcome == LeanRequestExecutionOutcome::Completed,
                },
            ),
            LeanRequestExecutionAction::Revoke {
                expected_generation,
                expected_deadline,
                expected_progress,
                fresh_generation,
                outcome,
            } => {
                let expected_owner = expected_generation.to_string();
                let expected = ExecutionObservation {
                    generation: &expected_owner,
                    deadline: expected_deadline as i64,
                    progress_seq: expected_progress,
                    ..observed
                };
                let authorized = authorize_execution_revocation(
                    observed,
                    expected,
                    &fresh_generation.to_string(),
                    outcome_state(outcome),
                );
                assert_eq!(authorized, case.expected.is_some(), "{}", case.name);
                checked += 1;
                continue;
            }
            _ => continue,
        };
        let authorized = authorize_live_execution(
            observed,
            &generation.to_string(),
            case.pre.now as i64,
            operation,
        );
        assert_eq!(authorized, case.expected.is_some(), "{}", case.name);
        checked += 1;
    }
    assert_eq!(
        checked, 23,
        "all generated live-authorization and revocation cases must run"
    );
}

#[test]
fn generated_provider_eof_cases_fence_production_policy() {
    let cases = crate::lean_vocab_test::lean_provider_eof_cases();
    assert_eq!(cases.len(), 2);
    for case in cases {
        assert_eq!(
            crate::lifecycle::execution_policy::provider_eof_is_failure(case.saw_explicit_final),
            case.expected_failure,
            "{case:?}"
        );
    }
}
