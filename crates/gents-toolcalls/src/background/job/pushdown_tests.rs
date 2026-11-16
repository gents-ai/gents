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
use gents_lean_vocab::families::plugin_job_recoveries::PluginJobRow;
use gents_lean_vocab::lean_recovery_sweep_cases;
use gents_protocol::job::{JobControlKind, JobControlOutcome, JobResume, JobState, JobStepOutput};
use std::collections::{BTreeMap, BTreeSet};

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
                    Some(gents_lean_vocab::family::Json(row_wire(&admitted))),
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
        case.check(
            gents_lean_vocab::families::plugin_job_admission::AdmissionOutput {
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
            },
        );
    }
}

fn decoded<T>(value: &gents_lean_vocab::family::Json) -> T
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_value(value.0.clone()).expect("modeled plugin-job wire decodes")
}

/// Lean `adopt` and `adoptTerminal` through the production adoption owner:
/// the adopted row and call, inline delivery with no Continuation for a
/// pinned terminal, and the receipt compare-and-set that commits once and
/// then conflicts.
#[test]
fn plugin_job_step0_adoption_follows_the_lean_model() {
    use gents_lean_vocab::family::Json;

    gents_lean_vocab::families::plugin_job_adoption_step0::replay(|input| {
        let ctx: gents_lean_vocab::LeanPluginJobContext = decoded(&input.context);
        let job: gents_lean_vocab::LeanPluginJobRow = decoded(&input.job);
        let out: gents_lean_vocab::LeanPluginJobOutput = decoded(&input.output);
        let call = tool_call(
            &input.tool_call.state,
            &input.tool_call.operation,
            &input.tool_call.await_mode,
        );
        let call_wire = |call: &crate::background::job::dispatch::ToolCallView| {
            let (state, operation, await_mode) = tool_call_wire(call);
            gents_lean_vocab::families::plugin_job_adoption_step0::ToolCallWire {
                state,
                operation,
                await_mode,
            }
        };
        Some(
            match crate::background::job::dispatch::adopt(
                &context(&ctx, &job, &out),
                &call,
                &row(&job),
                &output(&out),
            ) {
                Ok(adopted) => {
                    let (terminal, late) = adopted.receipt_decisions();
                    gents_lean_vocab::families::plugin_job_adoption_step0::Step0Output {
                        admitted: true,
                        code: None,
                        job: Some(Json(row_wire(&adopted.row))),
                        tool_call: call_wire(&adopted.call),
                        delivery: delivery_spelling(adopted.delivery).map(str::to_owned),
                        continuation_appended: adopted.delivery == Some(Delivery::Continuation),
                        receipt: Some(
                            gents_lean_vocab::families::plugin_job_adoption_step0::ReceiptWire {
                                terminal_decision: terminal.spelling().to_owned(),
                                late_decision: late.spelling().to_owned(),
                            },
                        ),
                    }
                }
                Err(refusal) => {
                    gents_lean_vocab::families::plugin_job_adoption_step0::Step0Output {
                        admitted: false,
                        code: refusal.code().map(str::to_owned),
                        job: None,
                        tool_call: call_wire(&call),
                        delivery: None,
                        continuation_appended: false,
                        receipt: None,
                    }
                }
            },
        )
    });
}

/// Lean `hostPin` through the production pin writer: only the guest fields
/// of a claimed pin survive.
#[test]
fn plugin_job_host_pin_follows_the_lean_model() {
    gents_lean_vocab::families::plugin_job_adoption_host_pin::replay(|input| {
        let claimed: gents_lean_vocab::LeanPluginJobPin = decoded(&input.claimed);
        let facts: gents_lean_vocab::LeanPluginJobHostFacts = decoded(&input.facts);
        Some(gents_lean_vocab::family::Json(pin_wire(&host_pin(
            &guest_of(&claimed),
            &host_facts(&facts),
        ))))
    });
}

#[test]
fn plugin_job_unique_handle_follows_the_lean_model() {
    gents_lean_vocab::families::plugin_job_adoption_handle::replay(|input| {
        unique_handle(&input.taken, &input.handle)
    });
}

#[test]
fn plugin_job_address_split_follows_the_lean_model() {
    gents_lean_vocab::families::plugin_job_adoption_address::replay(|input| {
        split_address(&input.address).map(|(session_id, tool_call_id)| {
            gents_lean_vocab::families::plugin_job_adoption_address::SplitWire {
                session_id,
                tool_call_id,
            }
        })
    });
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
