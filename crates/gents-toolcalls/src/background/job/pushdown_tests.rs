use crate::background::job::admit;
use crate::background::job::admit::{is_stalled, Refusal};
use crate::background::job::control::{self, KillCause};
use crate::background::job::dispatch::{host_pin, split_address, unique_handle, Delivery};
use crate::background::job::driver;
use crate::background::job::recovery::{
    step_invocation_allowed, tick_action, RecoveryFacts, TickAction,
};
use crate::plugin::job_calls;
use crate::test_support::plugin_job::*;
use gents_lean_vocab::{
    lean_plugin_job_adoption_cases, lean_plugin_job_control_cases, LeanPluginJobAdoptionCase,
    LeanPluginJobControlCase,
};
use gents_lean_vocab::families::plugin_job_recoveries::PluginJobRow;
use gents_lean_vocab::lean_recovery_sweep_cases;
use gents_protocol::job::{JobControlKind, JobControlOutcome, JobState, JobStepOutput};
use std::collections::BTreeSet;

const PLUGIN_JOB_SWEEP_ID: &str = "tool_call_lifecycle_recover_plugin_job_rows";

/// Lean `admitStep` and `settleStep` through the production admission owner:
/// the refusal code, the admitted row, the state a refused row fails closed
/// into, and the children the kill reaches.
#[test]
fn generated_plugin_job_admission_cases_drive_admit_step() {
    let mut names = BTreeSet::new();
    for case in gents_lean_vocab::families::plugin_job_admission::cases() {
        let name = case.input.name.clone();
        assert!(names.insert(name.clone()), "duplicate {name}");
        let decoded_context: gents_lean_vocab::LeanPluginJobContext =
            serde_json::from_value(case.input.context.0.clone()).unwrap();
        let decoded_job: gents_lean_vocab::LeanPluginJobRow =
            serde_json::from_value(case.input.job.0.clone()).unwrap();
        let decoded_output: gents_lean_vocab::LeanPluginJobOutput =
            serde_json::from_value(case.input.output.0.clone()).unwrap();
        let context = context(&decoded_context, &decoded_job, &decoded_output);
        let job = row(&decoded_job);
        let mut output = output(&decoded_output);
        if let Some(trace) = &case.input.trace {
            let JobStepOutput::Effects { effects } = &mut output else {
                panic!("{name}: wire trace requires spawn effects");
            };
            assert_eq!(effects.spawn.len(), trace.spawn_wires.len(), "{name}");
            for (spawn, wire) in effects.spawn.iter_mut().zip(&trace.spawn_wires) {
                spawn.target.clone_from(wire);
            }
        }
        let (admitted, code, dynamic_code_disabled, admitted_job, mut killed, settled) =
            match crate::background::job::admit::admit_step(&context, &job, &output) {
                Ok(admitted) => (
                    true,
                    None,
                    false,
                    Some(gents_lean_vocab::family::Json(row_json(&admitted))),
                    Vec::new(),
                    admitted,
                ),
                Err(refusal) => {
                    use crate::background::job::admit::PolicyDenialDetail;
                    match name.as_str() {
                        "dynamic_check_spawn_refused" => assert_eq!(
                            refusal.detail,
                            Some(PolicyDenialDetail::DynamicCheck),
                            "{name}"
                        ),
                        "caller_target_unconfigured_refused" => assert_eq!(
                            refusal.detail,
                            Some(PolicyDenialDetail::TargetUnavailable),
                            "{name}"
                        ),
                        _ => {}
                    }
                    (
                        false,
                        refusal.code().map(str::to_owned),
                        refusal.refusal
                            == crate::background::job::admit::Refusal::DynamicCodeDisabled,
                        None,
                        refusal.kills().to_vec(),
                        refusal.closed_row().clone(),
                    )
                }
            };
        killed.sort_unstable();
        let recovery = case.input.trace.as_ref().map(|trace| {
            let durable = match trace.interruption.as_str() {
                "before_kill" if !admitted => &job,
                "before_kill" | "after_kill" => &settled,
                other => panic!("unknown interruption {other}"),
            };
            let facts = crate::background::job::recovery::facts_from_durable_read(
                durable,
                durable.state == JobState::Running,
                durable.pin.as_ref().expect("wire trace has a durable pin"),
                crate::background::job::recovery::DurableRead {
                    session_stopped: trace.read.session_stopped,
                    now_millis: trace.read.now_millis,
                    available: if trace.read.available {
                        Ok(())
                    } else {
                        Err(admit::Refusal::NotAvailable)
                    },
                    progress_revision: trace.read.progress_revision,
                    in_flight: trace.read.step_in_flight,
                },
            );
            let before = facts;
            let (_, after) = tick_row(before.clone());
            gents_lean_vocab::families::plugin_job_admission::TraceRows {
                before: gents_lean_vocab::family::recode(&observed_row(&before)),
                after: gents_lean_vocab::family::recode(&observed_row(&after)),
            }
        });
        case.check(gents_lean_vocab::families::plugin_job_admission::AdmissionOutput {
            admitted,
            code,
            dynamic_code_disabled,
            job: admitted_job,
            row_state: serde_json::to_value(settled.state)
                .unwrap()
                .as_str()
                .unwrap()
                .to_owned(),
            killed_retry_keys: killed,
            recovery,
        });
    }
}

/// Lean `adopt`, `adoptTerminal`, `hostPin`, `uniqueHandle` and `splitAddress`
/// through the production adoption owner: the written pin, the adopted call,
/// inline delivery with no Continuation for a pinned terminal, and the
/// receipt compare-and-set that commits once and then conflicts.
#[test]
fn generated_plugin_job_adoption_cases_drive_adopt() {
    let cases = lean_plugin_job_adoption_cases();
    assert_eq!(
        cases.len(),
        12,
        "Lean pins twelve adoption rows in `adoption_regressions`"
    );
    let mut names = BTreeSet::new();
    for case in cases {
        assert!(names.insert(case.name()), "duplicate {}", case.name());
        match case {
            LeanPluginJobAdoptionCase::Step0 {
                name,
                tool_call: call,
                context: ctx,
                job,
                output: out,
                expected,
            } => {
                let call = tool_call(call);
                match crate::background::job::dispatch::adopt(
                    &context(ctx, job, out),
                    &call,
                    &row(job),
                    &output(out),
                ) {
                    Ok(adopted) => {
                        assert!(expected.admitted, "{name}: adopted a refused step");
                        assert_eq!(expected.code, None, "{name}");
                        assert_eq!(
                            Some(&adopted.row),
                            expected.job.as_ref().map(row).as_ref(),
                            "{name}: adopted row"
                        );
                        assert_eq!(
                            adopted.call,
                            tool_call(&expected.tool_call),
                            "{name}: adopted call"
                        );
                        assert_eq!(
                            delivery_spelling(adopted.delivery),
                            expected.delivery.as_deref(),
                            "{name}: delivery"
                        );
                        assert_eq!(
                            adopted.delivery == Some(Delivery::Continuation),
                            expected.continuation_appended,
                            "{name}: continuation"
                        );
                        let (terminal, late) = adopted.receipt_decisions();
                        let receipt = expected
                            .receipt
                            .as_ref()
                            .unwrap_or_else(|| panic!("{name}: adopted row without receipt"));
                        assert_eq!(
                            (terminal.spelling(), late.spelling()),
                            (
                                receipt.terminal_decision.as_str(),
                                receipt.late_decision.as_str()
                            ),
                            "{name}: receipt decisions"
                        );
                    }
                    Err(refusal) => {
                        assert!(!expected.admitted, "{name}: refused an adopted step");
                        assert_eq!(refusal.code(), expected.code.as_deref(), "{name}");
                        assert!(expected.job.is_none(), "{name}");
                        assert_eq!(
                            call,
                            tool_call(&expected.tool_call),
                            "{name}: a refused step leaves the call"
                        );
                        assert_eq!(expected.delivery, None, "{name}");
                        assert!(!expected.continuation_appended, "{name}");
                        assert!(expected.receipt.is_none(), "{name}");
                    }
                }
            }
            LeanPluginJobAdoptionCase::HostPin {
                name,
                claimed,
                facts,
                expected,
            } => {
                assert_eq!(
                    host_pin(&guest_of(claimed), &host_facts(facts)),
                    pin(&expected.pin),
                    "{name}: host pin"
                );
            }
            LeanPluginJobAdoptionCase::Handle {
                name,
                taken,
                handle,
                expected,
            } => {
                assert_eq!(
                    unique_handle(taken, handle),
                    expected.handle,
                    "{name}: handle"
                );
            }
            LeanPluginJobAdoptionCase::Address {
                name,
                address,
                expected,
            } => {
                assert_eq!(
                    split_address(address),
                    expected
                        .split
                        .as_ref()
                        .map(|s| (s.session_id.clone(), s.tool_call_id.clone())),
                    "{name}: address split"
                );
            }
        }
    }
}

/// Lean `Control.kill`, `resumeValid`, `resumePin`, `echo`, `rootCallOf`,
/// `resultReadAuthorized`, `resultOffered` and `onInvocationFailure` through
/// the pure decision cores that sit beside the frozen async owners
/// `plugin_job::control::{control, kill, validate_resume}`,
/// `plugin::job_calls::result` and `plugin_job::driver::on_step_failure`,
/// which call them: the ended row and the killed children, the
/// resume pin with its echo and root call, the `job_calls.result` read scope
/// and offer, and the third consecutive step failure.
#[test]
fn generated_plugin_job_control_cases_drive_control_owners() {
    let cases = lean_plugin_job_control_cases();
    assert_eq!(
        cases.len(),
        23,
        "Lean pins five kill, six resume, six result-read, three result-offer and three step-failure rows in `control_regressions`"
    );
    let mut names = BTreeSet::new();
    for case in cases {
        assert!(names.insert(case.name()), "duplicate {}", case.name());
        match case {
            LeanPluginJobControlCase::Kill {
                name,
                job,
                observations: observed,
                control: kind,
                expected,
            } => {
                let observed = observations(observed);
                let observe = |key: &str| observed[key];
                let job = row(job);
                let cause = match kind.as_str() {
                    "stop" => KillCause::Stop,
                    "pause" => KillCause::Pause,
                    "deadline" => KillCause::Deadline,
                    other => panic!("{name}: unknown Lean kill cause {other}"),
                };
                let outcome = control::kill_decision(&job, cause, &observe);
                assert_eq!(outcome.won, expected.won, "{name}: terminal winner");
                assert_eq!(
                    outcome.row.state,
                    spelled::<JobState>(&expected.row_state),
                    "{name}: row state"
                );
                assert_eq!(
                    by_retry_key(outcome.killed),
                    kills(&expected.kill),
                    "{name}: killed children"
                );
                let requested = match kind.as_str() {
                    "stop" => Some(JobControlKind::Stop),
                    "pause" => Some(JobControlKind::Pause),
                    _ => None,
                };
                if let Some(requested) = requested {
                    assert_eq!(
                        control::control_outcome(&job, requested, &observe),
                        if expected.won {
                            JobControlOutcome::Applied {
                                state: spelled(&expected.job_state),
                                killed_children: expected.kill.len() as u32,
                            }
                        } else {
                            JobControlOutcome::AlreadyTerminal {
                                state: spelled(&expected.job_state),
                            }
                        },
                        "{name}: control outcome"
                    );
                }
            }
            LeanPluginJobControlCase::Resume {
                name,
                caller,
                owner,
                same_plugin,
                root,
                prior,
                guest_pin: guest,
                facts,
                resume: block,
                expected,
                ..
            } => {
                let (caller, owner) = (process_scope(caller), process_scope(owner));
                let (root, prior) = (row(root), row(prior));
                let (guest, facts, block) = (guest_pin(guest), host_facts(facts), resume(block));
                let valid = admit::resume_valid(
                    &caller,
                    &owner,
                    *same_plugin,
                    &prior,
                    &guest,
                    &facts,
                    &block,
                );
                assert_eq!(valid, expected.valid, "{name}: resume validity");
                if valid {
                    assert_eq!(
                        Some(admit::resume_pin(&root, &prior, &guest, &facts)),
                        expected.pin.as_ref().map(pin),
                        "{name}: written pin"
                    );
                    assert_eq!(
                        control::resume_echo(&block),
                        control_echo(&expected.control),
                        "{name}: control echo"
                    );
                    assert_eq!(
                        admit::root_call_of(&root),
                        root_call(&expected.root_call),
                        "{name}: echoed root call"
                    );
                } else {
                    assert!(expected.pin.is_none(), "{name}");
                }
            }
            LeanPluginJobControlCase::ResultRead {
                name,
                caller,
                owner,
                same_plugin,
                terminal,
                expected,
            } => {
                assert_eq!(
                    job_calls::result_read(
                        &process_scope(caller),
                        &process_scope(owner),
                        *same_plugin,
                        *terminal
                    ),
                    expected.authorized,
                    "{name}: result read"
                );
            }
            LeanPluginJobControlCase::ResultOffer {
                name,
                invocation,
                expected,
            } => {
                assert_eq!(
                    job_calls::result_offer(&stamp_context(invocation)),
                    expected.offered,
                    "{name}: result offer"
                );
            }
            LeanPluginJobControlCase::StepFailure {
                name,
                job,
                observations: observed,
                expected,
                ..
            } => {
                let observed = observations(observed);
                let observe = |key: &str| observed[key];
                let (outcome, refusal) = driver::step_failure_decision(&row(job), &observe);
                let failed = &outcome.row;
                assert_eq!(
                    failed.state,
                    spelled::<JobState>(&expected.row_state),
                    "{name}: row state"
                );
                assert_eq!(
                    failed.consecutive_failures, expected.consecutive_failures,
                    "{name}: failure count"
                );
                assert_eq!(
                    by_retry_key(outcome.killed),
                    kills(&expected.kill),
                    "{name}: killed children"
                );
                assert_eq!(
                    refusal.as_ref().and_then(|r| r.code()),
                    expected.code.as_deref(),
                    "{name}: refusal code"
                );
            }
        }
    }
}

/// One tick of the row: the owner's `tick_action` is the first application of
/// Lean's `pluginJobRecover`; a `Step` receives every terminal, and the stall
/// rule over that step's output (`is_stalled`, the admission owner) is the
/// second. Returns the owner action and the row after it.
pub(super) fn tick_row(row: RecoveryFacts) -> (TickAction, RecoveryFacts) {
    let action = tick_action(&row);
    let after = match action {
        TickAction::Untouched => row,
        TickAction::Kill(_) => RecoveryFacts {
            running: false,
            ..row
        },
        TickAction::Step => {
            let observed = row.terminal_children;
            let stalled = is_stalled(row.live, observed, row.terminal_children, row.step_new);
            RecoveryFacts {
                running: !stalled,
                observed,
                stepped: true,
                ..row
            }
        }
    };
    (action, after)
}

fn facts_of(row: &PluginJobRow) -> RecoveryFacts {
    RecoveryFacts {
        running: row.running,
        session_stopped: row.session_stopped,
        terminal_children: u32::try_from(row.terminal_children).unwrap(),
        observed: u32::try_from(row.observed).unwrap(),
        live: u32::try_from(row.live).unwrap(),
        step_new: u32::try_from(row.step_new).unwrap(),
        deadline_expired: row.deadline_expired,
        available: if row.available {
            Ok(())
        } else {
            Err(Refusal::NotAvailable)
        },
        stepped: row.stepped,
        in_flight: row.step_in_flight,
    }
}

fn observed_row(row: &RecoveryFacts) -> PluginJobRow {
    PluginJobRow {
        running: row.running,
        session_stopped: row.session_stopped,
        terminal_children: u64::from(row.terminal_children),
        observed: u64::from(row.observed),
        live: u64::from(row.live),
        step_new: u64::from(row.step_new),
        deadline_expired: row.deadline_expired,
        available: row.available.is_ok(),
        stepped: row.stepped,
        step_in_flight: row.in_flight,
    }
}

/// Lean `pluginJobRecoverySweep` ticks (`pluginJobStale`, then
/// `pluginJobRecover ∘ pluginJobRecover`) through the owner's first-application
/// decision `plugin_job::recovery::tick_action`, which
/// `redrive_running_jobs` applies to each running job row: a stale row is
/// killed or re-driven one step, and a row that is not stale is untouched.
/// Lean's measures stay with its own `recovery_regressions`.
#[tokio::test]
async fn generated_plugin_job_recovery_cases_drive_redrive_owner() {
    let mut names = Vec::new();
    for case in gents_lean_vocab::families::plugin_job_recoveries::cases() {
        let name = case.input.name.clone();
        let before = facts_of(&case.input.before);
        let (action, after) = tick_row(before.clone());
        if before.session_stopped {
            assert_eq!(
                action,
                TickAction::Kill(crate::background::job::control::KillCause::Stop),
                "{name}"
            );
        }
        case.check(
            gents_lean_vocab::families::plugin_job_recoveries::RecoveryOutput {
                stale: !matches!(action, TickAction::Untouched),
                row: observed_row(&after),
                step_commit_allowed: step_invocation_allowed(true, before.session_stopped),
                step_invocation_allowed: step_invocation_allowed(
                    before.running,
                    before.session_stopped,
                ),
            },
        );
        names.push(name);
    }
    let sweep_cases = lean_recovery_sweep_cases()
        .iter()
        .filter(|case| case.sweep_id == PLUGIN_JOB_SWEEP_ID)
        .map(|case| case.name.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        sweep_cases, names,
        "the registered sweep's witnesses are the plugin-job tick rows"
    );
}
