//! Step 0 of a job tool call. It runs in the foreground inside the owned
//! loop; an answer that is not a job ends the call there, and a refusal
//! completes it failed with the host code and the call that follows.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use gents_loop::live_output::DeferredToolWrites;
use gents_protocol::job::{
    JobBudget, JobControlKind, JobOrigin, JobPin, JobProgress, JobState, JobStepInput,
    JobStepOutput,
};
use gents_protocol::plugin::PluginExecutionReceipt;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub use super::admit::host_pin;
use super::admit::{
    admit_step, fresh_spawns, out_pin, out_resume, step_row, AdmitRefusal, HostFacts, Refusal,
    StepContext, StepRow,
};
use super::control::{kill, resume_target_in_txn, validate_resume, KillCause};
use super::driver::{
    guest_state_of, progress_of, read_step_row, read_step_row_in_txn, scratch_of,
    settle_plugin_job_in_txn, settlement_of, step_context_for, terminal_text, writes_of, JobDriver,
};
use super::effects::{stage_effects_in_txn, write_scratch, JobContext};
use super::input::{build_step_input, CallArgs, StepKind};
use super::origin::{call_origin, engine_guard, engine_guard_next};
use super::records::{
    read_step_state, write_pin_in_txn, write_progress_in_txn, write_step_state_in_txn,
    HostProgressFacts,
};
use super::settings::effective_ceiling;
use super::JobBinding;
use crate::background_tools::ProcessControlScope;
use crate::config_client::ConfigAccess;
use crate::graphql::graphql_string_list_literal;
use crate::hook::persistence::{
    count_live_backgrounded_rows_in, MAX_BACKGROUNDED_TOOLS_PER_PARENT,
};
use crate::hook::DefraSessionHook;
use crate::lifecycle::SessionMessageCause;
use crate::plugin::data::DataCaller;
use crate::plugin::data::StampContext;
use crate::plugin::job_calls::JobCaller;
use crate::plugin::store::InstalledPlugin;
use crate::plugin::PluginVerdict;
use crate::tool_call_lifecycle::delivery::PluginToolWrites;
use crate::tool_call_lifecycle::plugin_receipt::decide;
use crate::tool_call_lifecycle::PluginJobTerminal;
use crate::tool_call_lifecycle::{AwaitMode, ReceiptDecision, ToolRowKind};
use crate::tool_call_lifecycle::{FailureClass, ToolCallLifecycle, ToolCallState};
use crate::tool_surface::EndpointScope;

/// Runs step 0 of the running foreground job tool call `lifecycle` and ends
/// it unless the output adopts a job. Returns the payload the model reads.
pub async fn run_step0(
    hook: &DefraSessionHook,
    lifecycle: &mut ToolCallLifecycle,
    binding: &JobBinding,
    args: Value,
) -> Result<String> {
    let Some(driver) = hook.job_driver.as_ref() else {
        return refuse(
            lifecycle,
            FailureClass::ServiceUnavailable,
            None,
            "job tools are bound without the runtime's job driver",
            "Next: ask the operator to run this agent on a runtime that starts the job driver",
        )
        .await;
    };
    let runtime = driver.runtime();
    let tool = binding.tool_name.as_str();
    let coordinate = binding.plugin.plugin.as_str();
    if let Err(error) = serde_json::from_value::<CallArgs>(args) {
        return refuse(
            lifecycle,
            FailureClass::ArgumentInvalid,
            None,
            &format!("invalid {tool} arguments: {error}"),
            &format!(
                "Next: call `{tool}` again with argv as a list of strings and args as an object"
            ),
        )
        .await;
    }
    let installed = match runtime
        .plugins
        .resolve(coordinate, binding.plugin.digest.as_deref())
    {
        Ok(installed) => installed,
        Err(error) => {
            return refuse(
                lifecycle,
                FailureClass::ServiceUnavailable,
                Refusal::NotAvailable.code(),
                &format!("{error:#}"),
                &format!("Next: ask the operator to reinstall the pack that provides {coordinate}"),
            )
            .await
        }
    };
    let access = ConfigAccess::local(runtime.node.clone());
    let row_doc_id = lifecycle
        .doc_id()
        .context("a job tool call has no physical identity")?
        .to_owned();
    let (input, _) = build_step_input(&access, runtime, &row_doc_id, StepKind::Step0 { binding })
        .await
        .context("assembling the step 0 input")?;
    let caller = &input.caller;
    let data = DataCaller::new(runtime.node.clone(), &caller.node_did)?;
    let jobs = JobCaller::new(
        runtime.node.clone(),
        ProcessControlScope {
            request_id: caller.request_id.clone(),
            session_id: caller.session_id.clone(),
            node_did: caller.node_did.clone(),
            requester_did: lifecycle.requester_did().map(str::to_owned),
        },
        StampContext::ToolOutput {
            node_did: caller.node_did.clone(),
            session_id: caller.session_id.clone(),
            request_doc_id: caller.request_doc_id.clone(),
            tool_call_doc_id: row_doc_id.clone(),
        },
    )
    .with_children(input.job.chain.clone(), input.ceiling.result_bytes);
    let (call, receipt) = runtime
        .plugins
        .call_data_bound_jobs_with_receipt(
            &installed,
            serde_json::to_value(&input)?,
            runtime.tool_root.as_deref(),
            binding.plugin.tool_calls,
            Some(data),
            Some(jobs),
        )
        .await;
    lifecycle.stage_plugin_receipt(Some(receipt.clone()));
    let call = match call {
        Ok(call) if call.outcome.verdict == PluginVerdict::Success => call,
        Ok(call) => {
            return refuse(
                lifecycle,
                FailureClass::ToolReturnedError,
                None,
                &format!(
                    "plugin {} did not return a result: {}",
                    call.coordinate, call.outcome.diagnostics
                ),
                &format!("Next: call `{tool}` again with different arguments"),
            )
            .await
        }
        Err(error) => {
            return refuse(
                lifecycle,
                FailureClass::ToolReturnedError,
                None,
                &format!("{error:#}"),
                &format!("Next: call `{tool}` again with different arguments"),
            )
            .await
        }
    };
    let output: JobStepOutput = match serde_json::from_value(call.outcome.output) {
        Ok(output) => output,
        Err(error) => {
            return refuse(
                lifecycle,
                FailureClass::ToolReturnedError,
                None,
                &format!("plugin {coordinate} returned a malformed job step output: {error}"),
                &format!("Next: call `{tool}` again, or ask the operator to check {coordinate}"),
            )
            .await
        }
    };

    if out_pin(&output).is_some() {
        return adopt_step0(
            driver, lifecycle, binding, &installed, &input, &output, &receipt,
        )
        .await;
    }
    let row = read_step_row(&access, &row_doc_id).await?;
    let max_hop =
        crate::request_admission::max_request_hop(runtime.node.as_ref(), &caller.node_did).await?;
    let mut ctx = step0_context(runtime, binding, &installed, &input, lifecycle, max_hop)?;
    ctx.control_targets = control_targets(&access, coordinate, &output).await?;
    if let Err(refusal) = admit_step(&ctx, &row, &output) {
        let (class, message, next) = step0_refusal(
            refusal.refusal,
            binding,
            &installed,
            &runtime.home,
            matches!(output, JobStepOutput::Effects { .. }),
            None,
        );
        return refuse(lifecycle, class, refusal.code(), &message, &next).await;
    }
    if !scratch_of(&output).is_empty() {
        return refuse(
            lifecycle,
            FailureClass::PolicyDenied,
            Refusal::PolicyDenied.code(),
            "step 0 scratch requires a pin",
            "Next: return the scratch files with a `pin`, or finish without scratch",
        )
        .await;
    }
    let writes = writes_of(&output);
    if !writes.is_empty() {
        let Some(grant) = installed.data_grant() else {
            return refuse(
                lifecycle,
                FailureClass::PolicyDenied,
                Refusal::PolicyDenied.code(),
                &format!("plugin {coordinate} declares no data, so its answer cannot write"),
                "Next: finish without writes",
            )
            .await;
        };
        lifecycle.stage_deferred_writes(Some(DeferredToolWrites::new(PluginToolWrites {
            grant,
            writes: writes.to_vec(),
        })));
    }
    if let JobStepOutput::Result { control, .. } = &output {
        for request in control {
            let cause = match request.kind {
                JobControlKind::Stop => KillCause::Stop,
                JobControlKind::Pause => KillCause::Pause,
            };
            let (_, target) = ctx
                .control_targets
                .iter()
                .find(|(doc_id, _)| *doc_id == request.row_doc_id)
                .context("an admitted control entry has no target")?;
            let (owner, _) = target
                .as_ref()
                .context("an admitted control entry has no job row")?;
            let mut target_lifecycle = ToolCallLifecycle::load_by_doc_id(
                runtime.node.clone(),
                &request.row_doc_id,
                &owner.node_did,
                &owner.session_id,
                owner.requester_did.as_deref(),
            )
            .await?
            .with_context(|| format!("job row {} does not exist", request.row_doc_id))?;
            kill(&access, &mut target_lifecycle, cause).await?;
        }
    }
    let settlement =
        settlement_of(&output).context("an effects output without a pin was admitted")?;
    let (terminal, text) =
        terminal_text(lifecycle, settlement).context("a step 0 answer has no terminal")?;
    match terminal.state {
        ToolCallState::Completed => lifecycle.complete(&text).await?,
        _ => {
            lifecycle
                .fail(&text, FailureClass::ToolReturnedError)
                .await?
        }
    }
    if terminal.state == ToolCallState::Completed && lifecycle.state() == ToolCallState::Failed {
        return Ok(json!({
            "status": "failed",
            "message": "plugin data writes were refused and nothing was committed",
            "next": format!("Next: call `{tool}` again with writes the plugin's data grant allows"),
        })
        .to_string());
    }
    Ok(text)
}

/// The scope and plugin of each job row a pinless result's `control` names
/// (`None` when no job row has that document id), for admission to authorize.
async fn control_targets(
    access: &ConfigAccess,
    plugin: &str,
    output: &JobStepOutput,
) -> Result<Vec<(String, Option<(ProcessControlScope, bool)>)>> {
    #[derive(Deserialize)]
    struct Target {
        #[serde(rename = "_docID")]
        doc_id: String,
        session_id: String,
        node_did: String,
        #[serde(default)]
        requester_did: Option<String>,
        #[serde(default)]
        job_pin: Option<JobPin>,
    }
    let JobStepOutput::Result { control, .. } = output else {
        return Ok(Vec::new());
    };
    if control.is_empty() {
        return Ok(Vec::new());
    }
    let list = graphql_string_list_literal(control.iter().map(|r| r.row_doc_id.as_str()));
    let response = access
        .execute(&format!(
            "{{ AgentToolCall(filter: {{ _docID: {{ _in: {list} }} }}) {{ _docID session_id node_did requester_did job_pin }} }}"
        ))
        .await?;
    let rows: Vec<Target> = serde_json::from_value(
        response
            .pointer("/data/AgentToolCall")
            .cloned()
            .context("AgentToolCall query omitted rows for the control targets")?,
    )
    .context("decoding the control targets")?;
    Ok(control
        .iter()
        .map(|request| {
            let target = rows
                .iter()
                .find(|row| row.doc_id == request.row_doc_id)
                .and_then(|row| {
                    let pin = row.job_pin.as_ref()?;
                    let owner = ProcessControlScope {
                        request_id: String::new(),
                        session_id: row.session_id.clone(),
                        node_did: row.node_did.clone(),
                        requester_did: row.requester_did.clone(),
                    };
                    Some((owner, pin.plugin_ref == plugin))
                });
            (request.row_doc_id.clone(), target)
        })
        .collect())
}

/// The tool call as the adoption decision sees it (Lean `ToolCallContext`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolCallView {
    pub state: ToolCallState,
    pub kind: ToolRowKind,
    pub await_mode: AwaitMode,
}

/// Lean `Adoption.Delivery`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    Inline,
    Continuation,
}

/// What admitting a step 0 output does to its row and call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Adoption {
    pub row: StepRow,
    pub call: ToolCallView,
    pub delivery: Option<Delivery>,
}

impl Adoption {
    /// Lean `receiptDecisions`: the adopted row holds no receipt, so its
    /// terminal compare-and-set commits, and a later attempt with another
    /// receipt conflicts. `decide` is generic over the receipt, so any two
    /// unequal values stand for them.
    pub fn receipt_decisions(&self) -> (ReceiptDecision, ReceiptDecision) {
        (
            decide::<str>(false, None, "final"),
            decide::<str>(true, Some("final"), "late"),
        )
    }
}

/// Lean `adopt` and `adoptTerminal` over `admit_step`: a pinned effects
/// output flips the call to a background plugin job, a pinned result or
/// suspend also completes it and is delivered inline, and anything else
/// leaves the call alone (Lean `adoptedCall`).
pub fn adopt(
    ctx: &StepContext,
    view: &ToolCallView,
    job: &StepRow,
    out: &JobStepOutput,
) -> Result<Adoption, AdmitRefusal> {
    let row = admit_step(ctx, job, out)?;
    let job_call = ToolCallView {
        kind: ToolRowKind::PluginJob,
        await_mode: AwaitMode::Background,
        ..*view
    };
    let (call, delivery) = match out {
        _ if out_pin(out).is_none() => (*view, None),
        JobStepOutput::Effects { .. } => (job_call, None),
        _ => (
            ToolCallView {
                state: ToolCallState::Completed,
                ..job_call
            },
            Some(Delivery::Inline),
        ),
    };
    Ok(Adoption {
        row,
        call,
        delivery,
    })
}

/// Lean `uniqueHandle`: a free handle is kept, else `handle-n` for the least
/// `n >= 2` that is free.
pub fn unique_handle(taken: &[String], handle: &str) -> String {
    if !taken.iter().any(|other| other == handle) {
        return handle.to_owned();
    }
    (2u64..)
        .map(|n| format!("{handle}-{n}"))
        .find(|candidate| !taken.contains(candidate))
        .expect("a finite list leaves a free suffix")
}

/// Lean `splitAddress`: `(session_id, tool_call_id)` at the last `:`.
pub fn split_address(address: &str) -> Option<(String, String)> {
    address
        .rsplit_once(':')
        .map(|(session, call)| (session.to_owned(), call.to_owned()))
}

/// What the adoption transaction decided.
enum Adopted {
    /// With the address of the row that resumed the named resume target.
    Refused(Refusal, Option<String>),
    Settled {
        lifecycle: ToolCallLifecycle,
        terminal: PluginJobTerminal,
        payload: String,
    },
    Done {
        lifecycle: ToolCallLifecycle,
        receipt: String,
    },
}

#[derive(Deserialize)]
struct SessionPin {
    #[serde(rename = "_docID")]
    doc_id: String,
    #[serde(default)]
    job_pin: Option<Value>,
}

/// The handles of the caller session's other job rows of `tool_name`, read through `txn`.
async fn taken_handles(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    session_id: &str,
    tool_name: &str,
    row_doc_id: &str,
) -> Result<Vec<String>> {
    let response = txn
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ session_id: {{ _eq: "{}" }}, tool_name: {{ _eq: "{}" }} }}) {{ _docID job_pin }} }}"#,
            crate::graphql::escape_graphql_string(session_id),
            crate::graphql::escape_graphql_string(tool_name)
        ))
        .await?;
    let rows: Vec<SessionPin> = serde_json::from_value(
        response
            .pointer("/data/AgentToolCall")
            .cloned()
            .context("session job query omitted rows")?,
    )
    .context("decoding session job rows")?;
    Ok(rows
        .into_iter()
        .filter(|row| row.doc_id != row_doc_id)
        .filter_map(|row| serde_json::from_value::<JobPin>(row.job_pin?).ok())
        .map(|pin| pin.handle)
        .collect())
}

/// The digest of the call's C9 arguments.
fn args_digest(input: &JobStepInput) -> String {
    let call = json!({
        "argv": input.argv,
        "args": input.args,
        "script": input.script,
        "agent_budget": input.agent_budget,
    });
    format!("sha256:{:x}", Sha256::digest(call.to_string()))
}

/// Adopts the pinned step 0 `output` of the running foreground call as a
/// background job: the flip, `job_pin`, initial `job_progress`, step 0's
/// evidence, the admitted effects and the background receipt commit in one
/// transaction, which writes no execution receipt. A pinned `result` or
/// `suspend` adopts a row that is already terminal: the same transaction
/// settles it, and the settled payload is the call's own reply with no
/// Continuation completion. A refusal completes the call foreground failed.
/// Kicks `driver` after an effects commit and returns the payload the model
/// reads.
pub async fn adopt_step0(
    driver: &Arc<JobDriver>,
    lifecycle: &mut ToolCallLifecycle,
    binding: &JobBinding,
    installed: &InstalledPlugin,
    input: &JobStepInput,
    output: &JobStepOutput,
    step0_receipt: &PluginExecutionReceipt,
) -> Result<String> {
    let runtime = driver.runtime();
    let effects = match output {
        JobStepOutput::Effects { effects } => Some(effects),
        _ => None,
    };
    let guest = out_pin(output).context("an adopted output carries a pin")?;
    let (scratch, writes, resume) = (scratch_of(output), writes_of(output), out_resume(output));
    let caller = &input.caller;
    let coordinate = binding.plugin.plugin.as_str();
    let row_doc_id = lifecycle
        .doc_id()
        .context("a job tool call has no physical identity")?
        .to_owned();
    if !scratch.is_empty() {
        if let Err(error) = write_scratch(Path::new(&caller.cwd), &caller.tool_call_id, scratch) {
            return refuse(
                lifecycle,
                FailureClass::PolicyDenied,
                Refusal::PolicyDenied.code(),
                &format!("{error:#}"),
                "Next: return scratch files with plain names under the working folder, or finish without scratch",
            )
            .await;
        }
    }
    let grant = installed.data_grant();
    if !writes.is_empty() && grant.is_none() {
        return refuse(
            lifecycle,
            FailureClass::PolicyDenied,
            Refusal::PolicyDenied.code(),
            &format!("plugin {coordinate} declares no data, so its answer cannot write"),
            "Next: finish without writes",
        )
        .await;
    }
    // Admission depends on the grant and the write shape, never on a stamp's
    // value; checking here keeps a refusal out of the transaction, where an
    // error would terminate the request instead of failing the call.
    if let Some(grant) = grant.as_ref().filter(|_| !writes.is_empty()) {
        let stamps = crate::plugin::data::StampContext::JobStep {
            node_did: caller.node_did.clone(),
            session_id: caller.session_id.clone(),
            request_doc_id: caller.request_doc_id.clone(),
            tool_call_doc_id: row_doc_id.clone(),
            job_row_doc_id: row_doc_id.clone(),
            chain_root_doc_id: row_doc_id.clone(),
        };
        if crate::plugin::data::admit_write(grant, &stamps, writes.to_vec()).is_err() {
            return refuse(
                lifecycle,
                FailureClass::PolicyDenied,
                Refusal::PolicyDenied.code(),
                "plugin data writes were refused and nothing was committed",
                &format!(
                    "Next: call `{}` again with writes the plugin's data grant allows",
                    binding.tool_name
                ),
            )
            .await;
        }
    }
    let cause = SessionMessageCause {
        caller_node_did: caller.node_did.clone(),
        caller_agent_id: caller.agent_id.clone(),
        caller_session_id: caller.session_id.clone(),
        caller_request_id: caller.request_id.clone(),
        caller_request_doc_id: caller.request_doc_id.clone(),
        caller_hop: caller.request_hop,
        tool_call_id: caller.tool_call_id.clone(),
        tool_call_doc_id: row_doc_id.clone(),
        correlation: caller.correlation.clone(),
    };
    let args_digest = args_digest(input);
    let adopted = {
        let (cause, args_digest, row_doc_id, grant, lifecycle) =
            (&cause, &args_digest, &row_doc_id, &grant, &*lifecycle);
        ConfigAccess::transact_local_write(
            &runtime.node,
            Some(::identity::Did::new(caller.node_did.clone())?),
            "plugin_job.adopt_step0",
            |txn| {
                Box::pin(async move {
                    let adopted_at = u64::try_from(chrono::Utc::now().timestamp_millis())?;
                    let provisional = HostFacts {
                        engine_digest: installed.digest.clone(),
                        plugin_ref: coordinate.to_owned(),
                        cwd: caller.cwd.clone(),
                        args_digest: args_digest.clone(),
                        chain_root: row_doc_id.clone(),
                        origin: JobOrigin::Dynamic,
                        adopted_at,
                        resume_of: None,
                        replayed_from: None,
                        ceiling_at_pin: effective_ceiling(None),
                    };
                    let (mut ctx, _) = step_context_for(
                        txn,
                        runtime,
                        input,
                        provisional.clone(),
                        output,
                        lifecycle,
                    )
                    .await?;
                    ctx.started_by_job = caller.started_by_job;
                    if effects.is_some() {
                        ctx.live_background = u32::try_from(
                            count_live_backgrounded_rows_in(txn, &caller.request_doc_id).await?,
                        )
                        .unwrap_or(u32::MAX);
                    }
                    let facts = HostFacts {
                        origin: call_origin(input.script.as_deref()),
                        ceiling_at_pin: ctx.live.clone(),
                        ..provisional
                    };
                    ctx.adoption_facts = facts.clone();
                    ctx.resume_facts = facts;
                    let mut resumed_by = None;
                    if let Some(resume) = resume {
                        let (target, by) =
                            resume_target_in_txn(txn, coordinate, &resume.row_doc_id).await?;
                        ctx.resume_targets = vec![target];
                        resumed_by = by;
                        ctx.resume_facts.resume_of = Some(resume.row_doc_id.clone());
                        if let Err(refusal) = validate_resume(
                            txn,
                            &ctx.caller_scope,
                            coordinate,
                            resume,
                            guest,
                            &ctx.resume_facts,
                        )
                        .await?
                        {
                            return Ok(Adopted::Refused(refusal, resumed_by));
                        }
                    }
                    let mut out = output.clone();
                    if resume.is_none() {
                        let taken = taken_handles(
                            txn,
                            &caller.session_id,
                            lifecycle.tool_name(),
                            row_doc_id,
                        )
                        .await?;
                        if let Some(pin) = pin_mut(&mut out) {
                            pin.handle = unique_handle(&taken, &pin.handle);
                        }
                    }
                    let view = ToolCallView {
                        state: lifecycle.state(),
                        kind: lifecycle.kind(),
                        await_mode: lifecycle.await_mode(),
                    };
                    let row = read_step_row_in_txn(txn, row_doc_id).await?;
                    let adoption = match adopt(&ctx, &view, &row, &out) {
                        Ok(adoption) => adoption,
                        Err(refusal) => return Ok(Adopted::Refused(refusal.refusal, resumed_by)),
                    };
                    let pin = adoption
                        .row
                        .pin
                        .clone()
                        .context("an adopted row has a pin")?;
                    // (b) the flip, with the job's wall deadline
                    let deadline_at = chrono::DateTime::from_timestamp_millis(i64::try_from(
                        pin.adopted_at + pin.ceiling_at_pin.max_wall_secs * 1000,
                    )?)
                    .context("the job deadline is out of range")?;
                    let flipped = lifecycle
                        .adopt_plugin_job_in_txn(txn, deadline_at)
                        .await?
                        .context("the call is no longer a running foreground call")?;
                    // (c) the pin
                    write_pin_in_txn(txn, row_doc_id, &pin).await?;
                    // (e) initial progress, then step 0's evidence and resume block
                    let budget = JobBudget {
                        total: pin.agent_budget,
                        spent: adoption.row.spent,
                        remaining: pin.agent_budget.saturating_sub(adoption.row.spent),
                    };
                    let origin = serde_json::to_value(pin.origin)?;
                    let started = progress_of(output).cloned().unwrap_or_else(|| JobProgress {
                        title: pin.script_id.clone(),
                        state: JobState::Running,
                        pause_reason: None,
                        phases: Vec::new(),
                        active: Vec::new(),
                        spawned: 0,
                        budget: budget.clone(),
                        last_event: format!("origin: {}", origin.as_str().unwrap_or_default()),
                        last_event_at: None,
                        question: None,
                        observed_terminal_children: 0,
                        revision: 0,
                    });
                    let progress = write_progress_in_txn(
                        txn,
                        row_doc_id,
                        0,
                        started,
                        HostProgressFacts {
                            spawned: adoption.row.spent,
                            budget,
                            observed_terminal_children: adoption.row.observed_terminal,
                            state: JobState::Running,
                            pause_reason: None,
                        },
                    )
                    .await?;
                    // The guest state a resume continues from is the prior row's, unless step 0 wrote its own.
                    let guest = match (guest_state_of(output), adoption.row.resume.as_ref()) {
                        (Some(state), _) => Some(state.clone()),
                        (None, Some(resume)) => read_step_state(txn, &resume.row_doc_id)
                            .await?
                            .and_then(|state| state.guest),
                        (None, None) => None,
                    };
                    write_step_state_in_txn(
                        txn,
                        row_doc_id,
                        step0_receipt,
                        adoption.row.resume.as_ref(),
                        guest.as_ref(),
                    )
                    .await?;
                    // (d) step 0's data writes
                    if !writes.is_empty() {
                        let grant = grant
                            .as_ref()
                            .context("a plugin without data cannot write")?;
                        let stamps = crate::plugin::data::StampContext::JobStep {
                            node_did: caller.node_did.clone(),
                            session_id: caller.session_id.clone(),
                            request_doc_id: caller.request_doc_id.clone(),
                            tool_call_doc_id: row_doc_id.clone(),
                            job_row_doc_id: row_doc_id.clone(),
                            chain_root_doc_id: pin.chain_root_doc_id.clone(),
                        };
                        crate::application_write::apply_plugin_writes_in_txn(
                            txn,
                            grant,
                            &stamps,
                            writes.to_vec(),
                        )
                        .await?;
                    }
                    if let Some(settlement) = settlement_of(&out) {
                        // The pinned terminal settles in this transaction; the
                        // flip wrote `background`, but the settled payload is
                        // the call's own reply.
                        let inline = adoption.delivery == Some(Delivery::Inline);
                        let (_, payload) = terminal_text(&flipped, settlement.clone())
                            .context("a step 0 answer has no terminal")?;
                        let terminal = settle_plugin_job_in_txn(txn, &flipped, settlement, inline)
                            .await?
                            .context("a step 0 answer has no terminal")?;
                        return Ok(Adopted::Settled {
                            lifecycle: flipped,
                            terminal,
                            payload,
                        });
                    }
                    let effects = effects.context("only an effects output stays a job")?;
                    // (f) the admitted effects
                    let staged_row = StepRow {
                        pin: Some(pin.clone()),
                        caller_cwd: pin.cwd.clone(),
                        ..step_row(&ctx, &row, &out)
                    };
                    let fresh = fresh_spawns(&ctx, &staged_row, &out);
                    let job = JobContext {
                        ctx: &ctx,
                        row: &staged_row,
                        cause,
                    };
                    stage_effects_in_txn(txn, &job, &fresh, effects).await?;
                    // (g) the answer-first background receipt
                    let address = format!("{}:{}", caller.session_id, caller.tool_call_id);
                    let receipt = json!({
                        "status": "running",
                        "address": address,
                        "handle": pin.handle,
                        "script_id": pin.script_id,
                        "tier": progress.last_event,
                        "next": format!("Next: `gents job show {address}`"),
                    })
                    .to_string();
                    crate::tool_call_lifecycle::publish_background_receipt_in_txn(
                        txn,
                        &flipped.background_receipt_binding()?,
                        &receipt,
                    )
                    .await?;
                    Ok(Adopted::Done {
                        lifecycle: flipped,
                        receipt,
                    })
                })
            },
        )
        .await?
    };
    match adopted {
        Adopted::Refused(refusal, resumed_by) => {
            let resume = out_resume(output).map(|resume| resume.row_doc_id.as_str());
            let (class, message, next) = step0_refusal(
                refusal,
                binding,
                installed,
                &runtime.home,
                false,
                resume.map(|row| (row, resumed_by.as_deref())),
            );
            refuse(lifecycle, class, refusal.code(), &message, &next).await
        }
        Adopted::Settled {
            lifecycle: flipped,
            terminal,
            payload,
        } => {
            *lifecycle = flipped;
            lifecycle.finish_plugin_job_row(terminal).await?;
            Ok(payload)
        }
        Adopted::Done {
            lifecycle: flipped,
            receipt,
        } => {
            *lifecycle = flipped;
            driver.kick(&row_doc_id);
            Ok(receipt)
        }
    }
}

/// The guest pin of a pinned output, which the host overwrites with its facts.
fn pin_mut(out: &mut JobStepOutput) -> Option<&mut gents_protocol::job::JobGuestPin> {
    match out {
        JobStepOutput::Result { pin, .. } | JobStepOutput::Suspend { pin, .. } => pin.as_mut(),
        JobStepOutput::Effects { effects } => effects.pin.as_mut(),
        JobStepOutput::Fail { .. } => None,
    }
}

/// Completes the running call failed with `message` and the call that
/// follows; returns the payload the model reads.
async fn refuse(
    lifecycle: &mut ToolCallLifecycle,
    class: FailureClass,
    code: Option<&str>,
    message: &str,
    next: &str,
) -> Result<String> {
    let payload = json!({
        "status": "failed",
        "code": code,
        "message": message,
        "next": next,
    })
    .to_string();
    lifecycle.fail(&payload, class).await?;
    Ok(payload)
}

/// The failure class, message and next call of a step 0 refusal. `resume` is
/// the row a step-0 resume named and the address of the row that resumed it.
fn step0_refusal(
    refusal: Refusal,
    binding: &JobBinding,
    installed: &InstalledPlugin,
    home: &Path,
    effects: bool,
    resume: Option<(&str, Option<&str>)>,
) -> (FailureClass, String, String) {
    let tool = binding.tool_name.as_str();
    let coordinate = binding.plugin.plugin.as_str();
    match refusal {
        Refusal::NotAvailable if engine_guard(home, installed).is_err() => (
            FailureClass::ServiceUnavailable,
            format!("the engine pack of {coordinate} was not installed by the operator"),
            engine_guard_next(installed),
        ),
        Refusal::NotAvailable => (
            FailureClass::ServiceUnavailable,
            format!("{tool} has no job grant for this agent"),
            format!("Next: ask the operator to add `job` to integrations.plugins[{coordinate}]"),
        ),
        Refusal::DepthExceeded => (
            FailureClass::PolicyDenied,
            "a job cannot start another job".to_owned(),
            format!("Next: run `{tool}` from a session that no job started"),
        ),
        Refusal::BackgroundBudgetExceeded => (
            FailureClass::ServiceUnavailable,
            format!("{tool} cannot be admitted because the parent request has reached the concurrent background capacity ceiling ({MAX_BACKGROUNDED_TOOLS_PER_PARENT})"),
            format!("Next: wait for a background completion notification, then retry `{tool}`"),
        ),
        Refusal::DynamicCodeDisabled => {
            let (message, next) = super::driver::refusal_text(refusal, tool, "");
            (FailureClass::PolicyDenied, message.to_owned(), next)
        }
        Refusal::ResumeUnknown => (
            FailureClass::PolicyDenied,
            format!(
                "no job row {} exists to resume",
                resume.map_or("", |(row, _)| row)
            ),
            format!("Next: `gents job show <address>` with the address the run's receipt printed; the `{tool}` help says how a run is resumed"),
        ),
        Refusal::ResumeNotHead => match resume {
            Some((row, Some(address))) => (
                FailureClass::PolicyDenied,
                format!("{row} was already resumed by {address}; only a run's newest row resumes"),
                format!("Next: `gents job show {address}`; the `{tool}` help says how a run is resumed"),
            ),
            _ => (
                FailureClass::PolicyDenied,
                "that row was already resumed; only a run's newest row resumes".to_owned(),
                format!("Next: `gents job show <address>` with the newest row's address; the `{tool}` help says how a run is resumed"),
            ),
        },
        Refusal::PolicyDenied if effects => (
            FailureClass::PolicyDenied,
            "step 0 effects require a pin".to_owned(),
            "Next: return the effects with a `pin`, or finish with a result".to_owned(),
        ),
        _ => (
            FailureClass::PolicyDenied,
            "the call asked for something its job policy refuses".to_owned(),
            format!(
                "Next: change the call to fit `job.policy`, or ask the operator to widen it in integrations.plugins[{coordinate}]"
            ),
        ),
    }
}

/// The facts admission decides step 0 against: the live ceiling and policy of
/// the binding, the plugin and its provenance, and the ancestry walk.
fn step0_context(
    runtime: &super::driver::JobRuntime,
    binding: &JobBinding,
    installed: &InstalledPlugin,
    input: &JobStepInput,
    lifecycle: &ToolCallLifecycle,
    max_hop: Option<u32>,
) -> Result<StepContext> {
    let caller = &input.caller;
    let policy = binding.settings.as_ref().map(|settings| &settings.policy);
    let live = effective_ceiling(binding.settings.as_ref());
    let facts = HostFacts {
        engine_digest: installed.digest.clone(),
        plugin_ref: binding.plugin.plugin.clone(),
        cwd: caller.cwd.clone(),
        args_digest: String::new(),
        chain_root: input.job.row_doc_id.clone(),
        origin: call_origin(input.script.as_deref()),
        adopted_at: 0,
        resume_of: None,
        replayed_from: None,
        ceiling_at_pin: live.clone(),
    };
    Ok(StepContext {
        live,
        available: true,
        caller_targets: EndpointScope::none(),
        job_targets: EndpointScope::none(),
        policy: policy.cloned().unwrap_or_default(),
        engine_digest: installed.digest.clone(),
        engine_provenance_operator: engine_guard(&runtime.home, installed).is_ok(),
        max_hop,
        target_hops: Vec::new(),
        target_ids: Vec::new(),
        live_background: 0,
        started_by_job: caller.started_by_job,
        cwd_allowed: Vec::new(),
        within: Vec::new(),
        adoption_facts: facts.clone(),
        caller_scope: ProcessControlScope {
            request_id: caller.request_id.clone(),
            session_id: caller.session_id.clone(),
            node_did: caller.node_did.clone(),
            requester_did: lifecycle.requester_did().map(str::to_owned),
        },
        control_targets: Vec::new(),
        resume_targets: Vec::new(),
        resume_facts: facts,
    })
}

#[cfg(test)]
mod tests {
    use super::super::admit::{admit_step, StepCall};
    use super::*;
    use crate::tool_call_lifecycle::{AwaitMode, ToolRowKind};
    use gents_protocol::job::{DynamicMode, JobCeiling, JobPolicy, JobState};

    fn ceiling() -> JobCeiling {
        JobCeiling {
            targets: vec!["a".to_owned()],
            max_live: 4,
            max_spawns: 10,
            max_messages_per_slot: 2,
            result_bytes: 1024,
            max_wall_secs: 60,
        }
    }

    fn facts() -> HostFacts {
        HostFacts {
            engine_digest: "engine".to_owned(),
            plugin_ref: "ns/plugin".to_owned(),
            cwd: "/work".to_owned(),
            args_digest: "args".to_owned(),
            chain_root: "row".to_owned(),
            origin: JobOrigin::Dynamic,
            adopted_at: 1,
            resume_of: None,
            replayed_from: None,
            ceiling_at_pin: ceiling(),
        }
    }

    fn context() -> StepContext {
        StepContext {
            live: ceiling(),
            available: true,
            caller_targets: EndpointScope::All,
            job_targets: EndpointScope::All,
            policy: JobPolicy {
                dynamic: DynamicMode::Run,
                dynamic_targets: vec!["a".to_owned()],
                ..Default::default()
            },
            engine_digest: "engine".to_owned(),
            engine_provenance_operator: true,
            max_hop: Some(8),
            target_hops: vec![(("node".to_owned(), "a".to_owned()), 0)],
            target_ids: vec![("a".to_owned(), ("node".to_owned(), "a".to_owned()))],
            live_background: 0,
            started_by_job: false,
            cwd_allowed: Vec::new(),
            within: Vec::new(),
            adoption_facts: facts(),
            caller_scope: ProcessControlScope {
                request_id: "request".to_owned(),
                session_id: "session".to_owned(),
                node_did: "node".to_owned(),
                requester_did: None,
            },
            control_targets: Vec::new(),
            resume_targets: Vec::new(),
            resume_facts: facts(),
        }
    }

    fn step0_row() -> StepRow {
        StepRow {
            root: "row".to_owned(),
            row_doc_id: "row".to_owned(),
            caller_session: "session".to_owned(),
            caller_cwd: String::new(),
            handle: String::new(),
            caller_hop: 0,
            call: StepCall {
                argv: vec!["run".to_owned()],
                args: None,
                script: None,
                agent_budget: None,
            },
            root_call: None,
            resume: None,
            pin: None,
            children: Vec::new(),
            spent: 0,
            observed_terminal: 0,
            consecutive_failures: 0,
            state: JobState::Running,
        }
    }

    fn view() -> ToolCallView {
        ToolCallView {
            state: ToolCallState::Running,
            kind: ToolRowKind::Native,
            await_mode: AwaitMode::Foreground,
        }
    }

    const PIN: &str = r#""pin": {"script_id": "s", "source_digest": "sha256:s", "agent_budget": 2, "handle": "h"}"#;

    fn output(json: &str) -> JobStepOutput {
        serde_json::from_str(json).unwrap()
    }

    fn pinned_effects() -> JobStepOutput {
        output(&format!(
            r#"{{"effects": {{{PIN}, "spawn": [{{"slot": "s", "attempt": 0, "target": "a", "prompt": "p", "title": "t"}}]}}}}"#
        ))
    }

    #[test]
    fn adopt_flips_a_pinned_effects_step_to_a_background_plugin_job_with_no_delivery() {
        let out = pinned_effects();
        let adopted = adopt(&context(), &view(), &step0_row(), &out).unwrap();
        assert_eq!(adopted.call.await_mode, AwaitMode::Background);
        assert_eq!(adopted.call.kind, ToolRowKind::PluginJob);
        assert_eq!(adopted.call.state, ToolCallState::Running);
        assert_eq!(adopted.delivery, None);
        assert_eq!(
            adopted.row,
            admit_step(&context(), &step0_row(), &out).unwrap()
        );
    }

    #[test]
    fn adopt_terminalizes_a_pinned_step0_result_inline() {
        for json in [
            format!(r#"{{"result": "done", {PIN}}}"#),
            format!(
                r#"{{"suspend": {{"kind": "pause", "reason": "budget_exceeded", "message": "m"}}, {PIN}}}"#
            ),
        ] {
            let adopted = adopt(&context(), &view(), &step0_row(), &output(&json)).unwrap();
            assert_eq!(adopted.call.state, ToolCallState::Completed, "{json}");
            assert_eq!(adopted.call.await_mode, AwaitMode::Background, "{json}");
            assert_eq!(adopted.call.kind, ToolRowKind::PluginJob, "{json}");
            assert_eq!(adopted.delivery, Some(Delivery::Inline), "{json}");
        }
    }

    #[test]
    fn pinless_or_refused_step0_leaves_the_call() {
        let adopted = adopt(
            &context(),
            &view(),
            &step0_row(),
            &output(r#"{"result": "done"}"#),
        )
        .unwrap();
        assert_eq!(adopted.call, view());
        assert_eq!(adopted.delivery, None);

        let full = StepContext {
            live_background: 8,
            ..context()
        };
        let refusal = adopt(&full, &view(), &step0_row(), &pinned_effects()).unwrap_err();
        assert_eq!(refusal.code(), Some("background_tool_budget_exceeded"));
    }

    #[test]
    fn receipt_decisions_commit_once_then_conflict() {
        let adopted = adopt(&context(), &view(), &step0_row(), &pinned_effects()).unwrap();
        let (terminal, late) = adopted.receipt_decisions();
        assert_eq!(
            (terminal, late),
            (ReceiptDecision::Commit, ReceiptDecision::Conflict)
        );
        assert_eq!(
            (terminal.spelling(), late.spelling()),
            ("commit", "conflict")
        );
    }

    #[test]
    fn unique_handle_keeps_a_free_handle_and_suffixes_the_least_free_n() {
        assert_eq!(unique_handle(&[], "h"), "h");
        assert_eq!(unique_handle(&["x".to_owned()], "h"), "h");
        assert_eq!(unique_handle(&["h".to_owned()], "h"), "h-2");
        assert_eq!(
            unique_handle(&["h".to_owned(), "h-2".to_owned()], "h"),
            "h-3"
        );
    }

    #[test]
    fn split_address_splits_at_the_last_colon() {
        assert_eq!(
            split_address("job-session:a:b:call1"),
            Some(("job-session:a:b".to_owned(), "call1".to_owned()))
        );
        assert_eq!(split_address("nocolon"), None);
    }
}
