use std::collections::BTreeSet;

use crate::background_tools::ProcessControlScope;
use crate::hook::persistence::MAX_BACKGROUNDED_TOOLS_PER_PARENT;
use crate::lifecycle::materialize::{next_request_hop, request_hop_within_bound, RequestHopCause};
use crate::plugin_job::keys::job_spawn_key;
use crate::tool_surface::EndpointScope;
use gents_protocol::job::{
    DynamicMode, JobCeiling, JobChildState, JobControlRequest, JobEffects, JobGuestPin,
    JobInterrupt, JobMessage, JobOrigin, JobPin, JobPolicy, JobResume, JobResumeEngine, JobSpawn,
    JobState, JobStepOutput,
};

/// Lean `CallArgs`; Lean types `args` as opaque text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepCall {
    pub argv: Vec<String>,
    pub args: Option<String>,
    pub path: Option<String>,
    pub script: Option<String>,
    pub agent_budget: Option<u32>,
}

/// Lean `RootCall`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootCall {
    pub argv: Vec<String>,
    pub args: Option<String>,
    pub path: Option<String>,
    pub agent_budget: Option<u32>,
}

/// Lean `Child`: the retry key, the `(node_did, agent_id)` target and the
/// slot attempt's message count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepChild {
    pub retry_key: String,
    pub slot: String,
    pub attempt: u32,
    pub target: (String, String),
    pub state: JobChildState,
    pub messages: u32,
}

/// Lean `PluginJob.Job`: the row admission reads and returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepRow {
    pub root: String,
    pub row_doc_id: String,
    pub caller_session: String,
    pub caller_cwd: String,
    pub handle: String,
    pub caller_hop: u32,
    pub call: StepCall,
    pub root_call: Option<RootCall>,
    pub resume: Option<JobResume>,
    pub pin: Option<JobPin>,
    pub children: Vec<StepChild>,
    pub spent: u32,
    pub observed_terminal: u32,
    pub consecutive_failures: u32,
    pub state: JobState,
}

/// Lean `HostFacts`: every `JobPin` field outside `JobGuestPin`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostFacts {
    pub engine_digest: String,
    pub plugin_ref: String,
    pub cwd: String,
    pub args_digest: String,
    pub chain_root: String,
    pub origin: JobOrigin,
    pub adopted_at: u64,
    pub resume_of: Option<String>,
    pub replayed_from: Option<String>,
    pub ceiling_at_pin: JobCeiling,
}

/// A resume target's owner scope, same-plugin flag, prior row and chain root.
pub type ResumeChain = (ProcessControlScope, bool, StepRow, StepRow);
/// A listed doc id, its chain-head flag and its target, if any.
pub type ResumeTarget = (String, bool, Option<ResumeChain>);

/// Lean `PluginJob.StepContext`: the facts re-read in the step transaction.
/// Lean's function-valued facts are finite tables over the values a step names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepContext {
    pub live: JobCeiling,
    pub available: bool,
    pub caller_targets: EndpointScope<(String, String), ()>,
    pub job_targets: EndpointScope<(String, String), ()>,
    pub policy: JobPolicy,
    pub engine_digest: String,
    pub engine_provenance_operator: bool,
    pub installed_trusted_sources: Vec<String>,
    pub max_hop: Option<u32>,
    pub target_hops: Vec<((String, String), u32)>,
    /// The host's resolution of the AgentTarget ids the caller can see to
    /// Lean's `(target_node_did, agent_id)` targets; the protocol, ceilings and
    /// `dynamic_targets` name targets by these ids.
    pub target_ids: Vec<(String, (String, String))>,
    pub live_background: u32,
    pub started_by_job: bool,
    pub cwd_allowed: Vec<String>,
    pub within: Vec<(String, String)>,
    pub named_origin: Option<JobOrigin>,
    pub adoption_facts: HostFacts,
    pub caller_scope: ProcessControlScope,
    pub control_targets: Vec<(String, Option<(ProcessControlScope, bool)>)>,
    pub resume_targets: Vec<ResumeTarget>,
    pub resume_facts: HostFacts,
}

/// Lean `Refusal`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    Terminal,
    NotAvailable,
    EngineChanged,
    PolicyDenied,
    Stalled,
    DepthExceeded,
    TimedOut,
    StepFailed,
    BackgroundBudgetExceeded,
    ResumeUnknown,
    ResumeNotHead,
}

impl Refusal {
    /// The host terminal code; `Terminal` is a no-op and has none.
    pub fn code(self) -> Option<&'static str> {
        match self {
            Self::Terminal => None,
            Self::NotAvailable => Some("plugin_job_not_available"),
            Self::EngineChanged => Some("plugin_job_engine_changed"),
            Self::PolicyDenied => Some("plugin_job_policy_denied"),
            Self::Stalled => Some("plugin_job_stalled"),
            Self::DepthExceeded => Some("plugin_job_depth_exceeded"),
            Self::TimedOut => Some("plugin_job_timed_out"),
            Self::StepFailed => Some("plugin_job_step_failed"),
            Self::BackgroundBudgetExceeded => Some("background_tool_budget_exceeded"),
            Self::ResumeUnknown => Some("plugin_job_resume_unknown"),
            Self::ResumeNotHead => Some("plugin_job_resume_not_head"),
        }
    }
}

/// Lean `settleStep`'s refusal branch: the row a refusal leaves and the retry
/// keys of the live children its kill reaches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmitRefusal {
    pub refusal: Refusal,
    closed_row: Box<StepRow>,
    kills: Vec<String>,
}

impl AdmitRefusal {
    pub fn code(&self) -> Option<&'static str> {
        self.refusal.code()
    }

    pub fn closed_row(&self) -> &StepRow {
        &self.closed_row
    }

    pub fn kills(&self) -> &[String] {
        &self.kills
    }
}

/// Lean `hostPin`: the guest supplies only the `JobGuestPin` fields.
pub fn host_pin(guest: &JobGuestPin, facts: &HostFacts) -> JobPin {
    JobPin {
        script_id: guest.script_id.clone(),
        source_digest: guest.source_digest.clone(),
        origin: facts.origin,
        engine_digest: facts.engine_digest.clone(),
        plugin_ref: facts.plugin_ref.clone(),
        agent_budget: guest.agent_budget,
        handle: guest.handle.clone(),
        cwd: facts.cwd.clone(),
        adopted_at: facts.adopted_at,
        args_digest: facts.args_digest.clone(),
        chain_root_doc_id: facts.chain_root.clone(),
        resume_of: facts.resume_of.clone(),
        replayed_from: facts.replayed_from.clone(),
        ceiling_at_pin: facts.ceiling_at_pin.clone(),
    }
}

/// Lean `freshSpawns` over the row admission runs against.
pub fn fresh_spawns<'a>(
    _ctx: &StepContext,
    job: &StepRow,
    out: &'a JobStepOutput,
) -> Vec<&'a JobSpawn> {
    match out {
        JobStepOutput::Effects { effects } => fresh_of(job, &effects.spawn),
        _ => Vec::new(),
    }
}

/// Lean `stalled`: nothing live, no terminal still unobserved, and the step
/// opened nothing. An interrupt is not new.
pub fn is_stalled(live: u32, observed_terminal: u32, terminal: u32, step_new: u32) -> bool {
    live == 0 && observed_terminal >= terminal && step_new == 0
}

/// Lean `fresh.length + msgs.length`. Interrupts are not counted.
pub fn step_new(ctx: &StepContext, job: &StepRow, out: &JobStepOutput) -> u32 {
    match out {
        JobStepOutput::Effects { effects } => {
            (fresh_spawns(ctx, job, out).len() + effects.message.len()) as u32
        }
        _ => 0,
    }
}

/// Lean `resumeValid`: scope, plugin, a terminal prior, and a pin the guest
/// may continue (engine current, or the same script and engine, budget not lower).
pub fn resume_valid(
    caller: &ProcessControlScope,
    owner: &ProcessControlScope,
    same_plugin: bool,
    prior: &StepRow,
    guest: &JobGuestPin,
    facts: &HostFacts,
    resume: &JobResume,
) -> bool {
    same_plugin
        && prior.state != JobState::Running
        && caller.authorizes(
            &owner.session_id,
            &owner.node_did,
            owner.requester_did.as_deref(),
        )
        && resume.row_doc_id == prior.row_doc_id
        && facts.resume_of.as_deref() == Some(prior.row_doc_id.as_str())
        && prior.pin.as_ref().is_some_and(|pin| {
            (resume.engine == Some(JobResumeEngine::Current)
                || (guest.script_id == pin.script_id && facts.engine_digest == pin.engine_digest))
                && pin.agent_budget <= guest.agent_budget
        })
}

/// Lean `admitStep`, settled as `settleStep`: every refusal but `Terminal`
/// fails the row and kills its live chain children.
pub fn admit_step(
    ctx: &StepContext,
    job: &StepRow,
    out: &JobStepOutput,
) -> Result<StepRow, AdmitRefusal> {
    admit(ctx, job, out).map_err(|refusal| match refusal {
        Refusal::Terminal => AdmitRefusal {
            refusal,
            closed_row: Box::new(job.clone()),
            kills: Vec::new(),
        },
        _ => AdmitRefusal {
            refusal,
            closed_row: Box::new(StepRow {
                state: JobState::Failed,
                ..job.clone()
            }),
            kills: live_own_children(job)
                .map(|child| child.retry_key.clone())
                .collect(),
        },
    })
}

fn admit(ctx: &StepContext, job: &StepRow, out: &JobStepOutput) -> Result<StepRow, Refusal> {
    if job.state != JobState::Running {
        return Err(Refusal::Terminal);
    }
    if !ctx.available || !ctx.engine_provenance_operator {
        return Err(Refusal::NotAvailable);
    }
    if engine_changed(ctx, job, out) {
        return Err(Refusal::EngineChanged);
    }
    if out_resume(out).is_some() && !resume_placed(job, out) {
        return Err(Refusal::PolicyDenied);
    }
    if !resume_admitted(ctx, out) {
        return Err(resume_refusal(ctx, out));
    }
    let Some(pin) = step_pin(ctx, job, out) else {
        if ctx.policy.dynamic == DynamicMode::Off && runs_dynamic_code(ctx, &job.call) {
            return Err(Refusal::PolicyDenied);
        }
        return admit_non_job(ctx, job, out);
    };
    let row = step_row(ctx, job, out);
    if dynamic_mode_refusal(&ctx.policy, pin.origin, &row, out).is_some() {
        return Err(Refusal::PolicyDenied);
    }
    if job.pin.is_none() {
        if ctx.live == empty_ceiling() {
            return Err(Refusal::NotAvailable);
        }
        if ctx.live_background as usize >= MAX_BACKGROUNDED_TOOLS_PER_PARENT {
            return Err(Refusal::BackgroundBudgetExceeded);
        }
        if ctx.started_by_job {
            return Err(Refusal::DepthExceeded);
        }
    }
    if pin.origin == JobOrigin::Installed
        && !ctx.installed_trusted_sources.contains(&pin.source_digest)
    {
        return Err(Refusal::PolicyDenied);
    }
    admit_job_output(
        ctx,
        &pin,
        &StepRow {
            pin: Some(pin.clone()),
            consecutive_failures: 0,
            ..row
        },
        out,
    )
}

/// Lean `Ceiling.empty`: an absent job block.
fn empty_ceiling() -> JobCeiling {
    JobCeiling {
        targets: Vec::new(),
        max_live: 0,
        max_spawns: 0,
        max_messages_per_slot: 0,
        result_bytes: 0,
        max_wall_secs: 0,
    }
}

/// Lean `Ceiling.meet`; a target list permits the AgentTarget ids it lists.
pub(super) fn meet(a: &JobCeiling, b: &JobCeiling) -> JobCeiling {
    JobCeiling {
        targets: a
            .targets
            .iter()
            .filter(|id| b.targets.contains(id))
            .cloned()
            .collect(),
        max_live: a.max_live.min(b.max_live),
        max_spawns: a.max_spawns.min(b.max_spawns),
        max_messages_per_slot: a.max_messages_per_slot.min(b.max_messages_per_slot),
        result_bytes: a.result_bytes.min(b.result_bytes),
        max_wall_secs: a.max_wall_secs.min(b.max_wall_secs),
    }
}

pub(super) fn out_pin(out: &JobStepOutput) -> Option<&JobGuestPin> {
    match out {
        JobStepOutput::Result { pin, .. } | JobStepOutput::Suspend { pin, .. } => pin.as_ref(),
        JobStepOutput::Effects { effects } => effects.pin.as_ref(),
        JobStepOutput::Fail { .. } => None,
    }
}

pub(super) fn out_resume(out: &JobStepOutput) -> Option<&JobResume> {
    match out {
        JobStepOutput::Result { resume, .. } | JobStepOutput::Suspend { resume, .. } => {
            resume.as_ref()
        }
        JobStepOutput::Effects { effects } => effects.resume.as_ref(),
        JobStepOutput::Fail { .. } => None,
    }
}

pub(super) fn resolve_target<'a>(
    ctx: &'a StepContext,
    target_id: &str,
) -> Option<&'a (String, String)> {
    ctx.target_ids
        .iter()
        .find(|(id, _)| id == target_id)
        .map(|(_, pair)| pair)
}

/// An id list permits a target when one of its ids resolves to it.
fn ids_permit(ctx: &StepContext, ids: &[String], pair: &(String, String)) -> bool {
    ctx.target_ids
        .iter()
        .any(|(id, resolved)| resolved == pair && ids.contains(id))
}

fn effective_targets_permit(ctx: &StepContext, pin: &JobPin, pair: &(String, String)) -> bool {
    ids_permit(ctx, &meet(&ctx.live, &pin.ceiling_at_pin).targets, pair)
        && ctx.job_targets.permits(pair)
        && ctx.caller_targets.permits(pair)
        && (pin.origin != JobOrigin::Dynamic || ids_permit(ctx, &ctx.policy.dynamic_targets, pair))
}

fn step_pin(ctx: &StepContext, job: &StepRow, out: &JobStepOutput) -> Option<JobPin> {
    job.pin
        .clone()
        .or_else(|| out_pin(out).map(|guest| adopted_pin(ctx, out, guest)))
}

/// Lean `resumePin`. The prior row is unused, as in Lean: its pin was checked
/// by `resume_valid`. This runs only after `resume_valid` admitted the resume
/// and the resume check `root.pin.is_some()` (`resume_admitted`) held, which is
/// why the `expect` on the root pin is allowed.
pub fn resume_pin(
    root: &StepRow,
    _prior: &StepRow,
    guest: &JobGuestPin,
    facts: &HostFacts,
) -> JobPin {
    let root = root.pin.as_ref().expect("resume root row has no pin");
    JobPin {
        script_id: guest.script_id.clone(),
        source_digest: guest.source_digest.clone(),
        origin: facts.origin,
        engine_digest: facts.engine_digest.clone(),
        plugin_ref: facts.plugin_ref.clone(),
        agent_budget: guest.agent_budget,
        handle: root.handle.clone(),
        cwd: facts.cwd.clone(),
        adopted_at: facts.adopted_at,
        args_digest: root.args_digest.clone(),
        chain_root_doc_id: root.chain_root_doc_id.clone(),
        resume_of: facts.resume_of.clone(),
        replayed_from: facts.replayed_from.clone(),
        ceiling_at_pin: facts.ceiling_at_pin.clone(),
    }
}

/// Lean `rootCallOf`.
pub fn root_call_of(root: &StepRow) -> RootCall {
    RootCall {
        argv: root.call.argv.clone(),
        args: root.call.args.clone(),
        path: root.call.path.clone(),
        agent_budget: root.call.agent_budget,
    }
}

/// Lean `resumeRow`.
fn resume_row(root: &StepRow, prior: &StepRow, row: &StepRow, resume: &JobResume) -> StepRow {
    StepRow {
        root: root.root.clone(),
        handle: root.handle.clone(),
        children: prior.children.clone(),
        spent: prior.spent,
        root_call: Some(root_call_of(root)),
        resume: Some(resume.clone()),
        ..row.clone()
    }
}

/// Lean `resumePlaced`: `resume` only on step 0, on effects or beside a pin.
fn resume_placed(job: &StepRow, out: &JobStepOutput) -> bool {
    match out {
        JobStepOutput::Effects { .. } => job.pin.is_none(),
        _ => job.pin.is_none() && out_pin(out).is_some(),
    }
}

/// A listed doc id. Absent means no target and not a chain head.
fn resume_listed<'a>(
    ctx: &'a StepContext,
    doc_id: &str,
) -> Option<(bool, Option<&'a ResumeChain>)> {
    ctx.resume_targets
        .iter()
        .find(|(id, _, _)| id == doc_id)
        .map(|(_, head, target)| (*head, target.as_ref()))
}

/// Lean `resumeRefusal`.
fn resume_refusal(ctx: &StepContext, out: &JobStepOutput) -> Refusal {
    let (Some(resume), Some(_)) = (out_resume(out), out_pin(out)) else {
        return Refusal::PolicyDenied;
    };
    match resume_listed(ctx, &resume.row_doc_id) {
        None | Some((_, None)) => Refusal::ResumeUnknown,
        Some((head, Some((owner, same_plugin, _, _)))) => {
            if *same_plugin
                && !head
                && ctx.caller_scope.authorizes(
                    &owner.session_id,
                    &owner.node_did,
                    owner.requester_did.as_deref(),
                )
            {
                Refusal::ResumeNotHead
            } else {
                Refusal::PolicyDenied
            }
        }
    }
}

/// Lean `resumeAdmitted`.
fn resume_admitted(ctx: &StepContext, out: &JobStepOutput) -> bool {
    let Some(resume) = out_resume(out) else {
        return true;
    };
    let Some(guest) = out_pin(out) else {
        return false;
    };
    let Some((head, Some((owner, same_plugin, prior, root)))) =
        resume_listed(ctx, &resume.row_doc_id)
    else {
        return false;
    };
    head && root.pin.is_some()
        && resume_valid(
            &ctx.caller_scope,
            owner,
            *same_plugin,
            prior,
            guest,
            &ctx.resume_facts,
            resume,
        )
}

/// Lean `stepRow`: a validated resume continues the chain head.
pub(super) fn step_row(ctx: &StepContext, job: &StepRow, out: &JobStepOutput) -> StepRow {
    match out_resume(out).and_then(|resume| {
        resume_listed(ctx, &resume.row_doc_id)
            .and_then(|(_, target)| target.map(|entry| (resume, entry)))
    }) {
        Some((resume, (_, _, prior, root))) => resume_row(root, prior, job, resume),
        None => job.clone(),
    }
}

/// Lean `adoptedPin`.
fn adopted_pin(ctx: &StepContext, out: &JobStepOutput, guest: &JobGuestPin) -> JobPin {
    let Some(resume) = out_resume(out) else {
        return host_pin(guest, &ctx.adoption_facts);
    };
    let Some((_, Some((_, _, prior, root)))) = resume_listed(ctx, &resume.row_doc_id) else {
        return host_pin(guest, &ctx.adoption_facts);
    };
    match (root.pin.as_ref(), prior.pin.as_ref()) {
        (Some(_), Some(_)) => resume_pin(root, prior, guest, &ctx.resume_facts),
        _ => host_pin(guest, &ctx.adoption_facts),
    }
}

fn engine_changed(ctx: &StepContext, job: &StepRow, out: &JobStepOutput) -> bool {
    job.pin.as_ref().is_some_and(|pin| {
        ctx.engine_digest != pin.engine_digest
            && !out_resume(out).is_some_and(|r| r.engine == Some(JobResumeEngine::Current))
    })
}

/// Lean `ch.key.root == job.root`, read by re-encoding, never by decoding.
fn in_chain(job: &StepRow, child: &StepChild) -> bool {
    child.retry_key == job_spawn_key(&job.root, &child.slot, child.attempt)
}

fn spawn_key(job: &StepRow, spawn: &JobSpawn) -> String {
    job_spawn_key(&job.root, &spawn.slot, spawn.attempt)
}

fn known(job: &StepRow, key: &str) -> bool {
    job.children.iter().any(|child| child.retry_key == key)
}

fn fresh_of<'a>(job: &StepRow, spawns: &'a [JobSpawn]) -> Vec<&'a JobSpawn> {
    let mut seen = BTreeSet::new();
    spawns
        .iter()
        .filter(|spawn| {
            let key = spawn_key(job, spawn);
            !known(job, &key) && seen.insert(key)
        })
        .collect()
}

fn live_own_children(job: &StepRow) -> impl Iterator<Item = &StepChild> {
    job.children
        .iter()
        .filter(|child| in_chain(job, child) && child.state == JobChildState::Live)
}

pub(super) fn live_count(job: &StepRow) -> usize {
    live_own_children(job).count()
}

pub(super) fn terminal_count(job: &StepRow) -> usize {
    job.children
        .iter()
        .filter(|child| in_chain(job, child) && child.state != JobChildState::Live)
        .count()
}

fn latest_attempt<'a>(job: &'a StepRow, slot: &str) -> Option<&'a StepChild> {
    job.children
        .iter()
        .filter(|child| in_chain(job, child) && child.slot == slot)
        .fold(None, |acc: Option<&StepChild>, child| match acc {
            Some(latest) if latest.attempt >= child.attempt => Some(latest),
            _ => Some(child),
        })
}

fn attempt_admitted(job: &StepRow, spawn: &JobSpawn) -> bool {
    match latest_attempt(job, &spawn.slot) {
        None => spawn.attempt == 0,
        Some(latest) => {
            latest.state == JobChildState::Interrupted && spawn.attempt == latest.attempt + 1
        }
    }
}

fn target_hop(ctx: &StepContext, pair: &(String, String)) -> u32 {
    ctx.target_hops
        .iter()
        .find(|(listed, _)| listed == pair)
        .map(|(_, hop)| *hop)
        .unwrap_or(0)
}

/// Lean `cwdAdmitted`. `None` is the pinned caller cwd.
fn cwd_admitted(ctx: &StepContext, pin: &JobPin, cwd: &Option<String>) -> bool {
    match cwd {
        None => true,
        Some(cwd) => {
            ctx.within
                .iter()
                .any(|(base, dir)| base == &pin.cwd && dir == cwd)
                && ctx.cwd_allowed.iter().any(|allowed| allowed == cwd)
        }
    }
}

fn spawn_refusal(
    ctx: &StepContext,
    pin: &JobPin,
    job: &StepRow,
    spawn: &JobSpawn,
    target: &(String, String),
) -> Option<Refusal> {
    if !effective_targets_permit(ctx, pin, target) {
        return Some(Refusal::PolicyDenied);
    }
    let hop = next_request_hop(
        RequestHopCause::CrossSession {
            cause_hop: job.caller_hop,
        },
        target_hop(ctx, target),
    );
    if !request_hop_within_bound(ctx.max_hop, hop) {
        return Some(Refusal::DepthExceeded);
    }
    if !cwd_admitted(ctx, pin, &spawn.cwd) {
        return Some(Refusal::PolicyDenied);
    }
    if spawn.slot.len() > 128 {
        return Some(Refusal::PolicyDenied);
    }
    if !attempt_admitted(job, spawn) {
        return Some(Refusal::PolicyDenied);
    }
    None
}

fn panel_admitted(pin: &JobPin, eff: &JobCeiling, job: &StepRow, n: usize) -> bool {
    job.spent as usize + n <= pin.agent_budget.min(eff.max_spawns) as usize
        && live_count(job) + n <= eff.max_live as usize
}

fn add_spawns(job: StepRow, fresh: &[(&JobSpawn, &(String, String))]) -> StepRow {
    let mut children = job.children;
    children.extend(fresh.iter().map(|(spawn, target)| StepChild {
        retry_key: job_spawn_key(&job.root, &spawn.slot, spawn.attempt),
        slot: spawn.slot.clone(),
        attempt: spawn.attempt,
        target: (*target).clone(),
        state: JobChildState::Live,
        messages: 0,
    }));
    StepRow {
        children,
        spent: job.spent + fresh.len() as u32,
        ..job
    }
}

fn messages_admitted(
    ctx: &StepContext,
    pin: &JobPin,
    cap: u32,
    job: &StepRow,
    messages: &[JobMessage],
) -> bool {
    messages
        .iter()
        .all(|message| match latest_attempt(job, &message.slot) {
            Some(latest) => {
                let count = messages.iter().filter(|m| m.slot == message.slot).count();
                effective_targets_permit(ctx, pin, &latest.target)
                    && latest.messages as usize + count <= cap as usize
            }
            None => false,
        })
}

fn add_messages(job: &StepRow, messages: &[JobMessage]) -> StepRow {
    let latest: Vec<Option<&StepChild>> = messages
        .iter()
        .map(|message| latest_attempt(job, &message.slot))
        .collect();
    let children = job
        .children
        .iter()
        .map(|child| {
            let n = latest
                .iter()
                .flatten()
                .filter(|l| {
                    l.retry_key == child.retry_key
                        && l.slot == child.slot
                        && l.attempt == child.attempt
                })
                .count();
            StepChild {
                messages: child.messages + n as u32,
                state: if n == 0 {
                    child.state
                } else {
                    JobChildState::Live
                },
                ..child.clone()
            }
        })
        .collect();
    StepRow {
        children,
        ..job.clone()
    }
}

fn admit_effects(
    ctx: &StepContext,
    pin: &JobPin,
    job: &StepRow,
    effects: &JobEffects,
) -> Result<StepRow, Refusal> {
    let mut fresh = Vec::new();
    for spawn in fresh_of(job, &effects.spawn) {
        let target = resolve_target(ctx, &spawn.target).ok_or(Refusal::PolicyDenied)?;
        if let Some(refusal) = spawn_refusal(ctx, pin, job, spawn, target) {
            return Err(refusal);
        }
        fresh.push((spawn, target));
    }
    let eff = meet(&ctx.live, &pin.ceiling_at_pin);
    if !fresh.is_empty() && !panel_admitted(pin, &eff, job, fresh.len()) {
        return Err(Refusal::PolicyDenied);
    }
    if !messages_admitted(ctx, pin, eff.max_messages_per_slot, job, &effects.message) {
        return Err(Refusal::PolicyDenied);
    }
    if !interrupts_admitted(&add_spawns(job.clone(), &fresh), &effects.interrupt) {
        return Err(Refusal::PolicyDenied);
    }
    let admitted = add_spawns(add_messages(job, &effects.message), &fresh);
    if !effects.message.is_empty() && live_count(&admitted) > eff.max_live as usize {
        return Err(Refusal::PolicyDenied);
    }
    if is_stalled(
        live_count(job) as u32,
        job.observed_terminal,
        terminal_count(job) as u32,
        step_new(
            ctx,
            job,
            &JobStepOutput::Effects {
                effects: effects.clone(),
            },
        ),
    ) {
        return Err(Refusal::Stalled);
    }
    Ok(StepRow {
        observed_terminal: terminal_count(&admitted) as u32,
        ..admitted
    })
}

fn interrupts_admitted(job: &StepRow, interrupts: &[JobInterrupt]) -> bool {
    interrupts
        .iter()
        .all(|interrupt| latest_attempt(job, &interrupt.slot).is_some())
}

fn end_row(
    job: &StepRow,
    state: JobState,
    control: &[JobControlRequest],
) -> Result<StepRow, Refusal> {
    (live_count(job) == 0 && control.is_empty())
        .then(|| StepRow {
            state,
            ..job.clone()
        })
        .ok_or(Refusal::PolicyDenied)
}

fn admit_job_output(
    ctx: &StepContext,
    pin: &JobPin,
    job: &StepRow,
    out: &JobStepOutput,
) -> Result<StepRow, Refusal> {
    match out {
        JobStepOutput::Effects { effects } => admit_effects(ctx, pin, job, effects),
        JobStepOutput::Result { control, .. } => end_row(job, JobState::Completed, control),
        JobStepOutput::Fail { .. } => end_row(job, JobState::Failed, &[]),
        JobStepOutput::Suspend { .. } => end_row(job, JobState::Completed, &[]),
    }
}

fn control_authorized(ctx: &StepContext, row: &str) -> bool {
    ctx.control_targets
        .iter()
        .find(|(doc_id, _)| doc_id == row)
        .and_then(|(_, target)| target.as_ref())
        .is_some_and(|(owner, same_plugin)| {
            *same_plugin
                && ctx.caller_scope.authorizes(
                    &owner.session_id,
                    &owner.node_did,
                    owner.requester_did.as_deref(),
                )
        })
}

fn admit_non_job(
    ctx: &StepContext,
    job: &StepRow,
    out: &JobStepOutput,
) -> Result<StepRow, Refusal> {
    match out {
        JobStepOutput::Result { control, .. } => control
            .iter()
            .all(|request| control_authorized(ctx, &request.row_doc_id))
            .then(|| job.clone())
            .ok_or(Refusal::PolicyDenied),
        JobStepOutput::Fail { .. } | JobStepOutput::Suspend { .. } => Ok(job.clone()),
        JobStepOutput::Effects { .. } => Err(Refusal::PolicyDenied),
    }
}

/// Lean `scriptVerbs`: the workflow verbs that run script code.
const SCRIPT_VERBS: [&str; 3] = ["run", "check", "import"];

fn runs_dynamic_code(ctx: &StepContext, call: &StepCall) -> bool {
    call.argv
        .first()
        .is_some_and(|verb| SCRIPT_VERBS.contains(&verb.as_str()))
        && (call.script.is_some()
            || call.path.is_some()
            || ctx.named_origin == Some(JobOrigin::Dynamic))
}

fn dynamic_mode_refusal(
    policy: &JobPolicy,
    origin: JobOrigin,
    job: &StepRow,
    out: &JobStepOutput,
) -> Option<Refusal> {
    match (origin, policy.dynamic, out) {
        (JobOrigin::Installed, _, _) => None,
        (JobOrigin::Dynamic, DynamicMode::Off, _) => Some(Refusal::PolicyDenied),
        (JobOrigin::Dynamic, DynamicMode::Check, JobStepOutput::Effects { effects }) => {
            (!fresh_of(job, &effects.spawn).is_empty() || !effects.message.is_empty())
                .then_some(Refusal::PolicyDenied)
        }
        (JobOrigin::Dynamic, _, _) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin_job::keys::job_spawn_key;
    use gents_protocol::job::{
        JobControlKind, JobControlRequest, JobEffects, JobFailure, JobInterrupt, JobMessage,
        JobResume, JobSuspend, JobSuspendKind,
    };

    fn ceiling(targets: &[&str]) -> JobCeiling {
        JobCeiling {
            targets: targets.iter().map(|t| (*t).to_owned()).collect(),
            max_live: 4,
            max_spawns: 10,
            max_messages_per_slot: 2,
            result_bytes: 1024,
            max_wall_secs: 60,
        }
    }

    fn pair(agent: &str) -> (String, String) {
        ("node".to_owned(), agent.to_owned())
    }

    fn scope(session: &str) -> ProcessControlScope {
        ProcessControlScope {
            request_id: "request".to_owned(),
            session_id: session.to_owned(),
            node_did: "node".to_owned(),
            requester_did: None,
        }
    }

    fn facts() -> HostFacts {
        HostFacts {
            engine_digest: "engine".to_owned(),
            plugin_ref: "ns/plugin".to_owned(),
            cwd: "/work".to_owned(),
            args_digest: "args".to_owned(),
            chain_root: "row".to_owned(),
            origin: JobOrigin::Installed,
            adopted_at: 1,
            resume_of: None,
            replayed_from: None,
            ceiling_at_pin: ceiling(&["a", "b"]),
        }
    }

    fn base_context() -> StepContext {
        StepContext {
            live: ceiling(&["a", "b"]),
            available: true,
            caller_targets: EndpointScope::All,
            job_targets: EndpointScope::All,
            policy: JobPolicy {
                dynamic: DynamicMode::Run,
                dynamic_targets: vec!["a".to_owned(), "b".to_owned()],
                trusted_packs: Vec::new(),
                project_roots: Vec::new(),
            },
            engine_digest: "engine".to_owned(),
            engine_provenance_operator: true,
            installed_trusted_sources: vec!["source".to_owned()],
            max_hop: Some(8),
            target_hops: vec![(pair("a"), 0), (pair("b"), 0)],
            target_ids: vec![("a".to_owned(), pair("a")), ("b".to_owned(), pair("b"))],
            live_background: 0,
            started_by_job: false,
            cwd_allowed: Vec::new(),
            within: Vec::new(),
            named_origin: None,
            adoption_facts: facts(),
            caller_scope: scope("session"),
            control_targets: Vec::new(),
            resume_targets: Vec::new(),
            resume_facts: facts(),
        }
    }

    fn pin() -> JobPin {
        JobPin {
            script_id: "script".to_owned(),
            source_digest: "source".to_owned(),
            origin: JobOrigin::Installed,
            engine_digest: "engine".to_owned(),
            plugin_ref: "ns/plugin".to_owned(),
            agent_budget: 10,
            handle: "handle".to_owned(),
            cwd: "/work".to_owned(),
            adopted_at: 1,
            args_digest: "args".to_owned(),
            chain_root_doc_id: "row".to_owned(),
            resume_of: None,
            replayed_from: None,
            ceiling_at_pin: ceiling(&["a", "b"]),
        }
    }

    fn running_row(children: Vec<StepChild>) -> StepRow {
        StepRow {
            root: "row".to_owned(),
            row_doc_id: "row".to_owned(),
            caller_session: "session".to_owned(),
            caller_cwd: "/work".to_owned(),
            handle: "handle".to_owned(),
            caller_hop: 0,
            call: StepCall {
                argv: vec!["run".to_owned()],
                args: None,
                path: None,
                script: None,
                agent_budget: None,
            },
            root_call: None,
            resume: None,
            pin: Some(pin()),
            spent: children.len() as u32,
            children,
            observed_terminal: 0,
            consecutive_failures: 0,
            state: JobState::Running,
        }
    }

    fn child(slot: &str, attempt: u32, state: JobChildState) -> StepChild {
        StepChild {
            retry_key: job_spawn_key("row", slot, attempt),
            slot: slot.to_owned(),
            attempt,
            target: pair("a"),
            state,
            messages: 0,
        }
    }

    fn spawn(slot: &str, attempt: u32, target: &str) -> JobSpawn {
        JobSpawn {
            slot: slot.to_owned(),
            attempt,
            target: target.to_owned(),
            prompt: String::new(),
            title: String::new(),
            cwd: None,
        }
    }

    fn effects(spawn: Vec<JobSpawn>, messages: &[&str]) -> JobStepOutput {
        JobStepOutput::Effects {
            effects: JobEffects {
                pin: None,
                resume: None,
                spawn,
                message: messages
                    .iter()
                    .map(|slot| JobMessage {
                        slot: (*slot).to_owned(),
                        text: String::new(),
                    })
                    .collect(),
                interrupt: Vec::new(),
                progress: None,
                writes: Vec::new(),
                scratch: Vec::new(),
            },
        }
    }

    fn result(control: &[&str]) -> JobStepOutput {
        JobStepOutput::Result {
            result: serde_json::Value::Null,
            writes: Vec::new(),
            scratch: Vec::new(),
            progress: None,
            control: control
                .iter()
                .map(|row| JobControlRequest {
                    row_doc_id: (*row).to_owned(),
                    kind: JobControlKind::Stop,
                })
                .collect(),
            pin: None,
            resume: None,
        }
    }

    fn fail() -> JobStepOutput {
        JobStepOutput::Fail {
            fail: JobFailure {
                code: String::new(),
                message: String::new(),
            },
            writes: Vec::new(),
            progress: None,
        }
    }

    fn suspend() -> JobStepOutput {
        JobStepOutput::Suspend {
            suspend: JobSuspend {
                kind: JobSuspendKind::Pause,
                reason: None,
                message: String::new(),
                question: None,
            },
            writes: Vec::new(),
            scratch: Vec::new(),
            progress: None,
            pin: None,
            resume: None,
        }
    }

    fn code(ctx: &StepContext, job: &StepRow, out: &JobStepOutput) -> Option<&'static str> {
        admit_step(ctx, job, out)
            .err()
            .and_then(|refusal| refusal.code())
    }

    const DENIED: Option<&str> = Some("plugin_job_policy_denied");

    #[test]
    fn refused_terminal_row_is_a_no_op() {
        let job = StepRow {
            state: JobState::Completed,
            ..running_row(vec![child("s1", 0, JobChildState::Live)])
        };
        let refusal = admit_step(&base_context(), &job, &effects(Vec::new(), &[])).unwrap_err();
        assert_eq!(refusal.code(), None);
        assert_eq!(refusal.closed_row(), &job);
        assert!(refusal.kills().is_empty());
    }

    #[test]
    fn missing_live_ceiling_is_not_available_and_kills() {
        let job = running_row(vec![
            child("s1", 0, JobChildState::Live),
            child("s2", 0, JobChildState::Completed),
        ]);
        let out = effects(vec![spawn("s3", 0, "a")], &[]);
        for ctx in [
            StepContext {
                available: false,
                ..base_context()
            },
            StepContext {
                engine_provenance_operator: false,
                ..base_context()
            },
        ] {
            let refusal = admit_step(&ctx, &job, &out).unwrap_err();
            assert_eq!(refusal.code(), Some("plugin_job_not_available"));
            assert_eq!(refusal.closed_row().state, JobState::Failed);
            assert_eq!(refusal.kills(), [job_spawn_key("row", "s1", 0)]);
        }
    }

    #[test]
    fn engine_at_another_digest_is_engine_changed_and_kills() {
        let job = running_row(vec![child("s1", 0, JobChildState::Live)]);
        let ctx = StepContext {
            engine_digest: "other".to_owned(),
            ..base_context()
        };
        let refusal = admit_step(&ctx, &job, &effects(Vec::new(), &[])).unwrap_err();
        assert_eq!(refusal.code(), Some("plugin_job_engine_changed"));
        assert_eq!(refusal.closed_row().state, JobState::Failed);
        assert_eq!(refusal.kills(), [job_spawn_key("row", "s1", 0)]);
    }

    #[test]
    fn effective_ceiling_is_live_intersect_pinned() {
        let mut job = running_row(vec![child("s1", 0, JobChildState::Live)]);
        job.pin.as_mut().unwrap().ceiling_at_pin.max_live = 3;
        let out = effects(vec![spawn("s2", 0, "a")], &[]);
        let mut ctx = base_context();
        ctx.live.max_live = 1;
        assert_eq!(code(&ctx, &job, &out), DENIED);
        ctx.live.max_live = 2;
        assert!(admit_step(&ctx, &job, &out).is_ok());

        let mut pinned = job.clone();
        pinned.pin.as_mut().unwrap().ceiling_at_pin.max_live = 1;
        let mut ctx = base_context();
        ctx.live.max_live = 3;
        assert_eq!(code(&ctx, &pinned, &out), DENIED);
        pinned.pin.as_mut().unwrap().ceiling_at_pin.max_live = 2;
        assert!(admit_step(&ctx, &pinned, &out).is_ok());

        let two = effects(vec![spawn("s2", 0, "a"), spawn("s3", 0, "a")], &[]);
        let mut pinned = running_row(Vec::new());
        pinned.pin.as_mut().unwrap().ceiling_at_pin.max_spawns = 1;
        let ctx = base_context();
        assert_eq!(code(&ctx, &pinned, &two), DENIED);
        pinned.pin.as_mut().unwrap().ceiling_at_pin.max_spawns = 2;
        assert!(admit_step(&ctx, &pinned, &two).is_ok());

        let twice = effects(Vec::new(), &["s1", "s1"]);
        let mut pinned = job.clone();
        pinned
            .pin
            .as_mut()
            .unwrap()
            .ceiling_at_pin
            .max_messages_per_slot = 1;
        assert_eq!(code(&ctx, &pinned, &twice), DENIED);
        pinned
            .pin
            .as_mut()
            .unwrap()
            .ceiling_at_pin
            .max_messages_per_slot = 2;
        assert!(admit_step(&ctx, &pinned, &twice).is_ok());
    }

    #[test]
    fn spawn_target_must_be_in_live_pinned_and_caller_ceiling() {
        let job = running_row(Vec::new());
        let out = effects(vec![spawn("s1", 0, "a")], &[]);

        let mut ctx = base_context();
        ctx.live.targets = vec!["b".to_owned()];
        assert_eq!(code(&ctx, &job, &out), DENIED);

        let mut pinned = job.clone();
        pinned.pin.as_mut().unwrap().ceiling_at_pin.targets = vec!["b".to_owned()];
        assert_eq!(code(&base_context(), &pinned, &out), DENIED);

        let ctx = StepContext {
            job_targets: EndpointScope::<(String, String), ()>::only_units([pair("b")]),
            ..base_context()
        };
        assert_eq!(code(&ctx, &job, &out), DENIED);

        let ctx = StepContext {
            caller_targets: EndpointScope::None,
            ..base_context()
        };
        assert_eq!(code(&ctx, &job, &out), DENIED);

        let ctx = StepContext {
            target_ids: vec![("b".to_owned(), pair("b"))],
            ..base_context()
        };
        assert_eq!(code(&ctx, &job, &out), DENIED);

        let admitted = admit_step(&base_context(), &job, &out).unwrap();
        assert_eq!(admitted.children.len(), 1);
        let new = &admitted.children[0];
        assert_eq!(new.state, JobChildState::Live);
        assert_eq!(new.target, pair("a"));
        assert_eq!(new.retry_key, job_spawn_key("row", "s1", 0));
        assert_eq!(admitted.spent, 1);
    }

    #[test]
    fn dynamic_spawn_target_must_be_in_dynamic_targets() {
        let mut job = running_row(vec![child("s1", 0, JobChildState::Live)]);
        job.pin.as_mut().unwrap().origin = JobOrigin::Dynamic;
        let fresh = effects(vec![spawn("s2", 0, "a")], &[]);

        let mut ctx = base_context();
        ctx.policy.dynamic_targets = vec!["b".to_owned()];
        assert_eq!(code(&ctx, &job, &fresh), DENIED);
        ctx.policy.dynamic_targets = vec!["a".to_owned()];
        assert!(admit_step(&ctx, &job, &fresh).is_ok());

        ctx.policy.dynamic = DynamicMode::Check;
        assert_eq!(code(&ctx, &job, &fresh), DENIED);
        assert_eq!(code(&ctx, &job, &effects(Vec::new(), &["s1"])), DENIED);
        let reserved = effects(vec![spawn("s1", 0, "a")], &[]);
        assert!(admit_step(&ctx, &job, &reserved).is_ok());

        ctx.policy.dynamic = DynamicMode::Off;
        assert_eq!(code(&ctx, &job, &reserved), DENIED);
        assert_eq!(code(&ctx, &job, &suspend()), DENIED);
    }

    #[test]
    fn only_new_keys_are_charged_and_reserved_keys_are_no_ops() {
        let job = running_row(vec![child("s1", 0, JobChildState::Live)]);
        let out = effects(
            vec![
                spawn("s1", 0, "a"),
                spawn("s2", 0, "a"),
                spawn("s2", 0, "b"),
            ],
            &[],
        );
        let admitted = admit_step(&base_context(), &job, &out).unwrap();
        assert_eq!(admitted.children.len(), 2);
        assert_eq!(
            admitted.children[1].retry_key,
            job_spawn_key("row", "s2", 0)
        );
        assert_eq!(admitted.spent, job.spent + 1);
    }

    #[test]
    fn resume_reattempts_only_a_terminal_interrupted_slot() {
        let ctx = base_context();
        let open = running_row(Vec::new());
        assert!(admit_step(&ctx, &open, &effects(vec![spawn("s1", 0, "a")], &[])).is_ok());
        assert_eq!(
            code(&ctx, &open, &effects(vec![spawn("s1", 1, "a")], &[])),
            DENIED
        );

        let interrupted = running_row(vec![child("s1", 0, JobChildState::Interrupted)]);
        assert!(admit_step(&ctx, &interrupted, &effects(vec![spawn("s1", 1, "a")], &[])).is_ok());
        assert_eq!(
            code(&ctx, &interrupted, &effects(vec![spawn("s1", 2, "a")], &[])),
            DENIED
        );

        for state in [
            JobChildState::Live,
            JobChildState::Completed,
            JobChildState::Failed,
            JobChildState::Other,
        ] {
            let job = running_row(vec![child("s1", 0, state)]);
            assert_eq!(
                code(&ctx, &job, &effects(vec![spawn("s1", 1, "a")], &[])),
                DENIED
            );
        }
    }

    #[test]
    fn budget_live_and_message_caps_refuse_all_or_nothing_with_policy_denied() {
        let job = running_row(vec![child("s1", 0, JobChildState::Completed)]);
        let two = effects(vec![spawn("s2", 0, "a"), spawn("s3", 0, "a")], &[]);

        let mut budget = job.clone();
        budget.pin.as_mut().unwrap().agent_budget = 2;
        let refusal = admit_step(&base_context(), &budget, &two).unwrap_err();
        assert_eq!(refusal.code(), DENIED);
        assert_eq!(refusal.closed_row().children, budget.children);

        let mut ctx = base_context();
        ctx.live.max_spawns = 2;
        assert_eq!(code(&ctx, &job, &two), DENIED);

        let mut ctx = base_context();
        ctx.live.max_live = 1;
        let refusal = admit_step(&ctx, &job, &two).unwrap_err();
        assert_eq!(refusal.code(), DENIED);
        assert_eq!(refusal.closed_row().children, job.children);

        let mut messaged = job.clone();
        messaged.children[0].messages = 1;
        assert_eq!(
            code(
                &base_context(),
                &messaged,
                &effects(Vec::new(), &["s1", "s1"])
            ),
            DENIED
        );
        assert_eq!(
            code(&base_context(), &job, &effects(Vec::new(), &["s9"])),
            DENIED
        );

        let job = running_row(vec![
            child("s1", 0, JobChildState::Completed),
            child("s2", 0, JobChildState::Completed),
        ]);
        let admitted = admit_step(&base_context(), &job, &effects(Vec::new(), &["s1"])).unwrap();
        assert_eq!(admitted.children[0].messages, 1);
        assert_eq!(admitted.children[0].state, JobChildState::Live);
        assert_eq!(admitted.children[1].state, JobChildState::Completed);
        assert_eq!(admitted.observed_terminal, 1);
        assert_eq!(admitted.spent, job.spent);
    }

    #[test]
    fn terminal_output_with_a_live_child_is_policy_denied_with_a_kill() {
        let ctx = base_context();
        let live = running_row(vec![child("s1", 0, JobChildState::Live)]);
        for out in [result(&[]), fail(), suspend()] {
            let refusal = admit_step(&ctx, &live, &out).unwrap_err();
            assert_eq!(refusal.code(), DENIED);
            assert_eq!(refusal.kills(), [job_spawn_key("row", "s1", 0)]);
        }

        let done = running_row(vec![child("s1", 0, JobChildState::Completed)]);
        for (out, state) in [
            (result(&[]), JobState::Completed),
            (suspend(), JobState::Completed),
            (fail(), JobState::Failed),
        ] {
            assert_eq!(admit_step(&ctx, &done, &out).unwrap().state, state);
        }
        assert_eq!(code(&ctx, &done, &result(&["other"])), DENIED);
    }

    #[test]
    fn step0_pin_is_host_pinned_and_gated() {
        let job = StepRow {
            pin: None,
            consecutive_failures: 2,
            ..running_row(Vec::new())
        };
        let guest = JobGuestPin {
            script_id: "script".to_owned(),
            source_digest: "source".to_owned(),
            agent_budget: 5,
            handle: "guest-handle".to_owned(),
        };
        let JobStepOutput::Effects { effects: mut step0 } = effects(vec![spawn("s1", 0, "a")], &[])
        else {
            unreachable!()
        };
        step0.pin = Some(guest.clone());
        let out = JobStepOutput::Effects { effects: step0 };

        let ctx = base_context();
        let admitted = admit_step(&ctx, &job, &out).unwrap();
        assert_eq!(admitted.pin, Some(host_pin(&guest, &ctx.adoption_facts)));
        assert_eq!(admitted.consecutive_failures, 0);
        assert_eq!(admitted.children.len(), 1);

        let empty = StepContext {
            live: JobCeiling {
                targets: Vec::new(),
                max_live: 0,
                max_spawns: 0,
                max_messages_per_slot: 0,
                result_bytes: 0,
                max_wall_secs: 0,
            },
            ..base_context()
        };
        assert_eq!(code(&empty, &job, &out), Some("plugin_job_not_available"));

        let busy = StepContext {
            live_background: 8,
            ..base_context()
        };
        assert_eq!(
            code(&busy, &job, &out),
            Some("background_tool_budget_exceeded")
        );

        let nested = StepContext {
            started_by_job: true,
            ..base_context()
        };
        assert_eq!(code(&nested, &job, &out), Some("plugin_job_depth_exceeded"));

        let untrusted = StepContext {
            installed_trusted_sources: Vec::new(),
            ..base_context()
        };
        assert_eq!(code(&untrusted, &job, &out), DENIED);
    }

    #[test]
    fn pinless_step0_admits_terminals_and_refuses_effects_and_unauthorized_control() {
        let job = StepRow {
            pin: None,
            ..running_row(Vec::new())
        };
        let ctx = base_context();
        for out in [result(&[]), fail(), suspend()] {
            assert_eq!(admit_step(&ctx, &job, &out), Ok(job.clone()));
        }
        assert_eq!(
            code(&ctx, &job, &effects(vec![spawn("s1", 0, "a")], &[])),
            DENIED
        );

        let stop = result(&["other"]);
        assert_eq!(code(&ctx, &job, &stop), DENIED);
        for (owner, same_plugin, admitted) in [
            (scope("elsewhere"), true, false),
            (scope("session"), false, false),
            (scope("session"), true, true),
        ] {
            let ctx = StepContext {
                control_targets: vec![("other".to_owned(), Some((owner, same_plugin)))],
                ..base_context()
            };
            assert_eq!(admit_step(&ctx, &job, &stop).is_ok(), admitted);
        }

        let mut off = base_context();
        off.policy.dynamic = DynamicMode::Off;
        let call = |argv: &str, script: Option<&str>, path: Option<&str>| StepRow {
            call: StepCall {
                argv: vec![argv.to_owned()],
                args: None,
                path: path.map(str::to_owned),
                script: script.map(str::to_owned),
                agent_budget: None,
            },
            ..job.clone()
        };
        assert_eq!(
            code(&off, &call("run", Some("inline"), None), &result(&[])),
            DENIED
        );
        assert_eq!(
            code(&off, &call("check", None, Some("a.rhai")), &result(&[])),
            DENIED
        );
        assert!(admit_step(&off, &call("run", None, None), &result(&[])).is_ok());
        let named = StepContext {
            named_origin: Some(JobOrigin::Dynamic),
            ..off.clone()
        };
        assert_eq!(
            code(&named, &call("import", None, None), &result(&[])),
            DENIED
        );
        assert!(admit_step(&named, &call("status", None, None), &result(&[])).is_ok());
    }

    fn pinless() -> StepRow {
        StepRow {
            pin: None,
            children: Vec::new(),
            spent: 0,
            ..running_row(Vec::new())
        }
    }

    fn resume_of(row: &str) -> JobResume {
        JobResume {
            row_doc_id: row.to_owned(),
            engine: None,
            answer: None,
            budget: None,
        }
    }

    fn guest() -> JobGuestPin {
        JobGuestPin {
            script_id: "script".to_owned(),
            source_digest: "source".to_owned(),
            agent_budget: 10,
            handle: "guest-handle".to_owned(),
        }
    }

    fn effects_riders(
        spawn: Vec<JobSpawn>,
        pin: Option<JobGuestPin>,
        resume: Option<JobResume>,
        interrupt: &[&str],
    ) -> JobStepOutput {
        let JobStepOutput::Effects { mut effects } = effects(spawn, &[]) else {
            unreachable!()
        };
        effects.pin = pin;
        effects.resume = resume;
        effects.interrupt = interrupt
            .iter()
            .map(|slot| JobInterrupt {
                slot: (*slot).to_owned(),
            })
            .collect();
        JobStepOutput::Effects { effects }
    }

    fn valid_resume() -> (StepContext, StepRow, JobStepOutput, StepRow, StepRow) {
        let mut prior = running_row(vec![child("s1", 0, JobChildState::Live)]);
        prior.row_doc_id = "prior".to_owned();
        prior.state = JobState::Completed;
        prior.spent = 4;
        let mut root_pin = pin();
        root_pin.handle = "pin-handle".to_owned();
        root_pin.args_digest = "root-args".to_owned();
        root_pin.chain_root_doc_id = "chain-doc".to_owned();
        let mut root = running_row(Vec::new());
        root.handle = "chain-handle".to_owned();
        root.state = JobState::Completed;
        root.pin = Some(root_pin);
        root.call = StepCall {
            argv: vec!["run".to_owned(), "saved".to_owned()],
            args: Some("{}".to_owned()),
            path: Some("a.rhai".to_owned()),
            script: None,
            agent_budget: Some(3),
        };
        let mut resume_facts = facts();
        resume_facts.resume_of = Some("prior".to_owned());
        resume_facts.plugin_ref = "ns/resume".to_owned();
        resume_facts.cwd = "/resume".to_owned();
        resume_facts.adopted_at = 9;
        resume_facts.replayed_from = Some("replay".to_owned());
        resume_facts.ceiling_at_pin.max_live = 7;
        let ctx = StepContext {
            resume_facts,
            resume_targets: vec![(
                "prior".to_owned(),
                true,
                Some((scope("session"), true, prior.clone(), root.clone())),
            )],
            ..base_context()
        };
        let job = StepRow {
            root: "new".to_owned(),
            handle: "new-handle".to_owned(),
            row_doc_id: "new".to_owned(),
            ..pinless()
        };
        let out = effects_riders(
            vec![spawn("s1", 0, "a")],
            Some(guest()),
            Some(resume_of("prior")),
            &[],
        );
        (ctx, job, out, prior, root)
    }

    #[test]
    fn hop_overflow_refuses_with_depth_exceeded() {
        let job = StepRow {
            caller_hop: 3,
            ..running_row(Vec::new())
        };
        let out = effects(vec![spawn("s1", 0, "a")], &[]);
        let mut ctx = base_context();
        ctx.max_hop = Some(3);
        assert_eq!(code(&ctx, &job, &out), Some("plugin_job_depth_exceeded"));
        ctx.max_hop = Some(4);
        assert!(admit_step(&ctx, &job, &out).is_ok());
    }

    #[test]
    fn spawn_cwd_outside_the_pinned_cwd_is_refused() {
        let job = running_row(Vec::new());
        let mut step = spawn("s1", 0, "a");
        step.cwd = Some("/other".to_owned());
        let out = effects(vec![step], &[]);
        assert_eq!(code(&base_context(), &job, &out), DENIED);
        let mut ctx = base_context();
        ctx.cwd_allowed = vec!["/other".to_owned()];
        assert_eq!(code(&ctx, &job, &out), DENIED);
        ctx.within = vec![("/elsewhere".to_owned(), "/other".to_owned())];
        assert_eq!(code(&ctx, &job, &out), DENIED);
        let mut ctx = base_context();
        ctx.within = vec![("/work".to_owned(), "/other".to_owned())];
        assert_eq!(code(&ctx, &job, &out), DENIED);
        ctx.cwd_allowed = vec!["/other".to_owned()];
        assert!(admit_step(&ctx, &job, &out).is_ok());
    }

    #[test]
    fn absent_spawn_cwd_means_the_caller_cwd() {
        let admitted = admit_step(
            &base_context(),
            &running_row(Vec::new()),
            &effects(vec![spawn("s1", 0, "a")], &[]),
        )
        .unwrap();
        assert_eq!(admitted.children.len(), 1);
    }

    #[test]
    fn slot_over_128_bytes_and_interrupt_of_an_unknown_slot_are_policy_denied() {
        let job = running_row(vec![child("s1", 0, JobChildState::Live)]);
        let ctx = base_context();
        assert_eq!(
            code(
                &ctx,
                &job,
                &effects(vec![spawn(&"s".repeat(129), 0, "a")], &[])
            ),
            DENIED
        );
        assert_eq!(
            code(
                &ctx,
                &job,
                &effects_riders(Vec::new(), None, None, &["missing"])
            ),
            DENIED
        );
        assert!(admit_step(&ctx, &job, &effects_riders(Vec::new(), None, None, &["s1"])).is_ok());
    }

    #[test]
    fn resume_is_admitted_only_beside_a_pin_on_step_0_naming_a_valid_chain_head() {
        let pinned = effects_riders(Vec::new(), None, Some(resume_of("prior")), &[]);
        assert_eq!(
            code(&base_context(), &running_row(Vec::new()), &pinned),
            DENIED
        );
        let mut bare = result(&[]);
        if let JobStepOutput::Result { resume, .. } = &mut bare {
            *resume = Some(resume_of("prior"));
        }
        assert_eq!(code(&base_context(), &pinless(), &bare), DENIED);

        let (ctx, job, out, _, _) = valid_resume();
        assert!(admit_step(&ctx, &job, &out).is_ok());
        let pinned_job = StepRow {
            pin: Some(pin()),
            ..job
        };
        assert_eq!(code(&ctx, &pinned_job, &out), DENIED);

        let (mut ctx, job, out, _, _) = valid_resume();
        ctx.resume_targets.clear();
        assert_eq!(code(&ctx, &job, &out), Some("plugin_job_resume_unknown"));
        let (mut ctx, job, out, _, _) = valid_resume();
        ctx.resume_targets[0].1 = false;
        assert_eq!(code(&ctx, &job, &out), Some("plugin_job_resume_not_head"));
        ctx.caller_scope.session_id = "other-session".into();
        assert_eq!(code(&ctx, &job, &out), DENIED);
        let (mut ctx, job, out, prior, mut root) = valid_resume();
        root.pin = None;
        ctx.resume_targets[0].2 = Some((scope("session"), true, prior.clone(), root));
        assert_eq!(code(&ctx, &job, &out), DENIED);

        let (ctx, job, out, prior, root) = valid_resume();
        let JobStepOutput::Effects { effects } = &out else {
            unreachable!()
        };
        let guest = effects.pin.as_ref().unwrap();
        let block = effects.resume.as_ref().unwrap();
        assert!(resume_valid(
            &ctx.caller_scope,
            &scope("session"),
            true,
            &prior,
            guest,
            &ctx.resume_facts,
            block,
        ));
        for (owner, same_plugin, prior_row, facts_of, script, budget) in [
            (
                scope("elsewhere"),
                true,
                prior.clone(),
                "prior",
                "script",
                10,
            ),
            (
                scope("session"),
                false,
                prior.clone(),
                "prior",
                "script",
                10,
            ),
            (
                scope("session"),
                true,
                StepRow {
                    state: JobState::Running,
                    ..prior.clone()
                },
                "prior",
                "script",
                10,
            ),
            (scope("session"), true, prior.clone(), "prior", "other", 10),
            (scope("session"), true, prior.clone(), "prior", "script", 9),
            (scope("session"), true, prior.clone(), "other", "script", 10),
        ] {
            let mut ctx = ctx.clone();
            ctx.resume_facts.resume_of = Some(facts_of.to_owned());
            ctx.resume_targets[0].2 =
                Some((owner.clone(), same_plugin, prior_row.clone(), root.clone()));
            let mut refused = out.clone();
            if let JobStepOutput::Effects { effects } = &mut refused {
                effects.pin.as_mut().unwrap().script_id = script.to_owned();
                effects.pin.as_mut().unwrap().agent_budget = budget;
            }
            assert_eq!(code(&ctx, &job, &refused), DENIED);
            assert!(!resume_valid(
                &ctx.caller_scope,
                &owner,
                same_plugin,
                &prior_row,
                refused_pin(&refused),
                &ctx.resume_facts,
                block,
            ));
        }
        let (mut ctx, job, out, _, _) = valid_resume();
        ctx.resume_facts.engine_digest = "other".to_owned();
        assert_eq!(code(&ctx, &job, &out), DENIED);
    }

    fn refused_pin(out: &JobStepOutput) -> &JobGuestPin {
        let JobStepOutput::Effects { effects } = out else {
            unreachable!()
        };
        effects.pin.as_ref().unwrap()
    }

    #[test]
    fn validated_resume_continues_the_chain_row_and_pins_the_resume_pin() {
        let (ctx, job, out, prior, root) = valid_resume();
        let admitted = admit_step(&ctx, &job, &out).unwrap();
        assert_eq!(admitted.children, prior.children);
        assert_eq!(admitted.spent, prior.spent);
        assert_eq!(admitted.root, root.root);
        assert_eq!(admitted.handle, root.handle);
        assert_eq!(
            admitted.root_call,
            Some(RootCall {
                argv: root.call.argv.clone(),
                args: root.call.args.clone(),
                path: root.call.path.clone(),
                agent_budget: root.call.agent_budget,
            })
        );
        assert_eq!(
            admitted.resume.as_ref().map(|r| r.row_doc_id.as_str()),
            Some("prior")
        );
        let root_pin = root.pin.as_ref().unwrap();
        let g = guest();
        assert_eq!(
            admitted.pin,
            Some(JobPin {
                script_id: g.script_id,
                source_digest: g.source_digest,
                agent_budget: g.agent_budget,
                handle: root_pin.handle.clone(),
                args_digest: root_pin.args_digest.clone(),
                chain_root_doc_id: root_pin.chain_root_doc_id.clone(),
                origin: ctx.resume_facts.origin,
                engine_digest: ctx.resume_facts.engine_digest.clone(),
                plugin_ref: ctx.resume_facts.plugin_ref.clone(),
                cwd: ctx.resume_facts.cwd.clone(),
                adopted_at: ctx.resume_facts.adopted_at,
                resume_of: ctx.resume_facts.resume_of.clone(),
                replayed_from: ctx.resume_facts.replayed_from.clone(),
                ceiling_at_pin: ctx.resume_facts.ceiling_at_pin.clone(),
            })
        );
    }

    #[test]
    fn stall_needs_no_live_child_no_unobserved_terminal_and_no_step_new() {
        assert!(is_stalled(0, 0, 0, 0));
        assert!(!is_stalled(1, 0, 0, 0));
        assert!(!is_stalled(0, 0, 1, 0));
        assert!(!is_stalled(0, 0, 0, 1));
        let job = running_row(Vec::new());
        let refusal = admit_step(&base_context(), &job, &effects(Vec::new(), &[])).unwrap_err();
        assert_eq!(refusal.code(), Some("plugin_job_stalled"));
        assert_eq!(refusal.closed_row().state, JobState::Failed);
    }

    #[test]
    fn interrupt_alone_is_not_new_and_the_step_stalls() {
        let ctx = base_context();
        let live = running_row(vec![child("s1", 0, JobChildState::Live)]);
        assert!(admit_step(
            &ctx,
            &live,
            &effects_riders(Vec::new(), None, None, &["s1"])
        )
        .is_ok());
        let done = StepRow {
            observed_terminal: 1,
            ..running_row(vec![child("s1", 0, JobChildState::Completed)])
        };
        assert_eq!(
            code(
                &ctx,
                &done,
                &effects_riders(Vec::new(), None, None, &["s1"])
            ),
            Some("plugin_job_stalled")
        );
    }

    #[test]
    fn admitted_row_carries_the_post_admission_terminal_count_and_step_new_counts_fresh_spawns_and_messages(
    ) {
        let job = running_row(vec![
            child("s1", 0, JobChildState::Completed),
            child("s2", 0, JobChildState::Completed),
        ]);
        let out = effects(vec![spawn("s3", 0, "a")], &["s1"]);
        assert_eq!(step_new(&base_context(), &job, &out), 2);
        let admitted = admit_step(&base_context(), &job, &out).unwrap();
        assert_eq!(admitted.observed_terminal, 1);
        assert_eq!(admitted.children[0].state, JobChildState::Live);
        assert_eq!(admitted.children[1].state, JobChildState::Completed);
    }
}
