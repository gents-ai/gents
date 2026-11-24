use crate::goal::{
    decide_goal_continuation, decide_model_goal_create, gate_goal_continuation,
    goal_continuation_materialization_step, goal_submission_step, next_goal_infrastructure_retries,
    observe_goal_agent_readiness, GoalAction, GoalAgentObservation, GoalAgentReadiness,
    GoalContinuationAction, GoalContinuationFacts, GoalContinuationPhase, GoalCreateDisposition,
    GoalCreateRequest, GoalCreationFingerprint, GoalDecision, GoalFailureCause, GoalGatedDecision,
    GoalRequestTerminal, GoalState, GoalSubmissionState,
};
use gents_protocol::goal_status::GoalStatus;

#[test]
fn generated_goal_decision_cases_fence_runtime_controller() {
    gents_lean_vocab::families::contracts_goal_decision::replay(|case| {
        decide_goal_continuation(GoalContinuationFacts {
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
            max_infrastructure_retries: i64::try_from(case.max_infrastructure_retries)
                .expect("modeled limit fits i64"),
        })
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
            max_infrastructure_retries: i64::try_from(case.max_infrastructure_retries)
                .expect("modeled limit fits i64"),
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
            wrapup_requested: case.pre_wrapup_requested,
            wrapup_completed: case.pre_wrapup_completed,
        };
        let action = match case.action.as_str() {
            "pause" => GoalAction::Pause,
            "resume" => GoalAction::Resume,
            "stop" => GoalAction::Stop,
            "complete" => GoalAction::Complete,
            "block" => GoalAction::Block,
            "usage_limit" => GoalAction::UsageLimit,
            "budget_exhausted" => GoalAction::BudgetExhausted,
            "wrapup_finished" => GoalAction::WrapupFinished,
            "wrapup_abandoned" => GoalAction::WrapupAbandoned,
            other => panic!("unknown action {other:?} in Lean case {}", case.name),
        };
        let stepped = pre.step(action);
        let post = stepped.unwrap_or(pre);
        gents_lean_vocab::families::contracts_goal_transition::GoalTransitionOutput {
            accepted: stepped.is_some(),
            status: post.status,
            wrapup_requested: post.wrapup_requested,
            wrapup_completed: post.wrapup_completed,
        }
    });
}

#[test]
fn generated_goal_message_resume_cases_fence_message_resume() {
    gents_lean_vocab::families::contracts_goal_message_resume::replay::<GoalStatus>(|case| {
        let pre = GoalState {
            status: case.status,
            wrapup_requested: case.wrapup_requested,
            wrapup_completed: case.wrapup_completed,
        };
        let resumed = pre.resume_on_message();
        let post = resumed.unwrap_or(pre);
        gents_lean_vocab::families::contracts_goal_message_resume::GoalMessageResumeOutput {
            resumed: resumed.is_some(),
            status: post.status,
            wrapup_requested: post.wrapup_requested,
            wrapup_completed: post.wrapup_completed,
        }
    });
}

#[test]
fn generated_goal_budget_cases_fence_budget_reached() {
    gents_lean_vocab::families::contracts_goal_budget::replay(|case| {
        let budget = case
            .token_budget
            .map(|budget| i64::try_from(budget).expect("modeled budgets fit i64"));
        let used = i64::try_from(case.tokens_used).expect("modeled usage fits i64");
        gents_lean_vocab::families::contracts_goal_budget::GoalBudgetOutput {
            reached: crate::goal::goal_budget_reached(budget, used),
        }
    });
}

#[test]
fn generated_goal_progress_cases_fence_the_no_progress_pause() {
    use crate::goal::progress::{progress_step, CallPrint, ProgressRecord};
    use gents_lean_vocab::families::contracts_goal_progress as family;
    let prints = |calls: &[family::CallPrintWire]| {
        calls
            .iter()
            .map(|call| CallPrint {
                tool: call.tool.clone(),
                arguments: call.arguments.clone(),
                result: call.result.clone(),
            })
            .collect::<Vec<_>>()
    };
    family::replay(|case| {
        let record = ProgressRecord {
            streak: case.streak,
            previous: prints(&case.previous),
        };
        let (next, pause) = progress_step(
            &record,
            prints(&case.calls),
            case.goal_state_changed,
            case.live_deadline,
            case.now,
        );
        family::GoalProgressOutput {
            streak: next.streak,
            pause,
        }
    });
}

#[test]
fn generated_goal_on_blocked_cases_fence_the_blocked_report() {
    use gents_lean_vocab::families::contracts_goal_on_blocked as family;
    family::replay(|case| {
        let policy = gents_protocol::goal::OnBlocked::parse(&case.on_blocked)
            .expect("modeled settings parse");
        family::GoalOnBlockedOutput {
            outcome: crate::goal::stop_policy::on_blocked_report(policy, case.confirming)
                .as_str()
                .to_owned(),
        }
    });
}

#[test]
fn generated_goal_effective_budget_cases_fence_the_users_ceiling() {
    use gents_lean_vocab::families::contracts_goal_effective_budget as family;
    let signed = |value: u64| i64::try_from(value).expect("modeled budgets fit i64");
    family::replay(|case| {
        let policy = gents_protocol::goal::GoalStopPolicy {
            token_ceiling: case.token_ceiling.map(signed),
            lifted: case.lifted,
            ..Default::default()
        };
        family::GoalEffectiveBudgetOutput {
            budget: crate::goal::stop_policy::effective_goal_budget(
                &policy,
                case.goal_budget.map(signed),
            )
            .map(|budget| u64::try_from(budget).expect("budgets are positive")),
        }
    });
}

fn family_budget(budget: Option<&gents_lean_vocab::family::Json>) -> Option<i128> {
    budget.map(|budget| {
        budget
            .0
            .as_i64()
            .map(i128::from)
            .or_else(|| budget.0.as_u64().map(i128::from))
            .expect("a modeled token budget is an integer")
    })
}

#[test]
fn generated_goal_create_cases_fence_authority_and_idempotency() {
    gents_lean_vocab::families::contracts_goal_create::replay::<GoalCreateDisposition>(|case| {
        let request = GoalCreateRequest {
            caller: case.caller.clone(),
            current_session: case.current_session.clone(),
            requested_owner: case.requested_owner.clone(),
            requested_session: case.requested_session.clone(),
            objective: case.objective.clone(),
            objective_nonempty: case.objective_nonempty,
            token_budget: family_budget(case.token_budget.as_ref()),
            limits: gents_protocol::goal::GoalLimits {
                max_infrastructure_retries: case.max_infrastructure_retries,
            },
            goal_tools: case.goal_tools,
            goal_create: case.goal_create,
        };
        let existing = case
            .existing_fingerprint
            .as_ref()
            .map(|existing| GoalCreationFingerprint {
                owner: existing.owner.clone(),
                session: existing.session.clone(),
                objective: existing.objective.clone(),
                token_budget: family_budget(existing.token_budget.as_ref()),
                limits: gents_protocol::goal::GoalLimits {
                    max_infrastructure_retries: existing.max_infrastructure_retries,
                },
            });
        decide_model_goal_create(&request, existing.as_ref())
    });
}

#[test]
fn generated_task_goal_cases_fence_fire_identity() {
    gents_lean_vocab::families::contracts_task_goal_publication_identity::replay(|case| {
        let identity =
            crate::goal::task_goal_fire_identity(&case.node_did, &case.task_id, &case.fire_key);
        gents_lean_vocab::families::contracts_task_goal_publication_identity::TaskGoalPublicationIdentity {
            session_id: Some(identity.session_id),
            request_id: Some(identity.request_id),
            retry_key: Some(identity.retry_key),
        }
    });
}

#[test]
fn generated_task_goal_recovery_cases_fence_request_witness() {
    gents_lean_vocab::families::contracts_task_goal_recovery::replay(|case| {
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
        let actual = crate::goal::decide_task_goal_fire_recovery(&expected, observed.as_ref());
        gents_lean_vocab::families::contracts_task_goal_recovery::TaskGoalRecoveryOutput {
            request_binding_matches: observed.as_ref() == Some(&expected),
            disposition: actual.disposition,
            recovered_request_id: actual.recovered_request_id,
            checkpointable: actual.checkpointable,
        }
    });
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
