use crate::goal::{
    decide_goal_continuation, decide_model_goal_create, gate_goal_continuation,
    goal_continuation_materialization_step, goal_creation_fingerprint, goal_submission_step,
    next_goal_infrastructure_retries, observe_goal_agent_readiness, GoalAction,
    GoalAgentObservation, GoalAgentReadiness, GoalAuditObservation, GoalContinuationAction,
    GoalContinuationFacts, GoalContinuationPhase, GoalCreateDisposition, GoalCreateRequest,
    GoalCreationFingerprint, GoalDecision, GoalFailureCause, GoalGatedDecision,
    GoalRequestTerminal, GoalState, GoalSubmissionState,
};
use gents_lean_vocab::{
    lean_goal_create_cases, lean_task_goal_publication_cases, lean_task_goal_recovery_cases,
};
use gents_protocol::goal_status::GoalStatus;

#[test]
fn generated_goal_decision_cases_fence_runtime_controller() {
    gents_lean_vocab::families::contracts_goal_decision::replay(|case| {
        decide_goal_continuation(
            case.status,
            case.terminal,
            case.session_idle,
            case.child_exists,
            case.budget_reached,
            case.has_activity,
            case.request_is_wrapup,
            i64::try_from(case.infrastructure_retries).expect("modeled retries fit i64"),
            case.wrapup_requested,
            case.wrapup_completed,
        )
    });
}

#[test]
fn generated_goal_readiness_gate_cases_fence_retry_accounting() {
    gents_lean_vocab::families::contracts_goal_readiness_gate::replay::<
        GoalFailureCause,
        String,
        String,
        GoalAgentReadiness,
        GoalRequestTerminal,
        GoalStatus,
    >(|case| {
        let observation = match case.observation.as_str() {
            "ready" => GoalAgentObservation::Ready {
                newer_than_terminal: case.newer_than_terminal,
            },
            "backend_recovering" => GoalAgentObservation::BackendRecovering,
            "unavailable" => GoalAgentObservation::Unavailable,
            "unassigned" => GoalAgentObservation::Unassigned,
            "unknown" => GoalAgentObservation::Unknown,
            other => panic!("unknown observation {other:?} in Lean case {}", case.name),
        };
        let facts = GoalContinuationFacts {
            status: case.status,
            terminal: case.terminal,
            session_idle: case.session_idle,
            child_exists: case.child_exists,
            budget_reached: case.budget_reached,
            has_activity: case.has_activity,
            request_is_wrapup: case.request_is_wrapup,
            infrastructure_retries: i64::try_from(case.infrastructure_retries)
                .expect("modeled retries fit i64"),
            wrapup_requested: case.wrapup_requested,
            wrapup_completed: case.wrapup_completed,
        };
        let gated = gate_goal_continuation(observation, case.settled, case.cause, &facts);
        gents_lean_vocab::families::contracts_goal_readiness_gate::ReadinessGateOutput {
            readiness: observe_goal_agent_readiness(observation, case.settled),
            gate: gated_wire(gated).to_owned(),
            retries: u64::try_from(next_goal_infrastructure_retries(case.cause, &facts, gated))
                .expect("retries are never negative"),
        }
    });
}

fn gated_wire(gated: GoalGatedDecision) -> &'static str {
    match gated {
        GoalGatedDecision::Decided(GoalDecision::None) => "none",
        GoalGatedDecision::Decided(GoalDecision::Continue) => "continue",
        GoalGatedDecision::Decided(GoalDecision::Retry) => "retry",
        GoalGatedDecision::Decided(GoalDecision::Pause) => "pause",
        GoalGatedDecision::Decided(GoalDecision::Wrapup) => "wrapup",
        GoalGatedDecision::Decided(GoalDecision::AbandonWrapup) => "abandon_wrapup",
        GoalGatedDecision::AwaitReadiness => "await_readiness",
        GoalGatedDecision::AgentUnavailable => "agent_unavailable",
    }
}

#[test]
fn generated_goal_transition_cases_fence_runtime_state_machine() {
    gents_lean_vocab::families::contracts_goal_transition::replay::<String, GoalStatus>(|case| {
        let pre = GoalState {
            status: case.pre_status,
            blocked_audits: i64::try_from(case.pre_blocked_audits).expect("modeled audits fit i64"),
            wrapup_requested: case.pre_wrapup_requested,
            wrapup_completed: case.pre_wrapup_completed,
        };
        let action = match case.action.as_str() {
            "pause" => GoalAction::Pause,
            "resume" => GoalAction::Resume,
            "stop" => GoalAction::Stop,
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
        let stepped = pre.step(action);
        let post = stepped.unwrap_or(pre);
        gents_lean_vocab::families::contracts_goal_transition::GoalTransitionOutput {
            accepted: stepped.is_some(),
            status: post.status,
            blocked_audits: u64::try_from(post.blocked_audits).expect("audits are never negative"),
            wrapup_requested: post.wrapup_requested,
            wrapup_completed: post.wrapup_completed,
        }
    });
}

#[test]
fn generated_goal_create_cases_fence_authority_and_idempotency() {
    let cases = lean_goal_create_cases();
    assert_eq!(cases.len(), 16, "the durable-goal creation matrix drifted");
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
fn generated_task_goal_cases_fence_declaration_and_identity() {
    let cases = lean_task_goal_publication_cases();
    assert_eq!(cases.len(), 11, "the Task goal publication matrix drifted");
    for case in cases {
        let declaration_valid = match case.goal_token_budget {
            Some(budget) => i64::try_from(budget).is_ok_and(|budget| {
                gents_documents::task_goal_declaration::validate_task_goal_declaration(
                    case.goal_objective.as_deref(),
                    Some(budget),
                )
                .is_ok()
            }),
            None => gents_documents::task_goal_declaration::validate_task_goal_declaration(
                case.goal_objective.as_deref(),
                None,
            )
            .is_ok(),
        };
        assert_eq!(
            declaration_valid, case.declaration_valid,
            "Lean case {} disagrees with the production declaration validator",
            case.name
        );

        if declaration_valid && case.goal_objective.is_some() {
            let identity =
                crate::goal::task_goal_fire_identity(&case.node_did, &case.task_id, &case.fire_key);
            assert_eq!(
                case.expected_session_id.as_deref(),
                Some(identity.session_id.as_str()),
                "Lean case {} session identity",
                case.name
            );
            assert_eq!(
                case.expected_request_id.as_deref(),
                Some(identity.request_id.as_str()),
                "Lean case {} request identity",
                case.name
            );
            assert_eq!(
                case.expected_retry_key.as_deref(),
                Some(identity.retry_key.as_str()),
                "Lean case {} retry identity",
                case.name
            );
        }
    }
}

#[test]
fn generated_task_goal_recovery_cases_fence_request_witness() {
    let cases = lean_task_goal_recovery_cases();
    assert_eq!(cases.len(), 10, "the Task goal recovery matrix drifted");
    for case in cases {
        let identity =
            crate::goal::task_goal_fire_identity(&case.node_did, &case.task_id, &case.fire_key);
        let expected = crate::goal::TaskGoalRequestBinding {
            node_did: case.node_did.clone(),
            agent_did: case.agent_did.clone(),
            session_id: identity.session_id,
            request_id: identity.request_id,
            retry_key: identity.retry_key,
        };
        let observed = case
            .request_present
            .then(|| crate::goal::TaskGoalRequestBinding {
                node_did: case
                    .observed_node_did
                    .clone()
                    .expect("present request must emit node_did"),
                agent_did: case
                    .observed_agent_did
                    .clone()
                    .expect("present request must emit agent_did"),
                session_id: case
                    .observed_session_id
                    .clone()
                    .expect("present request must emit session_id"),
                request_id: case
                    .observed_request_id
                    .clone()
                    .expect("present request must emit request_id"),
                retry_key: case
                    .observed_retry_key
                    .clone()
                    .expect("present request must emit retry_key"),
            });
        assert_eq!(
            observed.as_ref() == Some(&expected),
            case.request_binding_matches,
            "Lean case {} binding classification",
            case.name
        );
        let actual = crate::goal::decide_task_goal_fire_recovery(&expected, observed.as_ref());
        let disposition = match actual.disposition {
            crate::goal::TaskGoalFireRecoveryDisposition::Absent => "absent",
            crate::goal::TaskGoalFireRecoveryDisposition::Recovered => "recovered",
            crate::goal::TaskGoalFireRecoveryDisposition::Conflict => "conflict",
        };
        assert_eq!(
            disposition, case.expected_disposition,
            "Lean case {}",
            case.name
        );
        assert_eq!(
            actual.recovered_request_id, case.expected_recovered_request_id,
            "Lean case {}",
            case.name
        );
        assert_eq!(
            actual.checkpointable, case.expected_checkpointable,
            "Lean case {}",
            case.name
        );
        if !case.durable_goal_present
            && !case.creation_claim_present
            && case.request_binding_matches
        {
            assert!(
                actual.checkpointable,
                "Lean case {} lost a matching request when goal metadata was deleted",
                case.name
            );
        }
        if case.request_present && !case.request_binding_matches {
            assert_eq!(
                disposition, "conflict",
                "Lean case {} accepted a mismatched binding",
                case.name
            );
        }
    }
}

#[test]
fn generated_goal_submission_cases_fence_atomic_visibility() {
    gents_lean_vocab::families::contracts_goal_submission::replay(|case| {
        let actual = goal_submission_step(
            GoalSubmissionState {
                durable_goal: case.state.durable_goal,
                runnable_request: case.state.runnable_request,
                staged_goal: case.state.staged_goal,
                staged_request: case.state.staged_request,
            },
            case.action,
        );
        assert!(
            !actual.runnable_request || actual.durable_goal,
            "Lean case {} exposed a runnable request without its durable goal",
            case.name
        );
        gents_lean_vocab::families::contracts_goal_submission::SubmissionState {
            durable_goal: actual.durable_goal,
            runnable_request: actual.runnable_request,
            staged_goal: actual.staged_goal,
            staged_request: actual.staged_request,
        }
    });
}

#[test]
fn generated_goal_continuation_materialization_cases_fence_restart_idempotency() {
    gents_lean_vocab::families::contracts_goal_continuation_materialization::replay::<
        String,
        GoalContinuationPhase,
    >(|case| {
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
        gents_lean_vocab::families::contracts_goal_continuation_materialization::ContinuationMaterializationOutput {
            phase: goal_continuation_materialization_step(case.phase, action),
        }
    });
}
