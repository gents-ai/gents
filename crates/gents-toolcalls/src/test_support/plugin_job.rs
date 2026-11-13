use crate::background::job::admit::{
    HostFacts, RootCall, StepCall, StepChild, StepContext, StepRow,
};
use crate::background::job::control::{KillAction, KillObservation};
use crate::background::job::dispatch::{Delivery, ToolCallView};
use crate::background::status::ProcessControlScope;
use crate::call::lifecycle::ToolRowKind;
use crate::plugin::data::StampContext;
use gents_lean_vocab::{
    LeanPluginJobCeiling, LeanPluginJobContext, LeanPluginJobControlEcho, LeanPluginJobExtras,
    LeanPluginJobGuestPin, LeanPluginJobHostFacts, LeanPluginJobKill, LeanPluginJobObservation,
    LeanPluginJobOutput, LeanPluginJobPin, LeanPluginJobPolicy, LeanPluginJobProcessScope,
    LeanPluginJobResume, LeanPluginJobRootCall, LeanPluginJobRow, LeanPluginJobScope,
    LeanPluginJobTarget, LeanPluginJobToolCall,
};
use gents_protocol::job::{
    DataOp, DataWrite, JobBudget, JobCeiling, JobControlEcho, JobControlRequest, JobEffects,
    JobFailure, JobGuestPin, JobInterrupt, JobMessage, JobPin, JobPolicy, JobProgress, JobResume,
    JobResumeEngine, JobScratch, JobSpawn, JobState, JobStepOutput, JobSuspend,
};
use gents_protocol::tool_call::AwaitMode;
use gents_session::session::tool_call_state::ToolCallState;
use gents_tools::surface::EndpointScope;
use serde::de::DeserializeOwned;
use std::collections::BTreeMap;

/// A closed Lean spelling decoded by the native enum's own serde names.
pub fn spelled<T>(spelling: &str) -> T
where
    T: DeserializeOwned,
{
    serde_json::from_value(serde_json::Value::String(spelling.to_owned()))
        .unwrap_or_else(|error| panic!("unknown Lean spelling {spelling:?}: {error}"))
}

pub fn target(t: &LeanPluginJobTarget) -> (String, String) {
    (t.node.clone(), t.agent.clone())
}

/// A target's wire spelling is its AgentTarget id, which the host resolves to
/// Lean's `(node, agent)` pair; spelled unlike the agent id so a host that
/// matched agent ids would refuse.
pub fn wire_id(t: &LeanPluginJobTarget) -> String {
    format!("{}@{}", t.agent, t.node)
}

pub fn scope(s: &LeanPluginJobScope) -> EndpointScope<(String, String), ()> {
    match s.kind.as_str() {
        "none" => EndpointScope::None,
        "all" => EndpointScope::All,
        "only" => EndpointScope::<(String, String), ()>::only_units(s.targets.iter().map(target)),
        other => panic!("unknown Lean endpoint scope {other}"),
    }
}

pub fn ceiling(c: &LeanPluginJobCeiling) -> JobCeiling {
    JobCeiling {
        targets: c.targets.clone(),
        max_live: c.max_live,
        max_spawns: c.max_spawns,
        max_messages_per_slot: c.max_messages_per_slot,
        result_bytes: c.result_bytes,
        max_wall_secs: c.max_wall_secs,
    }
}

pub fn host_facts(f: &LeanPluginJobHostFacts) -> HostFacts {
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

pub fn pin(p: &LeanPluginJobPin) -> JobPin {
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

pub fn guest_pin(g: &LeanPluginJobGuestPin) -> JobGuestPin {
    JobGuestPin {
        script_id: g.script_id.clone(),
        source_digest: g.source_digest.clone(),
        agent_budget: g.agent_budget,
        handle: g.handle.clone(),
    }
}

/// The four fields a guest may write into a pin; the claimed host fields stay
/// behind, as in the Lean `GuestPin`.
pub fn guest_of(p: &LeanPluginJobPin) -> JobGuestPin {
    JobGuestPin {
        script_id: p.script_id.clone(),
        source_digest: p.source_digest.clone(),
        agent_budget: p.agent_budget,
        handle: p.handle.clone(),
    }
}

pub fn resume(r: &LeanPluginJobResume) -> JobResume {
    JobResume {
        row_doc_id: r.row_doc_id.clone(),
        engine: r.engine_current.then_some(JobResumeEngine::Current),
        answer: r.answer.clone(),
        budget: r.budget,
    }
}

pub fn process_scope(s: &LeanPluginJobProcessScope) -> ProcessControlScope {
    ProcessControlScope {
        request_id: s.request_id.clone(),
        session_id: s.session_id.clone(),
        node_did: s.node_did.clone(),
        requester_did: s.requester_did.clone(),
    }
}

pub fn root_call(c: &LeanPluginJobRootCall) -> RootCall {
    RootCall {
        argv: c.argv.clone(),
        args: c.args.clone(),
        agent_budget: c.agent_budget,
    }
}

pub fn row(r: &LeanPluginJobRow) -> StepRow {
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
pub fn writes(x: &LeanPluginJobExtras) -> Vec<DataWrite> {
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

pub fn scratch(x: &LeanPluginJobExtras) -> Vec<JobScratch> {
    x.scratch
        .iter()
        .map(|name| JobScratch {
            name: name.clone(),
            content: String::new(),
        })
        .collect()
}

pub fn progress(x: &LeanPluginJobExtras) -> Option<JobProgress> {
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

pub fn output(o: &LeanPluginJobOutput) -> JobStepOutput {
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

pub fn job_policy(p: &LeanPluginJobPolicy) -> JobPolicy {
    JobPolicy {
        dynamic: spelled(&p.mode),
        dynamic_targets: p.dynamic_targets.clone(),
        trusted_packs: p.trusted_packs.clone(),
    }
}

pub fn target_ids(targets: &[LeanPluginJobTarget]) -> Vec<String> {
    targets.iter().map(wire_id).collect()
}

pub fn context(
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
        caller_node_did: None,
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

pub fn tool_call(c: &LeanPluginJobToolCall) -> ToolCallView {
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

pub fn delivery_spelling(delivery: Option<Delivery>) -> Option<&'static str> {
    delivery.map(|d| match d {
        Delivery::Inline => "inline",
        Delivery::Continuation => "continuation",
    })
}

/// What the kill reads about each child's caused request, by retry key.
pub fn observations(observed: &[LeanPluginJobObservation]) -> BTreeMap<&str, KillObservation> {
    observed
        .iter()
        .map(|o| (o.retry_key.as_str(), spelled(&o.observation)))
        .collect()
}

/// The killed children and their actions, ordered by retry key.
pub fn kills(killed: &[LeanPluginJobKill]) -> Vec<(String, KillAction)> {
    let mut kills: Vec<_> = killed
        .iter()
        .map(|k| (k.retry_key.clone(), spelled(&k.action)))
        .collect();
    kills.sort_by(|a, b| a.0.cmp(&b.0));
    kills
}

pub fn by_retry_key(mut kills: Vec<(String, KillAction)>) -> Vec<(String, KillAction)> {
    kills.sort_by(|a, b| a.0.cmp(&b.0));
    kills
}

/// Lean `PluginData.Invocation` as the native call context that carries the
/// stamp values available there. The values are placeholders.
pub fn stamp_context(invocation: &str) -> StampContext {
    let caller_did = "did:key:node".to_owned();
    let session_id = "session".to_owned();
    let request_doc_id = "request".to_owned();
    let tool_call_doc_id = "tool-call".to_owned();
    match invocation {
        "tool_call" => StampContext::ToolOutput {
            caller_did,
            session_id,
            request_doc_id,
            tool_call_doc_id,
        },
        "job_step" => StampContext::JobStep {
            caller_did,
            session_id,
            request_doc_id,
            tool_call_doc_id,
            job_row_doc_id: "job-row".to_owned(),
            chain_root_doc_id: "chain-root".to_owned(),
        },
        "callback" => StampContext::Callback { caller_did },
        other => panic!("unknown Lean invocation {other}"),
    }
}

pub fn control_echo(e: &LeanPluginJobControlEcho) -> JobControlEcho {
    JobControlEcho {
        kind: spelled(&e.kind),
        payload: e.payload.as_ref().map(resume),
    }
}
