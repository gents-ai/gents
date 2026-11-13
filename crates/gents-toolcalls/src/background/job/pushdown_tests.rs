use crate::background::job::admit;
use crate::background::job::admit::{is_stalled, Refusal};
use crate::background::job::control::{self, KillCause, KillObservation};
use crate::background::job::dispatch::{host_pin, split_address, unique_handle, Delivery};
use crate::background::job::driver;
use crate::background::job::recovery::{
    step_invocation_allowed, tick_action, RecoveryFacts, TickAction,
};
use crate::plugin::job_calls;
use crate::test_support::plugin_job::*;
use gents_lean_vocab::{
    lean_plugin_job_admission_cases, lean_plugin_job_adoption_cases, LeanPluginJobAdoptionCase,
};
use gents_lean_vocab::{lean_recovery_sweep_cases, LeanPluginJobRecoveryRow};
use gents_protocol::job::{JobControlKind, JobControlOutcome, JobResume, JobState, JobStepOutput};
use std::collections::{BTreeMap, BTreeSet};

const PLUGIN_JOB_SWEEP_ID: &str = "tool_call_lifecycle_recover_plugin_job_rows";

/// Lean `admitStep` and `settleStep` through the production admission owner:
/// the refusal code, the admitted row, the state a refused row fails closed
/// into, and the children the kill reaches.
#[test]
fn generated_plugin_job_admission_cases_drive_admit_step() {
    let cases = lean_plugin_job_admission_cases();
    assert_eq!(
        cases.len(),
        222,
        "Lean supplies 30 admission regressions and 192 interrupted wire-to-recovery traces"
    );
    let mut names = BTreeSet::new();
    for case in cases {
        assert!(names.insert(case.name.as_str()), "duplicate {}", case.name);
        let expected = &case.expected;
        let expected_job = expected.job.as_ref().map(row);
        let context = context(&case.context, &case.job, &case.output);
        let job = row(&case.job);
        let mut output = output(&case.output);
        if let Some(trace) = &case.wire_trace {
            let JobStepOutput::Effects { effects } = &mut output else {
                panic!("{}: wire trace requires spawn effects", case.name);
            };
            assert_eq!(
                effects.spawn.len(),
                trace.spawn_wires.len(),
                "{}",
                case.name
            );
            for (spawn, wire) in effects.spawn.iter_mut().zip(&trace.spawn_wires) {
                spawn.target.clone_from(wire);
            }
        }
        let expected_state: JobState = spelled(&expected.row_state);
        let mut expected_kills: Vec<&str> = expected
            .kill
            .iter()
            .map(|kill| kill.retry_key.as_str())
            .collect();
        expected_kills.sort_unstable();
        let settled = match crate::background::job::admit::admit_step(&context, &job, &output) {
            Ok(admitted) => {
                assert!(expected.admitted, "{}: admitted a refused step", case.name);
                assert!(!expected.dynamic_code_disabled, "{}", case.name);
                assert_eq!(expected.code, None, "{}", case.name);
                assert_eq!(
                    Some(&admitted),
                    expected_job.as_ref(),
                    "{}: admitted row",
                    case.name
                );
                assert_eq!(admitted.state, expected_state, "{}", case.name);
                assert!(expected_kills.is_empty(), "{}: kill", case.name);
                admitted
            }
            Err(refusal) => {
                assert!(
                    !expected.admitted,
                    "{}: refused an admitted step",
                    case.name
                );
                assert_eq!(
                    refusal.code(),
                    expected.code.as_deref(),
                    "{}: refusal code",
                    case.name
                );
                use crate::background::job::admit::PolicyDenialDetail;
                match case.name.as_str() {
                    "dynamic_check_spawn_refused" => assert_eq!(
                        refusal.detail,
                        Some(PolicyDenialDetail::DynamicCheck),
                        "{}",
                        case.name
                    ),
                    "caller_target_unconfigured_refused" => assert_eq!(
                        refusal.detail,
                        Some(PolicyDenialDetail::TargetUnavailable),
                        "{}",
                        case.name
                    ),
                    _ => {}
                }
                assert_eq!(
                    refusal.refusal == crate::background::job::admit::Refusal::DynamicCodeDisabled,
                    expected.dynamic_code_disabled,
                    "{}: dynamic-source refusal detail",
                    case.name
                );
                assert!(expected.job.is_none(), "{}", case.name);
                assert_eq!(
                    refusal.closed_row().state,
                    expected_state,
                    "{}: row state",
                    case.name
                );
                let mut kills: Vec<&str> = refusal.kills().iter().map(String::as_str).collect();
                kills.sort_unstable();
                assert_eq!(kills, expected_kills, "{}: killed children", case.name);
                refusal.closed_row().clone()
            }
        };
        if let Some(trace) = &case.wire_trace {
            let durable = match trace.interruption.as_str() {
                "before_kill" if !expected.admitted => &job,
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
            let before = gents_lean_vocab::LeanPluginJobRecoveryRow {
                running: facts.running,
                terminal_children: facts.terminal_children,
                observed: facts.observed,
                live: facts.live,
                session_stopped: facts.session_stopped,
                deadline_expired: facts.deadline_expired,
                available: facts.available.is_ok(),
                stepped: facts.stepped,
                step_in_flight: facts.in_flight,
                step_new: facts.step_new,
            };
            assert_eq!(
                before, trace.before,
                "{}: settlement-to-recovery projection",
                case.name
            );
            let (_, after) = tick_row(before);
            assert_eq!(
                after, trace.after,
                "{}: recovery of the actual settled row",
                case.name
            );
        }
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

fn observed_by_key<'a, I>(observed: I) -> BTreeMap<String, KillObservation>
where
    I: IntoIterator<Item = (&'a str, &'a str)>,
{
    observed
        .into_iter()
        .map(|(key, observation)| (key.to_owned(), spelled(observation)))
        .collect()
}

fn spelling<T>(value: &T) -> String
where
    T: serde::Serialize,
{
    serde_json::to_value(value)
        .unwrap()
        .as_str()
        .expect("a closed enum spells as a string")
        .to_owned()
}

/// Lean `Control.kill` through `control::kill_decision`, and for a stop or
/// pause the control answer `control::control_outcome` gives on the same row.
#[test]
fn plugin_job_kills_follow_the_lean_control_model() {
    gents_lean_vocab::families::plugin_job_control_kill::replay(|input| {
        let observed = observed_by_key(
            input
                .observations
                .iter()
                .map(|o| (o.retry_key.as_str(), o.observation.as_str())),
        );
        let observe = |key: &str| observed[key];
        let job = row(&gents_lean_vocab::family::recode(&input.job));
        let cause = match input.control.as_str() {
            "stop" => KillCause::Stop,
            "pause" => KillCause::Pause,
            "deadline" => KillCause::Deadline,
            other => panic!("{}: unknown Lean kill cause {other}", input.name),
        };
        let outcome = control::kill_decision(&job, cause, &observe);
        let requested = match input.control.as_str() {
            "stop" => Some(JobControlKind::Stop),
            "pause" => Some(JobControlKind::Pause),
            _ => None,
        };
        let job_state =
            requested.map(
                |requested| match control::control_outcome(&job, requested, &observe) {
                    JobControlOutcome::Applied {
                        state,
                        killed_children,
                    } => {
                        assert!(outcome.won, "{}: applied control wins the row", input.name);
                        assert_eq!(
                            killed_children as usize,
                            outcome.killed.len(),
                            "{}",
                            input.name
                        );
                        spelling(&state)
                    }
                    JobControlOutcome::AlreadyTerminal { state } => {
                        assert!(!outcome.won, "{}: a terminal row is not won", input.name);
                        spelling(&state)
                    }
                    other => panic!("{}: a pure control decision answered {other:?}", input.name),
                },
            );
        gents_lean_vocab::families::plugin_job_control_kill::KillOutput {
            row_state: spelling(&outcome.row.state),
            won: outcome.won,
            job_state,
            kill: by_retry_key(outcome.killed)
                .into_iter()
                .map(|(retry_key, action)| {
                    gents_lean_vocab::families::plugin_job_control_kill::KillEntryWire {
                        retry_key,
                        action: spelling(&action),
                    }
                })
                .collect(),
        }
    });
}

/// Lean `resumeValid`, `resumePin`, `echo` and `rootCallOf` through
/// `admit::resume_valid`, `admit::resume_pin`, `control::resume_echo` and
/// `admit::root_call_of`.
#[test]
fn plugin_job_resumes_follow_the_lean_control_model() {
    use gents_lean_vocab::family::recode;
    fn resume_wire(
        r: &JobResume,
    ) -> gents_lean_vocab::families::plugin_job_control_resume::ResumeWire {
        gents_lean_vocab::families::plugin_job_control_resume::ResumeWire {
            row_doc_id: r.row_doc_id.clone(),
            engine_current: r.engine.is_some(),
            answer: r.answer.clone(),
            budget: r.budget.map(u64::from),
        }
    }
    gents_lean_vocab::families::plugin_job_control_resume::replay(|input| {
        let (caller, owner) = (
            process_scope(&recode(&input.caller)),
            process_scope(&recode(&input.owner)),
        );
        let (root, prior) = (row(&recode(&input.root)), row(&recode(&input.prior)));
        let (guest, facts, block) = (
            guest_pin(&recode(&input.guest_pin)),
            host_facts(&recode(&input.facts)),
            resume(&recode(&input.resume)),
        );
        let valid = admit::resume_valid(
            &caller,
            &owner,
            input.same_plugin,
            &prior,
            &guest,
            &facts,
            &block,
        );
        let pin = valid.then(|| {
            let p = admit::resume_pin(&root, &prior, &guest, &facts);
            gents_lean_vocab::families::plugin_job_control_resume::PinWire {
                script_id: p.script_id,
                source_digest: p.source_digest,
                engine_digest: p.engine_digest,
                plugin_ref: p.plugin_ref,
                handle: p.handle,
                cwd: p.cwd,
                args_digest: p.args_digest,
                chain_root: p.chain_root_doc_id,
                origin: spelling(&p.origin),
                agent_budget: u64::from(p.agent_budget),
                adopted_at: p.adopted_at,
                resume_of: p.resume_of,
                replayed_from: p.replayed_from,
                ceiling_at_pin:
                    gents_lean_vocab::families::plugin_job_control_resume::CeilingWire {
                        targets: p.ceiling_at_pin.targets,
                        max_live: u64::from(p.ceiling_at_pin.max_live),
                        max_spawns: u64::from(p.ceiling_at_pin.max_spawns),
                        max_messages_per_slot: u64::from(p.ceiling_at_pin.max_messages_per_slot),
                        result_bytes: p.ceiling_at_pin.result_bytes,
                        max_wall_secs: p.ceiling_at_pin.max_wall_secs,
                    },
            }
        });
        let echo = control::resume_echo(&block);
        let root_call = admit::root_call_of(&root);
        gents_lean_vocab::families::plugin_job_control_resume::ResumeOutput {
            valid,
            pin,
            control: gents_lean_vocab::families::plugin_job_control_resume::ControlEchoWire {
                kind: spelling(&echo.kind),
                payload: echo.payload.as_ref().map(resume_wire),
            },
            root_call: gents_lean_vocab::families::plugin_job_control_resume::RootCallWire {
                argv: root_call.argv,
                args: root_call.args,
                agent_budget: root_call.agent_budget.map(u64::from),
            },
        }
    });
}

/// Lean `resultReadAuthorized` through `job_calls::result_read`.
#[test]
fn plugin_job_result_reads_follow_the_lean_control_model() {
    use gents_lean_vocab::family::recode;
    gents_lean_vocab::families::plugin_job_control_result_read::replay(|input| {
        gents_lean_vocab::families::plugin_job_control_result_read::ResultReadOutput {
            authorized: job_calls::result_read(
                &process_scope(&recode(&input.caller)),
                &process_scope(&recode(&input.owner)),
                input.same_plugin,
                input.terminal,
            ),
        }
    });
}

/// Lean `resultOffered` through `job_calls::result_offer`.
#[test]
fn plugin_job_result_offers_follow_the_lean_control_model() {
    gents_lean_vocab::families::plugin_job_control_result_offer::replay(|input| {
        gents_lean_vocab::families::plugin_job_control_result_offer::ResultOfferOutput {
            offered: job_calls::result_offer(&stamp_context(&input.invocation)),
        }
    });
}

/// Lean `onInvocationFailure` through `driver::step_failure_decision`.
#[test]
fn plugin_job_step_failures_follow_the_lean_control_model() {
    gents_lean_vocab::families::plugin_job_control_step_failure::replay(|input| {
        let observed = observed_by_key(
            input
                .observations
                .iter()
                .map(|o| (o.retry_key.as_str(), o.observation.as_str())),
        );
        let observe = |key: &str| observed[key];
        let (outcome, refusal) = driver::step_failure_decision(
            &row(&gents_lean_vocab::family::recode(&input.job)),
            &observe,
        );
        gents_lean_vocab::families::plugin_job_control_step_failure::StepFailureOutput {
            row_state: spelling(&outcome.row.state),
            consecutive_failures: u64::from(outcome.row.consecutive_failures),
            kill: by_retry_key(outcome.killed)
                .into_iter()
                .map(|(retry_key, action)| {
                    gents_lean_vocab::families::plugin_job_control_step_failure::KillEntryWire {
                        retry_key,
                        action: spelling(&action),
                    }
                })
                .collect(),
            code: refusal.as_ref().and_then(|r| r.code()).map(str::to_owned),
        }
    });
}

/// One tick of the row: the owner's `tick_action` is the first application of
/// Lean's `pluginJobRecover`; a `Step` receives every terminal, and the stall
/// rule over that step's output (`is_stalled`, the admission owner) is the
/// second. Returns the owner action and the row after it.
pub(super) fn tick_row(row: LeanPluginJobRecoveryRow) -> (TickAction, LeanPluginJobRecoveryRow) {
    let facts = RecoveryFacts {
        running: row.running,
        session_stopped: row.session_stopped,
        terminal_children: row.terminal_children,
        observed: row.observed,
        live: row.live,
        step_new: row.step_new,
        deadline_expired: row.deadline_expired,
        available: if row.available {
            Ok(())
        } else {
            Err(Refusal::NotAvailable)
        },
        stepped: row.stepped,
        in_flight: row.step_in_flight,
    };
    let action = tick_action(&facts);
    let after = match action {
        TickAction::Untouched => row,
        TickAction::Kill(_) => LeanPluginJobRecoveryRow {
            running: false,
            ..row
        },
        TickAction::Step => {
            let observed = row.terminal_children;
            let stalled = is_stalled(row.live, observed, row.terminal_children, row.step_new);
            LeanPluginJobRecoveryRow {
                running: !stalled,
                observed,
                stepped: true,
                ..row
            }
        }
    };
    (action, after)
}

fn observed_row(
    row: &LeanPluginJobRecoveryRow,
) -> gents_lean_vocab::families::plugin_job_recoveries::PluginJobRow {
    gents_lean_vocab::families::plugin_job_recoveries::PluginJobRow {
        running: row.running,
        session_stopped: row.session_stopped,
        terminal_children: u64::from(row.terminal_children),
        observed: u64::from(row.observed),
        live: u64::from(row.live),
        step_new: u64::from(row.step_new),
        deadline_expired: row.deadline_expired,
        available: row.available,
        stepped: row.stepped,
        step_in_flight: row.step_in_flight,
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
        let before: LeanPluginJobRecoveryRow = gents_lean_vocab::family::recode(&case.input.before);
        let (action, after) = tick_row(before);
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
