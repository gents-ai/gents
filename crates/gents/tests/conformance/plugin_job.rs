//! Generated plugin-job admission, adoption, control, key, authority, wire and
//! lineage rows checked against the native job owners. Every `expected` value comes from the Lean
//! owners (`admitStep`, `settleStep`, `adopt`, `adoptTerminal`, `hostPin`,
//! `uniqueHandle`, `splitAddress`, `Control.kill`, `resumeValid`, `resumePin`,
//! `echo`, `resultReadAuthorized`, `resultOffered`, `onInvocationFailure`, the
//! `SpawnKey` encoders and `decodeRetryKey?`, `originOf`, `provenanceOf`,
//! `recordAfterInstall` and `openingRequest`); the rows' inputs are translated
//! to the owners' native types and nothing is recomputed here.
//!
//! Wherever `gents_protocol::job` has the type, the translation builds that
//! type. Lean models only the presence or count of a rider, so a protocol field
//! the model does not carry (a prompt, a title, a write's body) is left empty.
//! Composed wire traces carry the actual resolution table and raw ceilings.
use crate::lean_vocab_test::{
    lean_plugin_job_admission_cases, lean_plugin_job_adoption_cases,
    lean_plugin_job_authority_cases, lean_plugin_job_control_cases, lean_plugin_job_key_cases,
    lean_plugin_job_lineage_cases, lean_plugin_job_wire_cases, LeanPluginJobAdoptionCase,
    LeanPluginJobAuthorityCase, LeanPluginJobCeiling, LeanPluginJobContext,
    LeanPluginJobControlCase, LeanPluginJobControlEcho, LeanPluginJobExtras, LeanPluginJobGuestPin,
    LeanPluginJobHostFacts, LeanPluginJobKill, LeanPluginJobObservation, LeanPluginJobOutput,
    LeanPluginJobPin, LeanPluginJobPolicy, LeanPluginJobProcessScope, LeanPluginJobRecord,
    LeanPluginJobResume, LeanPluginJobRootCall, LeanPluginJobRow, LeanPluginJobScope,
    LeanPluginJobTarget, LeanPluginJobToolCall, LeanPluginJobWireCase, LeanPluginJobWireKind,
};
use gents::pack::home_install::{record_after_install_via, recorded_provenance, InstalledVia};
use gents::pack::PackKind;
use gents::plugin::data::StampContext;
use gents::plugin::job_calls;
use gents::tool_surface::EndpointScope;
use gents::tools::background::job::admit::{
    self, HostFacts, RootCall, StepCall, StepChild, StepContext, StepRow,
};
use gents::tools::background::job::control::{self, KillAction, KillCause, KillObservation};
use gents::tools::background::job::dispatch::{
    host_pin, split_address, unique_handle, Delivery, ToolCallView,
};
use gents::tools::background::job::driver;
use gents::tools::background::job::origin::call_origin;
use gents::tools::background::job::settings::{
    job_block_admissible, policy_valid, wire_targets_from,
};
use gents::tools::background::status::ProcessControlScope;
use gents::tools::call::lifecycle::{AwaitMode, ToolCallState, ToolRowKind};
use gents_protocol::job::keys::{
    decode_job_retry_key, job_message_key, job_session_id, job_spawn_key,
};
use gents_protocol::job::{
    ChildOutputRequest, ChildOutputResponse, ChildOutputSlice, DataCall, DataOp, DataQuery,
    DataQueryResponse, DataWrite, JobBudget, JobCall, JobCeiling, JobControlEcho, JobControlKind,
    JobControlOutcome, JobControlRequest, JobEffects, JobFailure, JobGuestPin, JobInterrupt,
    JobMessage, JobOrigin, JobOutcome, JobPin, JobPolicy, JobProgress, JobReadError,
    JobResultRequest, JobResultResponse, JobResume, JobResumeEngine, JobScratch, JobSpawn,
    JobState, JobStepInput, JobStepOutput, JobSuspend, JobView, PluginDataDeclaration,
    PluginJobDeclaration, PluginJobSettings, RoundResult, StampedField,
};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// A closed Lean spelling decoded by the native enum's own serde names.
fn spelled<T>(spelling: &str) -> T
where
    T: DeserializeOwned,
{
    serde_json::from_value(serde_json::Value::String(spelling.to_owned()))
        .unwrap_or_else(|error| panic!("unknown Lean spelling {spelling:?}: {error}"))
}

fn target(t: &LeanPluginJobTarget) -> (String, String) {
    (t.node.clone(), t.agent.clone())
}

/// A target's wire spelling is its AgentTarget id, which the host resolves to
/// Lean's `(node, agent)` pair; spelled unlike the agent id so a host that
/// matched agent ids would refuse.
fn wire_id(t: &LeanPluginJobTarget) -> String {
    format!("{}@{}", t.agent, t.node)
}

fn scope(s: &LeanPluginJobScope) -> EndpointScope<(String, String), ()> {
    match s.kind.as_str() {
        "none" => EndpointScope::None,
        "all" => EndpointScope::All,
        "only" => EndpointScope::<(String, String), ()>::only_units(s.targets.iter().map(target)),
        other => panic!("unknown Lean endpoint scope {other}"),
    }
}

fn ceiling(c: &LeanPluginJobCeiling) -> JobCeiling {
    JobCeiling {
        targets: c.targets.clone(),
        max_live: c.max_live,
        max_spawns: c.max_spawns,
        max_messages_per_slot: c.max_messages_per_slot,
        result_bytes: c.result_bytes,
        max_wall_secs: c.max_wall_secs,
    }
}

fn host_facts(f: &LeanPluginJobHostFacts) -> HostFacts {
    HostFacts {
        engine_digest: f.engine_digest.clone(),
        plugin_ref: f.plugin_ref.clone(),
        cwd: f.cwd.clone(),
        args_digest: f.args_digest.clone(),
        chain_root: f.chain_root.clone(),
        origin: spelled(&f.origin),
        adopted_at: f.adopted_at,
        resume_of: f.resume_of.clone(),
        replayed_from: f.replayed_from.clone(),
        ceiling_at_pin: ceiling(&f.ceiling_at_pin),
    }
}

fn pin(p: &LeanPluginJobPin) -> JobPin {
    JobPin {
        script_id: p.script_id.clone(),
        source_digest: p.source_digest.clone(),
        origin: spelled(&p.origin),
        engine_digest: p.engine_digest.clone(),
        plugin_ref: p.plugin_ref.clone(),
        agent_budget: p.agent_budget,
        handle: p.handle.clone(),
        cwd: p.cwd.clone(),
        adopted_at: p.adopted_at,
        args_digest: p.args_digest.clone(),
        chain_root_doc_id: p.chain_root.clone(),
        resume_of: p.resume_of.clone(),
        replayed_from: p.replayed_from.clone(),
        ceiling_at_pin: ceiling(&p.ceiling_at_pin),
    }
}

fn guest_pin(g: &LeanPluginJobGuestPin) -> JobGuestPin {
    JobGuestPin {
        script_id: g.script_id.clone(),
        source_digest: g.source_digest.clone(),
        agent_budget: g.agent_budget,
        handle: g.handle.clone(),
    }
}

/// The four fields a guest may write into a pin; the claimed host fields stay
/// behind, as in the Lean `GuestPin`.
fn guest_of(p: &LeanPluginJobPin) -> JobGuestPin {
    JobGuestPin {
        script_id: p.script_id.clone(),
        source_digest: p.source_digest.clone(),
        agent_budget: p.agent_budget,
        handle: p.handle.clone(),
    }
}

fn resume(r: &LeanPluginJobResume) -> JobResume {
    JobResume {
        row_doc_id: r.row_doc_id.clone(),
        engine: r.engine_current.then_some(JobResumeEngine::Current),
        answer: r.answer.clone(),
        budget: r.budget,
    }
}

fn process_scope(s: &LeanPluginJobProcessScope) -> ProcessControlScope {
    ProcessControlScope {
        request_id: s.request_id.clone(),
        session_id: s.session_id.clone(),
        node_did: s.node_did.clone(),
        requester_did: s.requester_did.clone(),
    }
}

fn root_call(c: &LeanPluginJobRootCall) -> RootCall {
    RootCall {
        argv: c.argv.clone(),
        args: c.args.clone(),
        agent_budget: c.agent_budget,
    }
}

fn row(r: &LeanPluginJobRow) -> StepRow {
    StepRow {
        root: r.root.clone(),
        row_doc_id: r.row_doc_id.clone(),
        caller_session: r.caller_session.clone(),
        caller_cwd: r.caller_cwd.clone(),
        handle: r.handle.clone(),
        caller_hop: r.caller_hop,
        call: StepCall {
            argv: r.call.argv.clone(),
            args: r.call.args.clone(),
            script: r.call.script.clone(),
            agent_budget: r.call.agent_budget,
        },
        root_call: r.root_call.as_ref().map(root_call),
        resume: r.resume.as_ref().map(resume),
        pin: r.pin.as_ref().map(pin),
        children: r
            .children
            .iter()
            .map(|c| StepChild {
                retry_key: c.retry_key.clone(),
                slot: c.slot.clone(),
                attempt: c.attempt,
                target: target(&c.target),
                state: spelled(&c.state),
                messages: c.messages,
            })
            .collect(),
        spent: r.spent,
        observed_terminal: r.observed_terminal,
        consecutive_failures: r.consecutive_failures,
        state: spelled(&r.state),
    }
}

/// Lean's `Extras` count a rider's presence; the protocol rider's body is not
/// modeled.
fn writes(x: &LeanPluginJobExtras) -> Vec<DataWrite> {
    (0..x.writes)
        .map(|_| DataWrite {
            op: DataOp::Insert,
            collection: String::new(),
            doc_id: None,
            fields: serde_json::Map::new(),
            on_conflict: None,
        })
        .collect()
}

fn scratch(x: &LeanPluginJobExtras) -> Vec<JobScratch> {
    x.scratch
        .iter()
        .map(|name| JobScratch {
            name: name.clone(),
            content: String::new(),
        })
        .collect()
}

fn progress(x: &LeanPluginJobExtras) -> Option<JobProgress> {
    x.progress.then(|| JobProgress {
        title: String::new(),
        state: JobState::Running,
        pause_reason: None,
        phases: Vec::new(),
        active: Vec::new(),
        spawned: 0,
        budget: JobBudget {
            total: 0,
            spent: 0,
            remaining: 0,
        },
        last_event: String::new(),
        last_event_at: None,
        question: None,
        observed_terminal_children: 0,
        revision: 0,
    })
}

fn output(o: &LeanPluginJobOutput) -> JobStepOutput {
    match o {
        LeanPluginJobOutput::Result {
            extras: x,
            control,
            pin,
            resume: r,
        } => JobStepOutput::Result {
            result: serde_json::Value::Null,
            writes: writes(x),
            scratch: scratch(x),
            progress: progress(x),
            control: control
                .iter()
                .map(|c| JobControlRequest {
                    row_doc_id: c.row_doc_id.clone(),
                    kind: spelled(&c.control),
                })
                .collect(),
            pin: pin.as_ref().map(guest_pin),
            resume: r.as_ref().map(resume),
        },
        LeanPluginJobOutput::Fail { extras: x } => JobStepOutput::Fail {
            fail: JobFailure {
                code: String::new(),
                message: String::new(),
            },
            writes: writes(x),
            progress: progress(x),
        },
        LeanPluginJobOutput::Suspend {
            suspend,
            reason,
            extras: x,
            pin,
            resume: r,
        } => JobStepOutput::Suspend {
            state: None,
            suspend: JobSuspend {
                kind: spelled(suspend),
                reason: reason.as_deref().map(spelled),
                message: String::new(),
                question: None,
            },
            writes: writes(x),
            scratch: scratch(x),
            progress: progress(x),
            pin: pin.as_ref().map(guest_pin),
            resume: r.as_ref().map(resume),
        },
        LeanPluginJobOutput::Effects {
            pin,
            resume: r,
            spawns,
            messages,
            interrupts,
            extras: x,
        } => JobStepOutput::Effects {
            effects: JobEffects {
                state: None,
                pin: pin.as_ref().map(guest_pin),
                resume: r.as_ref().map(resume),
                spawn: spawns
                    .iter()
                    .map(|s| JobSpawn {
                        slot: s.slot.clone(),
                        attempt: s.attempt,
                        target: wire_id(&s.target),
                        prompt: String::new(),
                        title: String::new(),
                        cwd: s.cwd.clone(),
                    })
                    .collect(),
                message: messages
                    .iter()
                    .map(|slot| JobMessage {
                        slot: slot.clone(),
                        text: String::new(),
                    })
                    .collect(),
                interrupt: interrupts
                    .iter()
                    .map(|slot| JobInterrupt { slot: slot.clone() })
                    .collect(),
                progress: progress(x),
                writes: writes(x),
                scratch: scratch(x),
            },
        },
    }
}

fn job_policy(p: &LeanPluginJobPolicy) -> JobPolicy {
    JobPolicy {
        dynamic: spelled(&p.mode),
        dynamic_targets: p.dynamic_targets.clone(),
        trusted_packs: p.trusted_packs.clone(),
    }
}

fn target_ids(targets: &[LeanPluginJobTarget]) -> Vec<String> {
    targets.iter().map(wire_id).collect()
}

fn context(
    c: &LeanPluginJobContext,
    _job: &LeanPluginJobRow,
    _out: &LeanPluginJobOutput,
) -> StepContext {
    StepContext {
        live: ceiling(&c.live),
        available: c.available,
        caller_targets: scope(&c.caller_targets),
        job_targets: scope(&c.job_targets),
        policy: job_policy(&c.policy),
        engine_digest: c.engine_digest.clone(),
        engine_provenance_operator: c.engine_provenance_operator,
        max_hop: c.max_hop,
        target_hops: c
            .target_hops
            .iter()
            .map(|h| (target(&h.target), h.hop))
            .collect(),
        target_ids: c
            .bindings
            .iter()
            .map(|binding| (binding.wire.clone(), target(&binding.target)))
            .collect(),
        live_background: c.live_background,
        started_by_job: c.started_by_job,
        cwd_allowed: c.cwd_allowed.clone(),
        within: c
            .within
            .iter()
            .map(|w| (w.base.clone(), w.cwd.clone()))
            .collect(),
        script_digest: c.script_digest.clone(),
        adoption_facts: host_facts(&c.adoption_facts),
        caller_scope: process_scope(&c.caller_scope),
        control_targets: c
            .control_targets
            .iter()
            .map(|t| {
                (
                    t.row_doc_id.clone(),
                    t.target
                        .as_ref()
                        .map(|o| (process_scope(&o.owner), o.same_plugin)),
                )
            })
            .collect(),
        resume_targets: c
            .resume_targets
            .iter()
            .map(|t| {
                (
                    t.row_doc_id.clone(),
                    t.is_head,
                    t.target.as_ref().map(|o| {
                        (
                            process_scope(&o.owner),
                            o.same_plugin,
                            row(&o.prior),
                            row(&o.root),
                        )
                    }),
                )
            })
            .collect(),
        resume_facts: host_facts(&c.resume_facts),
    }
}

fn tool_call(c: &LeanPluginJobToolCall) -> ToolCallView {
    ToolCallView {
        state: ToolCallState::from_persisted(&c.state)
            .unwrap_or_else(|| panic!("unknown Lean tool call state {}", c.state)),
        kind: match c.operation.as_str() {
            "nativeCommand" => ToolRowKind::Native,
            "pluginJob" => ToolRowKind::PluginJob,
            other => panic!("unknown Lean tool operation {other}"),
        },
        await_mode: AwaitMode::from_persisted(&c.await_mode)
            .unwrap_or_else(|| panic!("unknown Lean await mode {}", c.await_mode)),
    }
}

fn delivery_spelling(delivery: Option<Delivery>) -> Option<&'static str> {
    delivery.map(|d| match d {
        Delivery::Inline => "inline",
        Delivery::Continuation => "continuation",
    })
}

/// Lean `admitStep` and `settleStep` through the production admission owner:
/// the refusal code, the admitted row, the state a refused row fails closed
/// into, and the children the kill reaches.
#[test]
fn generated_plugin_job_admission_cases_drive_admit_step() {
    let cases = lean_plugin_job_admission_cases();
    assert_eq!(
        cases.len(),
        220,
        "Lean supplies 28 admission regressions and 192 interrupted wire-to-recovery traces"
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
        let settled =
            match gents::tools::background::job::admit::admit_step(&context, &job, &output) {
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
                    assert_eq!(
                        refusal.refusal
                            == gents::tools::background::job::admit::Refusal::DynamicCodeDisabled,
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
            let facts = gents::tools::background::job::recovery::facts_from_durable_read(
                durable,
                durable.state == JobState::Running,
                durable.pin.as_ref().expect("wire trace has a durable pin"),
                gents::tools::background::job::recovery::DurableRead {
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
            let before = crate::lean_vocab_test::LeanPluginJobRecoveryRow {
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
            let (_, after) = super::recovery_sweeps::tick_row(before);
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
                match gents::tools::background::job::dispatch::adopt(
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

/// What the kill reads about each child's caused request, by retry key.
fn observations(observed: &[LeanPluginJobObservation]) -> BTreeMap<&str, KillObservation> {
    observed
        .iter()
        .map(|o| (o.retry_key.as_str(), spelled(&o.observation)))
        .collect()
}

/// The killed children and their actions, ordered by retry key.
fn kills(killed: &[LeanPluginJobKill]) -> Vec<(String, KillAction)> {
    let mut kills: Vec<_> = killed
        .iter()
        .map(|k| (k.retry_key.clone(), spelled(&k.action)))
        .collect();
    kills.sort_by(|a, b| a.0.cmp(&b.0));
    kills
}

fn by_retry_key(mut kills: Vec<(String, KillAction)>) -> Vec<(String, KillAction)> {
    kills.sort_by(|a, b| a.0.cmp(&b.0));
    kills
}

/// Lean `PluginData.Invocation` as the native call context that carries the
/// stamp values available there. The values are placeholders.
fn stamp_context(invocation: &str) -> StampContext {
    let node_did = "did:key:node".to_owned();
    let session_id = "session".to_owned();
    let request_doc_id = "request".to_owned();
    let tool_call_doc_id = "tool-call".to_owned();
    match invocation {
        "tool_call" => StampContext::ToolOutput {
            node_did,
            session_id,
            request_doc_id,
            tool_call_doc_id,
        },
        "job_step" => StampContext::JobStep {
            node_did,
            session_id,
            request_doc_id,
            tool_call_doc_id,
            job_row_doc_id: "job-row".to_owned(),
            chain_root_doc_id: "chain-root".to_owned(),
        },
        "callback" => StampContext::Callback { node_did },
        other => panic!("unknown Lean invocation {other}"),
    }
}

fn control_echo(e: &LeanPluginJobControlEcho) -> JobControlEcho {
    JobControlEcho {
        kind: spelled(&e.kind),
        payload: e.payload.as_ref().map(resume),
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

/// Lean `SpawnKey.retryKey`, `messageKey` and `sessionId`: the protocol
/// encoders reproduce the Lean strings byte for byte. The protocol's
/// `decode_job_retry_key` is the one Rust decoder; it agrees with
/// `decodeRetryKey?` on the retry key and on the message keys, which are not
/// retry keys.
#[test]
fn generated_plugin_job_key_cases_match_protocol_encoders() {
    let cases = lean_plugin_job_key_cases();
    assert_eq!(
        cases.len(),
        5,
        "Lean pins five key rows in `key_regressions`"
    );
    let mut names = BTreeSet::new();
    for case in cases {
        assert!(names.insert(case.name.as_str()), "duplicate {}", case.name);
        let (root, slot, attempt) = (&case.root, &case.slot, case.attempt);
        let expected = &case.expected;
        assert_eq!(
            job_spawn_key(root, slot, attempt),
            expected.retry_key,
            "{}: retry key",
            case.name
        );
        assert_eq!(
            job_session_id(root, slot, attempt),
            expected.session_id,
            "{}: session id",
            case.name
        );
        let message_keys: Vec<(u32, String)> = case
            .messages
            .iter()
            .map(|&n| (n, job_message_key(root, slot, attempt, n)))
            .collect();
        assert_eq!(
            message_keys,
            expected
                .message_keys
                .iter()
                .map(|m| (m.n, m.key.clone()))
                .collect::<Vec<_>>(),
            "{}: message keys",
            case.name
        );
        assert_eq!(
            decode_job_retry_key(&expected.retry_key),
            expected
                .decoded
                .as_ref()
                .map(|d| (d.slot.clone(), d.attempt)),
            "{}: decoded retry key",
            case.name
        );
        assert_eq!(
            message_keys
                .iter()
                .any(|(_, key)| decode_job_retry_key(key).is_some()),
            expected.message_keys_decode,
            "{}: message keys decode",
            case.name
        );
    }
}

/// Lean `Record` as the stored `installed_via`: no record, a record without
/// the field (written before the field existed), or a stored value.
fn home_record(r: &LeanPluginJobRecord) -> Option<Option<InstalledVia>> {
    match (r.kind.as_str(), r.via.as_deref()) {
        ("absent", None) => None,
        ("legacy", None) => Some(None),
        ("written", Some(via)) => Some(Some(spelled(via))),
        other => panic!("unknown Lean install record {other:?}"),
    }
}

/// Lean `callOrigin`, `provenanceOf`, `recordAfterInstall`,
/// `packJobBlockAdmissible`, `Policy.valid` and `wireTargetsFrom` through the
/// pure cores `origin::call_origin`,
/// `home_install::{recorded_provenance, record_after_install_via}` and
/// `settings::{job_block_admissible, policy_valid, wire_targets_from}`.
#[test]
fn generated_plugin_job_authority_cases_drive_origin_and_provenance() {
    let cases = lean_plugin_job_authority_cases();
    let mut names = BTreeSet::new();
    let mut kinds: BTreeMap<&str, usize> = BTreeMap::new();
    for case in cases {
        assert!(names.insert(case.name()), "duplicate {}", case.name());
        match case {
            LeanPluginJobAuthorityCase::Provenance {
                name,
                pack_kind,
                record,
                expected,
            } => {
                *kinds.entry("provenance").or_default() += 1;
                assert_eq!(
                    recorded_provenance(spelled::<PackKind>(pack_kind), home_record(record)),
                    spelled::<InstalledVia>(&expected.provenance),
                    "{name}: provenance"
                );
            }
            LeanPluginJobAuthorityCase::Install {
                name,
                prior,
                pack_kind,
                explicit,
                via,
                expected,
            } => {
                *kinds.entry("install").or_default() += 1;
                let kind: PackKind = spelled(pack_kind);
                let stored = record_after_install_via(
                    home_record(prior),
                    kind.clone(),
                    *explicit,
                    spelled(via),
                );
                let record = Some(Some(stored));
                assert_eq!(record, home_record(&expected.record), "{name}: record");
                assert_eq!(
                    recorded_provenance(kind, record),
                    spelled::<InstalledVia>(&expected.provenance),
                    "{name}: provenance after install"
                );
            }
            LeanPluginJobAuthorityCase::Origin {
                name,
                call,
                expected,
            } => {
                *kinds.entry("origin").or_default() += 1;
                assert_eq!(
                    call_origin(call.script.as_deref()),
                    spelled::<JobOrigin>(&expected.origin),
                    "{name}: origin"
                );
            }
            LeanPluginJobAuthorityCase::PackJobBlock {
                name,
                via,
                closure,
                block,
                expected,
            } => {
                *kinds.entry("pack_job_block").or_default() += 1;
                assert_eq!(
                    job_block_admissible(
                        spelled(via),
                        closure,
                        &block.targets_from,
                        &block.trusted,
                        spelled(&block.dynamic),
                    ),
                    expected.admissible,
                    "{name}: admissible"
                );
            }
            LeanPluginJobAuthorityCase::PolicyValid {
                name,
                agent_targets,
                policy,
                expected,
            } => {
                *kinds.entry("policy_valid").or_default() += 1;
                assert_eq!(
                    policy_valid(agent_targets, &job_policy(policy)),
                    expected.valid,
                    "{name}: valid"
                );
            }
            LeanPluginJobAuthorityCase::WireTargetsFrom {
                name,
                pack,
                pack_targets,
                policy,
                job_targets,
                caller_targets,
                expected,
            } => {
                *kinds.entry("wire_targets_from").or_default() += 1;
                assert_eq!(
                    wire_targets_from(
                        pack,
                        &target_ids(pack_targets),
                        job_policy(policy),
                        &target_ids(job_targets),
                        &target_ids(caller_targets),
                    ),
                    (
                        job_policy(&expected.policy),
                        target_ids(&expected.job_targets),
                        target_ids(&expected.caller_targets),
                    ),
                    "{name}: wired policy and targets"
                );
            }
        }
    }
    assert_eq!(
        kinds,
        BTreeMap::from([
            ("install", 6),
            ("origin", 4),
            ("pack_job_block", 5),
            ("policy_valid", 4),
            ("provenance", 12),
            ("wire_targets_from", 2),
        ]),
        "Lean pins these authority rows in `authority_regressions`"
    );
}

/// Decodes one wire sample strictly into its protocol type and returns the
/// re-encoded JSON, which must equal the Lean rendering.
fn round_trip<T>(case: &LeanPluginJobWireCase) -> serde_json::Value
where
    T: DeserializeOwned + Serialize,
{
    let decoded: T = serde_json::from_value(case.value.clone())
        .unwrap_or_else(|error| panic!("{}: strict decode: {error}", case.name));
    let encoded = serde_json::to_value(&decoded)
        .unwrap_or_else(|error| panic!("{}: encode: {error}", case.name));
    assert_eq!(encoded, case.value, "{}: re-encoded JSON", case.name);
    encoded
}

fn round_trip_case(case: &LeanPluginJobWireCase) -> serde_json::Value {
    match case.kind {
        LeanPluginJobWireKind::JobStepInput => round_trip::<JobStepInput>(case),
        LeanPluginJobWireKind::JobStepOutput => round_trip::<JobStepOutput>(case),
        LeanPluginJobWireKind::JobView => round_trip::<JobView>(case),
        LeanPluginJobWireKind::JobOutcome => {
            let encoded = round_trip::<JobOutcome>(case);
            assert_eq!(
                encoded["code"], case.name,
                "{}: a host code names its row",
                case.name
            );
            encoded
        }
    }
}

/// Every wire sample the Lean types render decodes strictly into its
/// `gents_protocol::job` type and re-encodes to the same canonical JSON.
#[test]
fn generated_plugin_job_wire_cases_round_trip_protocol_types() {
    let cases = lean_plugin_job_wire_cases();
    let mut names = BTreeSet::new();
    let mut kinds: BTreeMap<&str, usize> = BTreeMap::new();
    for case in cases {
        assert!(names.insert(case.name.as_str()), "duplicate {}", case.name);
        round_trip_case(case);
        *kinds
            .entry(match case.kind {
                LeanPluginJobWireKind::JobStepInput => "job_step_input",
                LeanPluginJobWireKind::JobStepOutput => "job_step_output",
                LeanPluginJobWireKind::JobView => "job_view",
                LeanPluginJobWireKind::JobOutcome => "job_outcome",
            })
            .or_default() += 1;
    }
    assert_eq!(
        kinds,
        BTreeMap::from([
            ("job_outcome", 9),
            ("job_step_input", 3),
            ("job_step_output", 9),
            ("job_view", 1),
        ]),
        "Lean pins these wire samples in `wire_regressions`"
    );
}

/// The `OpeningRequest` rows decode strictly; L3 consumes them to drive
/// `opening_request`.
#[test]
fn generated_plugin_job_lineage_cases_decode_strictly() {
    let cases = lean_plugin_job_lineage_cases();
    assert_eq!(
        cases.len(),
        7,
        "Lean pins seven lineage rows in `lineage_regressions`"
    );
    let mut names = BTreeSet::new();
    for case in cases {
        assert!(names.insert(case.name.as_str()), "duplicate {}", case.name);
        let docs: BTreeSet<&str> = case.rows.iter().map(|r| r.doc_id.as_str()).collect();
        assert_eq!(
            docs.len(),
            case.rows.len(),
            "{}: duplicate _docID",
            case.name
        );
        assert!(!case.queries.is_empty(), "{}: no query", case.name);
        for query in &case.queries {
            if let Some(opening) = &query.expected_opening_doc_id {
                assert!(
                    docs.contains(opening.as_str()),
                    "{}: opening request {opening} is not a row",
                    case.name
                );
            }
            assert!(
                !query.listed_start || query.expected_opening_doc_id.is_some(),
                "{}: a listed start has an opening request",
                case.name
            );
        }
    }
}

/// `strip_secret_fields` drops a field whose upper-cased name contains one of
/// these (`toolset::shared::command::is_secret_env_name`), so no job field may.
const SECRET_SUBSTRINGS: [&str; 4] = ["KEY", "SECRET", "TOKEN", "PASSWORD"];

fn collect_field_names(value: &serde_json::Value, names: &mut BTreeSet<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (name, nested) in map {
                names.insert(name.clone());
                collect_field_names(nested, names);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_field_names(item, names);
            }
        }
        _ => {}
    }
}

/// Every optional field is set so `skip_serializing_if` hides none.
fn data_query() -> DataQuery {
    DataQuery {
        collection: "c".to_owned(),
        filter: serde_json::Map::new(),
        order: Some(serde_json::Map::new()),
        limit: 1,
        offset: Some(0),
        fields: vec!["f".to_owned()],
    }
}

#[test]
fn plugin_job_protocol_field_names_are_secret_free() {
    let mut names = BTreeSet::new();
    for case in lean_plugin_job_wire_cases() {
        collect_field_names(&round_trip_case(case), &mut names);
    }
    let settings: PluginJobSettings =
        serde_json::from_value(serde_json::json!({})).expect("settings defaults");
    for constructed in [
        serde_json::to_value(JobPolicy::default()),
        serde_json::to_value(settings),
        serde_json::to_value(JobControlRequest {
            row_doc_id: "row".to_owned(),
            kind: JobControlKind::Stop,
        }),
        serde_json::to_value(JobResultRequest {
            row_doc_id: "row".to_owned(),
            offset: 0,
            limit: 1,
        }),
        serde_json::to_value(JobResultResponse {
            row_doc_id: "row".to_owned(),
            state: JobState::Running,
            result: "text".to_owned(),
            offset: 0,
            total_bytes: 4,
            truncated: true,
        }),
        serde_json::to_value(ChildOutputRequest {
            slots: vec!["1:a".to_owned()],
            offset: 0,
            limit: 1,
        }),
        serde_json::to_value(ChildOutputResponse {
            slots: BTreeMap::from([(
                "1:a".to_owned(),
                ChildOutputSlice {
                    text: "text".to_owned(),
                    truncated: true,
                },
            )]),
        }),
        serde_json::to_value(DataCall {
            id: "d1".to_owned(),
            query: data_query(),
        }),
        serde_json::to_value(DataQueryResponse {
            rows: vec![serde_json::Map::new()],
        }),
        serde_json::to_value(JobCall::ChildOutput {
            id: "c1".to_owned(),
            child_output: ChildOutputRequest {
                slots: Vec::new(),
                offset: 0,
                limit: 1,
            },
        }),
        serde_json::to_value(JobCall::Result {
            id: "c2".to_owned(),
            result: JobResultRequest {
                row_doc_id: "row".to_owned(),
                offset: 0,
                limit: 1,
            },
        }),
        serde_json::to_value(RoundResult::Result { result: 1 }),
        serde_json::to_value(RoundResult::<u32>::Error {
            error: "e".to_owned(),
        }),
        serde_json::to_value(JobControlOutcome::Applied {
            state: JobState::Running,
            killed_children: 1,
        }),
        serde_json::to_value(JobControlOutcome::AlreadyTerminal {
            state: JobState::Running,
        }),
        serde_json::to_value(JobReadError::NotAJob),
        serde_json::to_value(JobReadError::Unavailable {
            message: "m".to_owned(),
        }),
        serde_json::to_value(JobReadError::Malformed {
            message: "m".to_owned(),
        }),
        serde_json::to_value(PluginJobDeclaration(true)),
        serde_json::to_value(PluginDataDeclaration {
            collections: vec!["c".to_owned()],
            ops: vec!["query".to_owned()],
            stamped: BTreeMap::from([("f".to_owned(), StampedField::NodeDid)]),
        }),
    ] {
        collect_field_names(&constructed.expect("constructed value encodes"), &mut names);
    }
    assert!(
        names.contains("row_doc_id") && names.contains("trusted_packs"),
        "the walk reaches nested and defaulted fields: {names:?}"
    );
    for name in &names {
        let upper = name.to_ascii_uppercase();
        for needle in SECRET_SUBSTRINGS {
            assert!(
                !upper.contains(needle),
                "job field {name:?} contains {needle}"
            );
        }
    }
}
