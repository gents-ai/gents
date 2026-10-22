//! Ends a job row: maps a [`Settlement`] onto the row's terminal state and
//! canonical tool output.

use rapidhash::{HashMapExt, RapidHashMap};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use gents_protocol::job::{
    DataWrite, JobControlKind, JobFailure, JobPauseReason, JobPin, JobProgress, JobScratch,
    JobState, JobStepInput, JobStepOutput, JobSuspend, JobSuspendKind,
};
use gents_protocol::plugin::PluginExecutionReceipt;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::Semaphore;

use super::admit::{
    admit_step, fresh_spawns, root_call_of, HostFacts, Refusal, StepCall, StepContext, StepRow,
};
use super::control::{
    kill, kill_decision, observe_live_children, KillCause, KillObservation, KillOutcome,
};
use super::effects::{
    interrupt_slots, recover_step_commit, stage_recorded_effects_in_txn, write_scratch, JobContext,
};
use super::input::{build_step_input, chain_of, derive_children_in_txn, CallArgs, StepKind};
use super::keys::job_session_id;
use super::origin::{engine_guard, script_index, trusted_hint};
use super::records::{
    read_pin, read_progress, read_step_state, write_progress_in_txn, write_step_state_in_txn,
    HostProgressFacts, RecordError,
};
use super::settings::effective_ceiling;
use crate::background_tools::ProcessControlScope;
use crate::config_client::{ConfigAccess, ConfigApplyTxn};
use crate::graphql::escape_graphql_string;
use crate::lifecycle::SessionMessageCause;
use crate::pack::InstalledVia;
use crate::plugin::data::{DataCaller, StampContext};
use crate::plugin::executor::PluginExecutor;
use crate::plugin::job_calls::{job_state, JobCaller};
use crate::plugin::store::{parse_coordinate, read_record, InstalledPlugin};
use crate::plugin::{GuestTrap, PluginVerdict};
use crate::session_origin::SessionScope;
use crate::tool_call_lifecycle::query::load_tool_call_arguments_in_txn;
use crate::tool_call_lifecycle::{PluginJobTerminal, ToolCallLifecycle, ToolCallState};
use crate::tool_surface::EndpointScope;

/// The handles one job step needs on its own runtime. The runtime's single
/// `JobDriver` holds the one value; tests build it directly.
#[derive(Clone)]
pub struct JobRuntime {
    pub node: Arc<EmbeddedNode>,
    pub plugins: Arc<PluginExecutor>,
    /// The gents home plugins are installed under; empty when there is none.
    pub home: PathBuf,
    /// The operator tool root: step 0's working folder when the request names no workspace.
    pub tool_root: Option<PathBuf>,
}

/// How a job row ends: the guest's terminal output, a host refusal, or a control.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Settlement {
    Result(serde_json::Value),
    Fail(JobFailure),
    Suspend(JobSuspend),
    Refused(Refusal),
    Control(JobControlKind),
}

/// What a settlement writes: the row state, its canonical output, the
/// completion reason, and the job state a paused or awaiting row records.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Terminal {
    pub(super) state: ToolCallState,
    output: Value,
    reason: Option<String>,
    progress: Option<(JobState, Option<JobPauseReason>)>,
}

/// The settlement table. `None` for `Refused(Terminal)`: the row already ended.
fn terminal(settlement: Settlement, tool: &str, address: &str) -> Option<Terminal> {
    let ended = |state, output, reason| Terminal {
        state,
        output,
        reason,
        progress: None,
    };
    let paused = |reason: Option<JobPauseReason>, message: &str| Terminal {
        state: ToolCallState::Completed,
        output: json!({"status": "paused", "reason": reason, "message": message}),
        reason: None,
        progress: Some((JobState::Paused, reason)),
    };
    Some(match settlement {
        Settlement::Result(result) => ended(ToolCallState::Completed, result, None),
        Settlement::Fail(JobFailure { code, message }) => ended(
            ToolCallState::Failed,
            json!({"status": "failed", "code": code, "message": message}),
            Some(code),
        ),
        Settlement::Suspend(JobSuspend {
            kind: JobSuspendKind::Pause,
            reason,
            message,
            ..
        }) => paused(reason, &message),
        Settlement::Suspend(JobSuspend {
            kind: JobSuspendKind::AwaitUser,
            message,
            question,
            ..
        }) => Terminal {
            state: ToolCallState::Completed,
            output: json!({"status": "awaiting_user", "question": question, "message": message}),
            reason: None,
            progress: Some((JobState::AwaitingUser, None)),
        },
        Settlement::Refused(refusal) => {
            let code = refusal.code()?;
            let (state, status) = if refusal == Refusal::TimedOut {
                (ToolCallState::TimedOut, "timed_out")
            } else {
                (ToolCallState::Failed, "failed")
            };
            let (message, next) = refusal_text(refusal, tool, address);
            ended(
                state,
                json!({"status": status, "code": code, "message": message, "next": next}),
                Some(code.to_owned()),
            )
        }
        Settlement::Control(JobControlKind::Stop) => ended(
            ToolCallState::Cancelled,
            json!({"status": "cancelled", "message": "the job was stopped"}),
            Some("explicit_cancel".to_owned()),
        ),
        Settlement::Control(JobControlKind::Pause) => {
            paused(Some(JobPauseReason::User), "the job was paused")
        }
    })
}

/// What a host refusal means and the call that follows it.
fn refusal_text(refusal: Refusal, tool: &str, address: &str) -> (&'static str, String) {
    let inspect = format!("Next: `{tool} status {address}`, then `{tool} resume {address}`");
    match refusal {
        Refusal::Stalled => ("the job stopped making progress", inspect),
        Refusal::PolicyDenied => ("the job asked for something its policy refuses", inspect),
        Refusal::NotAvailable => (
            "the job's plugin or its job grant is no longer available",
            format!("Next: ask the operator to restore `{tool}` and its `job` block, then `{tool} resume {address}`"),
        ),
        Refusal::EngineChanged => (
            "the job's engine changed since the job was pinned",
            format!("Next: `{tool} resume {address} --engine current`"),
        ),
        Refusal::DepthExceeded => (
            "the job would run too deep in a chain of jobs",
            format!("Next: run `{tool}` from a session that no job started"),
        ),
        Refusal::TimedOut => ("the job ran past its max_wall_secs", inspect),
        Refusal::StepFailed => ("three consecutive job steps failed", inspect),
        Refusal::BackgroundBudgetExceeded => (
            "the parent request has reached its concurrent background capacity ceiling",
            "Next: wait for a background completion notification, then retry".to_owned(),
        ),
        Refusal::Terminal => ("the job already ended", inspect),
        Refusal::ResumeUnknown | Refusal::ResumeNotHead => {
            ("the job's resume named a row that is not the newest of a run", inspect)
        }
    }
}

/// The settlement's terminal and its canonical output text; `None` for
/// `Refused(Terminal)`.
pub(super) fn terminal_text(
    lifecycle: &ToolCallLifecycle,
    settlement: Settlement,
) -> Option<(Terminal, String)> {
    let address = format!("{}:{}", lifecycle.session_id(), lifecycle.tool_call_id());
    let terminal = terminal(settlement, lifecycle.tool_name(), &address)?;
    let output = match terminal.output.clone() {
        Value::String(text) => text,
        output => output.to_string(),
    };
    Some((terminal, output))
}

/// Ends the job row `lifecycle` with `settlement`, publishing the execution
/// receipt staged on `lifecycle` (the terminal step's evidence, or the last
/// step's for a fail-closed or killed row) through the terminal
/// compare-and-set, its only writer. `Refused(Terminal)` is a no-op.
pub async fn settle_plugin_job(
    lifecycle: &mut ToolCallLifecycle,
    settlement: Settlement,
) -> Result<()> {
    let Some((terminal, output)) = terminal_text(lifecycle, settlement) else {
        return Ok(());
    };
    lifecycle
        .settle_plugin_job_row(
            terminal.state,
            &output,
            terminal.reason.as_deref(),
            terminal.progress,
        )
        .await?;
    Ok(())
}

/// [`settle_plugin_job`] inside `txn`: the settlement table and the terminal
/// write, `None` for `Refused(Terminal)`. Apply the result with
/// [`ToolCallLifecycle::finish_plugin_job_row`] after `txn` commits.
pub(crate) async fn settle_plugin_job_in_txn(
    txn: &ConfigApplyTxn<'_>,
    lifecycle: &ToolCallLifecycle,
    settlement: Settlement,
    inline: bool,
) -> Result<Option<PluginJobTerminal>> {
    let Some((terminal, output)) = terminal_text(lifecycle, settlement) else {
        return Ok(None);
    };
    lifecycle
        .settle_plugin_job_row_in_txn(
            txn,
            terminal.state,
            &output,
            terminal.reason.as_deref(),
            terminal.progress,
            inline,
        )
        .await
        .map(Some)
}

/// What one `run_step` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepOutcome {
    /// The step committed and the row keeps running.
    Committed,
    /// The step committed a terminal output and the row settled.
    Settled,
    /// The input revision was stale; nothing committed and the row is re-stepped.
    Stale,
    /// Admission refused; the row failed closed through `control::kill`.
    Refused(Refusal),
}

/// What the step transaction decided.
enum Stepped {
    /// The row had already ended; the step did nothing.
    Ended,
    Stale,
    Refused {
        row: Box<StepRow>,
        refusal: Refusal,
    },
    Stopped(Box<StepRow>),
    Committed(Option<PluginJobTerminal>),
}

/// Runs one step of the running job row `row_doc_id`: builds its input, runs
/// the plugin on the shared executor without a tool dispatcher, and commits
/// the step in one revision-checked transaction.
pub async fn run_step(runtime: &JobRuntime, row_doc_id: &str) -> Result<StepOutcome> {
    let access = ConfigAccess::local(runtime.node.clone());
    let mut lifecycle = load_lifecycle(&access, row_doc_id).await?;
    let row_running = lifecycle.is_running();
    // The precheck avoids invoking a guest for an already-stopped session;
    // `step_transaction` repeats this exact-scope read at the write boundary.
    let session_stopped = row_running
        && crate::session::session_is_stopped(
            &runtime.node,
            lifecycle.node_did(),
            lifecycle.session_id(),
            lifecycle.requester_did(),
        )
        .await?;
    if !super::recovery::step_invocation_allowed(row_running, session_stopped) {
        if !row_running {
            return Ok(StepOutcome::Refused(Refusal::Terminal));
        }
        let row = read_step_row(&access, row_doc_id).await?;
        kill(&access, &mut lifecycle, &row, KillCause::Stop).await?;
        return Ok(StepOutcome::Settled);
    }
    let pin = read_pin(&access, row_doc_id)
        .await?
        .with_context(|| format!("job row {row_doc_id} has no job_pin"))?;
    let (namespace, name) = parse_coordinate(&pin.plugin_ref)?;
    let installed = match read_record(&runtime.home, namespace, name) {
        Ok(record) if record.digest == pin.engine_digest => record,
        Ok(_) => {
            return fail_closed(&access, &mut lifecycle, row_doc_id, Refusal::EngineChanged).await
        }
        Err(_) => {
            return fail_closed(&access, &mut lifecycle, row_doc_id, Refusal::NotAvailable).await
        }
    };
    let (input, revision) = build_step_input(&access, runtime, row_doc_id, StepKind::Later).await?;
    step_on(runtime, &mut lifecycle, &installed, input, revision).await
}

/// The step of `input`, built at progress `revision`, on `installed`.
async fn step_on(
    runtime: &JobRuntime,
    lifecycle: &mut ToolCallLifecycle,
    installed: &InstalledPlugin,
    input: JobStepInput,
    revision: u64,
) -> Result<StepOutcome> {
    let access = ConfigAccess::local(runtime.node.clone());
    let row_doc_id = input.job.row_doc_id.as_str();
    let pin = input
        .pin
        .clone()
        .with_context(|| format!("job row {row_doc_id} has no job_pin"))?;
    let caller = &input.caller;
    let jobs = JobCaller::new(
        runtime.node.clone(),
        ProcessControlScope {
            request_id: caller.request_id.clone(),
            session_id: caller.session_id.clone(),
            node_did: caller.node_did.clone(),
            requester_did: lifecycle.requester_did().map(str::to_owned),
        },
        StampContext::JobStep {
            node_did: caller.node_did.clone(),
            session_id: caller.session_id.clone(),
            request_doc_id: caller.request_doc_id.clone(),
            tool_call_doc_id: row_doc_id.to_owned(),
            job_row_doc_id: row_doc_id.to_owned(),
            chain_root_doc_id: pin.chain_root_doc_id.clone(),
        },
    )
    .with_children(input.job.chain.clone(), input.ceiling.result_bytes);
    let (call, receipt) = runtime
        .plugins
        .call_job_step_with_receipt(
            installed,
            serde_json::to_value(&input)?,
            Some(DataCaller::new(runtime.node.clone(), &caller.node_did)?),
            jobs,
        )
        .await;
    let call = match call {
        Ok(call) => call,
        Err(error) => {
            let trapped = error
                .chain()
                .any(|cause| cause.downcast_ref::<GuestTrap>().is_some());
            let error = if trapped {
                anyhow::Error::new(StepFailure::Trap(format!("{error:#}")))
            } else {
                error
            };
            return Err(error.context(format!("the step of job row {row_doc_id} did not run")));
        }
    };
    if let Some(failure) = step_failure(call.outcome.verdict, &call.outcome.diagnostics) {
        return Err(anyhow::Error::new(failure).context(format!(
            "the step of job row {row_doc_id} returned {:?}",
            call.outcome.verdict
        )));
    }
    let output: JobStepOutput = serde_json::from_value(call.outcome.output).map_err(|error| {
        anyhow::Error::new(StepFailure::MalformedOutput(error.to_string()))
            .context(format!("decoding the step output of job row {row_doc_id}"))
    })?;

    let scratch = scratch_of(&output);
    if !scratch.is_empty() {
        write_scratch(Path::new(&pin.cwd), &caller.tool_call_id, scratch)?;
    }
    if let JobStepOutput::Effects { effects } = &output {
        if !effects.interrupt.is_empty() {
            let row = read_step_row(&access, row_doc_id).await?;
            let slots = effects
                .interrupt
                .iter()
                .filter_map(|interrupt| {
                    let child = row
                        .children
                        .iter()
                        .filter(|child| child.slot == interrupt.slot)
                        .max_by_key(|child| child.attempt)?;
                    Some(SessionScope {
                        node_did: child.target.0.clone(),
                        session_id: job_session_id(&row.root, &child.slot, child.attempt),
                        requester_did: Some(caller.node_did.clone()),
                    })
                })
                .collect::<Vec<_>>();
            let spawner = SessionScope {
                node_did: caller.node_did.clone(),
                session_id: caller.session_id.clone(),
                requester_did: lifecycle.requester_did().map(str::to_owned),
            };
            interrupt_slots(&runtime.node, &spawner, &slots).await?;
        }
    }
    if settlement_of(&output).is_some() {
        lifecycle.stage_plugin_receipt(Some(receipt.clone()));
    }

    let cause = SessionMessageCause {
        caller_node_did: caller.node_did.clone(),
        caller_agent_id: caller.agent_id.clone(),
        caller_session_id: caller.session_id.clone(),
        caller_request_id: caller.request_id.clone(),
        caller_request_doc_id: caller.request_doc_id.clone(),
        caller_hop: caller.request_hop,
        tool_call_id: caller.tool_call_id.clone(),
        tool_call_doc_id: row_doc_id.to_owned(),
        correlation: caller.correlation.clone(),
    };
    let staged = Mutex::new(Vec::new());
    let stepped = {
        let (input, pin, output, receipt, cause, staged, lifecycle) = (
            &input,
            &pin,
            &output,
            &receipt,
            &cause,
            &staged,
            &*lifecycle,
        );
        ConfigAccess::transact_local_write(
            &runtime.node,
            Some(::identity::Did::new(caller.node_did.clone())?),
            "plugin_job.run_step",
            |txn| {
                Box::pin(async move {
                    staged.lock().expect("staged requests lock").clear();
                    step_transaction(
                        txn, runtime, lifecycle, input, pin, revision, output, receipt, cause,
                        staged,
                    )
                    .await
                })
            },
        )
        .await
    };
    let stepped = match stepped {
        Ok(stepped) => stepped,
        Err(error) => {
            if matches!(
                error.downcast_ref::<RecordError>(),
                Some(RecordError::StaleRevision { .. })
            ) {
                return Ok(StepOutcome::Stale);
            }
            let staged = std::mem::take(&mut *staged.lock().expect("staged requests lock"));
            recover_step_commit(
                &runtime.node,
                Some(::identity::Did::new(caller.node_did.clone())?),
                error,
                &staged,
            )
            .await?;
            Stepped::Committed(None)
        }
    };
    match stepped {
        Stepped::Ended => Ok(StepOutcome::Refused(Refusal::Terminal)),
        Stepped::Stale => Ok(StepOutcome::Stale),
        Stepped::Refused { row, refusal } => {
            lifecycle.stage_plugin_receipt(Some(receipt));
            kill(&access, lifecycle, &row, KillCause::FailClosed(refusal)).await?;
            Ok(StepOutcome::Refused(refusal))
        }
        Stepped::Stopped(row) => {
            lifecycle.stage_plugin_receipt(Some(receipt));
            kill(&access, lifecycle, &row, KillCause::Stop).await?;
            Ok(StepOutcome::Settled)
        }
        Stepped::Committed(Some(terminal)) => {
            lifecycle.finish_plugin_job_row(terminal).await?;
            Ok(StepOutcome::Settled)
        }
        Stepped::Committed(None) => Ok(StepOutcome::Committed),
    }
}

/// Row doc ids of every started step transaction run, so a test can see a
/// conflict re-run the whole transaction.
#[cfg(test)]
static STEP_TRANSACTION_RUNS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// The step transaction: it re-reads every fact it admits against, so a
/// conflict re-runs it whole and writes nothing in memory.
#[allow(clippy::too_many_arguments)]
async fn step_transaction(
    txn: &ConfigApplyTxn<'_>,
    runtime: &JobRuntime,
    lifecycle: &ToolCallLifecycle,
    input: &JobStepInput,
    pin: &JobPin,
    revision: u64,
    output: &JobStepOutput,
    receipt: &PluginExecutionReceipt,
    cause: &SessionMessageCause,
    staged: &Mutex<Vec<gents_protocol::request_admission::AgentRequestCreate>>,
) -> Result<Stepped> {
    let row_doc_id = input.job.row_doc_id.as_str();
    #[cfg(test)]
    STEP_TRANSACTION_RUNS
        .lock()
        .expect("step run log lock")
        .push(row_doc_id.to_owned());
    // (a) the row is running and its pin is unchanged.
    if read_lifecycle_state(txn, row_doc_id).await? != ToolCallState::Running.as_str() {
        return Ok(Stepped::Ended);
    }
    anyhow::ensure!(
        read_pin(txn, row_doc_id).await?.as_ref() == Some(pin),
        "the job_pin of {row_doc_id} changed during the step"
    );
    // (b) the input is still current; (e)'s compare-and-set is the authority.
    let stored = read_progress(txn, row_doc_id).await?;
    if stored.as_ref().map_or(0, |progress| progress.revision) != revision {
        return Ok(Stepped::Stale);
    }
    // (c)
    let row = read_step_row_in_txn(txn, row_doc_id).await?;
    if crate::session::session_stopped_in_txn(
        txn,
        lifecycle.node_did(),
        lifecycle.session_id(),
        lifecycle.requester_did(),
    )
    .await?
    {
        return Ok(Stepped::Stopped(Box::new(row)));
    }
    let (ctx, installed) = step_context(txn, runtime, input, pin, output, lifecycle).await?;
    let admitted = match admit_step(&ctx, &row, output) {
        Ok(admitted) => admitted,
        Err(refusal) => {
            return Ok(Stepped::Refused {
                row: Box::new(row),
                refusal: refusal.refusal,
            })
        }
    };
    if let JobStepOutput::Effects { effects } = output {
        let fresh = fresh_spawns(&ctx, &row, output);
        let job = JobContext {
            ctx: &ctx,
            row: &row,
            cause,
        };
        let created = stage_recorded_effects_in_txn(txn, &job, &fresh, effects).await?;
        *staged.lock().expect("staged requests lock") = created;
    }
    // (d)
    let writes = writes_of(output);
    if !writes.is_empty() {
        let grant = installed
            .as_ref()
            .and_then(InstalledPlugin::data_grant)
            .with_context(|| {
                format!(
                    "plugin {} declares no data, so its step cannot write",
                    pin.plugin_ref
                )
            })?;
        let stamps = StampContext::JobStep {
            node_did: input.caller.node_did.clone(),
            session_id: input.caller.session_id.clone(),
            request_doc_id: input.caller.request_doc_id.clone(),
            tool_call_doc_id: row_doc_id.to_owned(),
            job_row_doc_id: row_doc_id.to_owned(),
            chain_root_doc_id: pin.chain_root_doc_id.clone(),
        };
        crate::application_write::apply_plugin_writes_in_txn(txn, &grant, &stamps, writes.to_vec())
            .await?;
    }
    // (e)
    let guest = progress_of(output)
        .cloned()
        .or_else(|| stored.clone())
        .with_context(|| format!("job row {row_doc_id} has no progress to carry"))?;
    let budget = stored.map_or_else(|| guest.budget.clone(), |progress| progress.budget);
    write_progress_in_txn(
        txn,
        row_doc_id,
        revision,
        guest,
        HostProgressFacts {
            spawned: admitted.spent,
            budget,
            observed_terminal_children: admitted.observed_terminal,
            state: JobState::Running,
            pause_reason: None,
        },
    )
    .await?;
    write_step_state_in_txn(txn, row_doc_id, receipt, None).await?;
    // (f)
    let Some(settlement) = settlement_of(output) else {
        return Ok(Stepped::Committed(None));
    };
    Ok(Stepped::Committed(
        settle_plugin_job_in_txn(txn, lifecycle, settlement, false).await?,
    ))
}

/// Every fact admission reads about the caller, the plugin and the host,
/// read through `txn`. The step-0 facts are inert: a pinned row never
/// consults them.
async fn step_context(
    txn: &ConfigApplyTxn<'_>,
    runtime: &JobRuntime,
    input: &JobStepInput,
    pin: &JobPin,
    output: &JobStepOutput,
    lifecycle: &ToolCallLifecycle,
) -> Result<(StepContext, Option<InstalledPlugin>)> {
    let facts = HostFacts {
        engine_digest: pin.engine_digest.clone(),
        plugin_ref: pin.plugin_ref.clone(),
        cwd: pin.cwd.clone(),
        args_digest: pin.args_digest.clone(),
        chain_root: pin.chain_root_doc_id.clone(),
        origin: pin.origin,
        adopted_at: pin.adopted_at,
        resume_of: pin.resume_of.clone(),
        replayed_from: pin.replayed_from.clone(),
        ceiling_at_pin: pin.ceiling_at_pin.clone(),
    };
    step_context_for(txn, runtime, input, facts, output, lifecycle).await
}

/// [`step_context`] over the host `facts` a step is admitted with; they name
/// the plugin and the working folder the live settings and mounts are read for.
pub(super) async fn step_context_for(
    txn: &ConfigApplyTxn<'_>,
    runtime: &JobRuntime,
    input: &JobStepInput,
    facts: HostFacts,
    output: &JobStepOutput,
    lifecycle: &ToolCallLifecycle,
) -> Result<(StepContext, Option<InstalledPlugin>)> {
    let caller = &input.caller;
    let (_, tools, targets) = crate::request_admission::load_request_context_in_txn(
        txn,
        &caller.node_did,
        &caller.agent_id,
    )
    .await?;
    let settings = tools
        .and_then(|tools| tools.integrations)
        .and_then(|integrations| integrations.plugins)
        .into_iter()
        .flatten()
        .find(|plugin| plugin.plugin == facts.plugin_ref)
        .and_then(|plugin| plugin.job);
    let installed = parse_coordinate(&facts.plugin_ref)
        .ok()
        .and_then(|(namespace, name)| read_record(&runtime.home, namespace, name).ok());
    let policy = settings.as_ref().map(|settings| &settings.policy);
    let cwd = Path::new(&facts.cwd);
    let installed_trusted_sources = match &installed {
        Some(plugin) => {
            let index = script_index(&runtime.home, plugin, policy, Some(cwd))?;
            let via: BTreeMap<&str, InstalledVia> = index
                .installed_via
                .iter()
                .map(|(pack, via)| (pack.as_str(), *via))
                .collect();
            index
                .files
                .iter()
                .filter(|(_, pack, _, class)| trusted_hint(pack, class, &via, &index.trusted_packs))
                .map(|(sha, ..)| sha.clone())
                .collect()
        }
        None => Vec::new(),
    };
    let key = |target: &crate::document_config::AgentTargetDocument| {
        (target.target_node_did.clone(), target.agent_id.clone())
    };
    let job_ids = settings
        .as_ref()
        .map(|settings| settings.agent_targets.as_slice());
    let mut target_hops = Vec::new();
    let mut within = Vec::new();
    let mut cwd_allowed = Vec::new();
    if let JobStepOutput::Effects { effects } = output {
        for spawn in &effects.spawn {
            if let Some(target) = targets
                .iter()
                .find(|target| target.target_id == spawn.target)
            {
                if !target_hops.iter().any(|(pair, _)| pair == &key(target)) {
                    target_hops.push((key(target), 0));
                }
            }
            if let Some(dir) = &spawn.cwd {
                let resolved = cwd.join(dir).canonicalize().ok();
                if resolved
                    .as_deref()
                    .zip(cwd.canonicalize().ok())
                    .is_some_and(|(dir, base)| dir.starts_with(base))
                {
                    within.push((facts.cwd.clone(), dir.clone()));
                }
                if resolved.as_deref().is_some_and(|dir| {
                    crate::workspace::require_under_ceiling(dir, runtime.tool_root.as_deref())
                        .is_ok()
                }) {
                    cwd_allowed.push(dir.clone());
                }
            }
        }
    }
    let ctx = StepContext {
        live: effective_ceiling(settings.as_ref()),
        available: settings.is_some() && installed.is_some(),
        caller_targets: EndpointScope::<(String, String), ()>::only_units(targets.iter().map(key)),
        job_targets: match job_ids {
            Some(ids) => EndpointScope::<(String, String), ()>::only_units(
                targets
                    .iter()
                    .filter(|target| ids.contains(&target.target_id))
                    .map(key),
            ),
            None => EndpointScope::none(),
        },
        policy: policy.cloned().unwrap_or_default(),
        engine_digest: installed
            .as_ref()
            .map(|plugin| plugin.digest.clone())
            .unwrap_or_default(),
        engine_provenance_operator: installed
            .as_ref()
            .is_some_and(|plugin| engine_guard(&runtime.home, plugin).is_ok()),
        installed_trusted_sources,
        max_hop: max_request_hop_in_txn(txn, &caller.node_did).await?,
        target_hops,
        target_ids: targets
            .iter()
            .map(|target| (target.target_id.clone(), key(target)))
            .collect(),
        live_background: 0,
        started_by_job: false,
        cwd_allowed,
        within,
        named_origin: None,
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
    };
    Ok((ctx, installed))
}

/// The target node's optional `max_request_hop`, read through `txn`.
async fn max_request_hop_in_txn(txn: &ConfigApplyTxn<'_>, node_did: &str) -> Result<Option<u32>> {
    let response = txn
        .execute(&format!(
            r#"{{ Node(filter: {{ node_did: {{ _eq: "{}" }} }}, limit: 2) {{ max_request_hop }} }}"#,
            escape_graphql_string(node_did)
        ))
        .await?;
    let rows = response
        .pointer("/data/Node")
        .and_then(Value::as_array)
        .context("Node hop-cap query returned no row array")?;
    anyhow::ensure!(rows.len() <= 1, "multiple Node rows matched a DID");
    match rows.first().map(|row| &row["max_request_hop"]) {
        None | Some(Value::Null) => Ok(None),
        Some(hop) => hop
            .as_u64()
            .and_then(|hop| u32::try_from(hop).ok())
            .map(Some)
            .context("Node.max_request_hop must be a nonnegative u32"),
    }
}

async fn read_lifecycle_state(txn: &ConfigApplyTxn<'_>, row_doc_id: &str) -> Result<String> {
    let response = txn
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ lifecycle_state }} }}"#,
            escape_graphql_string(row_doc_id)
        ))
        .await?;
    response
        .pointer("/data/AgentToolCall/0/lifecycle_state")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .with_context(|| format!("job row {row_doc_id} does not exist"))
}

/// The job row's lifecycle, loaded by its physical identity.
pub(super) async fn load_lifecycle(
    access: &ConfigAccess,
    row_doc_id: &str,
) -> Result<ToolCallLifecycle> {
    let response = access
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ session_id node_did requester_did request_doc_id }} }}"#,
            escape_graphql_string(row_doc_id)
        ))
        .await?;
    let stored: StoredRow = serde_json::from_value(
        response
            .pointer("/data/AgentToolCall/0")
            .cloned()
            .with_context(|| format!("job row {row_doc_id} does not exist"))?,
    )
    .with_context(|| format!("decoding job row {row_doc_id}"))?;
    let ConfigAccess::Local(node) = access else {
        anyhow::bail!("a job step needs a local node");
    };
    ToolCallLifecycle::load_by_doc_id(
        node.node().clone(),
        row_doc_id,
        &stored.node_did,
        &stored.session_id,
        stored.requester_did.as_deref(),
    )
    .await?
    .with_context(|| format!("job row {row_doc_id} does not exist"))
}

/// Fails the job closed with `refusal`, the step having committed nothing.
async fn fail_closed(
    access: &ConfigAccess,
    lifecycle: &mut ToolCallLifecycle,
    row_doc_id: &str,
    refusal: Refusal,
) -> Result<StepOutcome> {
    let row = read_step_row(access, row_doc_id).await?;
    kill(access, lifecycle, &row, KillCause::FailClosed(refusal)).await?;
    Ok(StepOutcome::Refused(refusal))
}

/// A step invocation that failed: the three failures that count towards
/// `plugin_job_step_failed`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StepFailure {
    #[error("the guest trapped: {0}")]
    Trap(String),
    #[error("the guest ran out of its budget")]
    BudgetExhausted,
    #[error("the guest returned a malformed step output: {0}")]
    MalformedOutput(String),
}

/// What a step failure means for the row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureVerdict {
    /// The row stays running and its step runs again.
    Retry,
    /// The third consecutive failure: the row fails `plugin_job_step_failed`.
    Settle,
}

/// Lean `maxConsecutiveStepFailures`.
const MAX_CONSECUTIVE_STEP_FAILURES: u32 = 3;

/// The failure `verdict` of a plugin call is, if it is one.
fn step_failure(verdict: PluginVerdict, diagnostics: &str) -> Option<StepFailure> {
    match verdict {
        PluginVerdict::Success => None,
        PluginVerdict::OutOfFuel | PluginVerdict::OutOfMemory | PluginVerdict::Timeout => {
            Some(StepFailure::BudgetExhausted)
        }
        PluginVerdict::BadOutput => Some(StepFailure::MalformedOutput(diagnostics.to_owned())),
        PluginVerdict::Refused | PluginVerdict::Failed => {
            Some(StepFailure::Trap(diagnostics.to_owned()))
        }
    }
}

/// Lean `onInvocationFailure`: one more failed invocation of `row`. The
/// third in a row ends a running row `failed`, kills its live own children
/// as a stop does, and refuses `StepFailed`; before that nothing is killed.
pub fn step_failure_decision(
    row: &StepRow,
    observe: &dyn Fn(&str) -> KillObservation,
) -> (KillOutcome, Option<Refusal>) {
    let counted = StepRow {
        consecutive_failures: row.consecutive_failures + 1,
        ..row.clone()
    };
    if counted.consecutive_failures >= MAX_CONSECUTIVE_STEP_FAILURES {
        let outcome = kill_decision(
            &counted,
            KillCause::FailClosed(Refusal::StepFailed),
            observe,
        );
        (outcome, Some(Refusal::StepFailed))
    } else {
        (
            KillOutcome {
                row: counted,
                won: false,
                killed: Vec::new(),
            },
            None,
        )
    }
}

/// Whether a wake came while the row's step was in flight.
#[derive(Debug, Default)]
struct Dirty(bool);

/// A row's consecutive step failures, held in memory (one active runtime per
/// node: a restart resets them), and the row and children's observations
/// the last failure read.
#[derive(Default)]
struct RowFailures {
    count: u32,
    read: Option<(StepRow, RapidHashMap<String, KillObservation>)>,
}

/// Runs job steps: one in flight per row, wakes during a step coalesced into
/// one re-run, and at most four steps at once.
pub struct JobDriver {
    runtime: JobRuntime,
    this: Weak<JobDriver>,
    in_flight: Mutex<RapidHashMap<String, Dirty>>,
    permits: Semaphore,
    failures: Mutex<RapidHashMap<String, RowFailures>>,
    /// Every row ever kicked, for tests that assert a wake reached no row.
    #[cfg(test)]
    pub(crate) kicked: Mutex<Vec<String>>,
}

impl JobDriver {
    pub fn new(runtime: JobRuntime) -> Arc<Self> {
        Arc::new_cyclic(|this| Self {
            runtime,
            this: this.clone(),
            in_flight: Mutex::new(RapidHashMap::new()),
            permits: Semaphore::new(4),
            failures: Mutex::new(RapidHashMap::new()),
            #[cfg(test)]
            kicked: Mutex::new(Vec::new()),
        })
    }

    /// The handles every step of this runtime runs on.
    pub fn runtime(&self) -> &JobRuntime {
        &self.runtime
    }

    pub(crate) fn in_flight_row_ids(&self) -> Vec<String> {
        self.in_flight
            .lock()
            .expect("in-flight rows lock")
            .keys()
            .cloned()
            .collect()
    }

    /// Wakes the row: starts its step, or marks it dirty when one is in flight.
    pub fn kick(&self, row_doc_id: &str) {
        #[cfg(test)]
        self.kicked.lock().unwrap().push(row_doc_id.to_owned());
        let mut in_flight = self.in_flight.lock().expect("in-flight rows lock");
        if let Some(dirty) = in_flight.get_mut(row_doc_id) {
            dirty.0 = true;
            return;
        }
        in_flight.insert(row_doc_id.to_owned(), Dirty::default());
        let driver = self.this.upgrade().expect("a driver is held by its Arc");
        let row_doc_id = row_doc_id.to_owned();
        tokio::spawn(async move { driver.drive(&row_doc_id).await });
    }

    /// Runs one step of the row now and returns what it did. `None` when a
    /// step of the row is already in flight: that step re-runs once for this
    /// call, as for a wake.
    pub async fn step_now(&self, row_doc_id: &str) -> Option<Result<StepOutcome>> {
        {
            let mut in_flight = self.in_flight.lock().expect("in-flight rows lock");
            if let Some(dirty) = in_flight.get_mut(row_doc_id) {
                dirty.0 = true;
                return None;
            }
            in_flight.insert(row_doc_id.to_owned(), Dirty::default());
        }
        let (again, outcome) = self.step_once(row_doc_id).await;
        let mut in_flight = self.in_flight.lock().expect("in-flight rows lock");
        let dirty = std::mem::take(
            &mut in_flight
                .get_mut(row_doc_id)
                .expect("the row is in flight")
                .0,
        );
        if again || dirty {
            let driver = self.this.upgrade().expect("a driver is held by its Arc");
            let row_doc_id = row_doc_id.to_owned();
            tokio::spawn(async move { driver.drive(&row_doc_id).await });
        } else {
            in_flight.remove(row_doc_id);
        }
        Some(outcome)
    }

    /// Steps the row until no wake or retry is left.
    async fn drive(&self, row_doc_id: &str) {
        loop {
            let (again, _) = self.step_once(row_doc_id).await;
            let mut in_flight = self.in_flight.lock().expect("in-flight rows lock");
            let dirty = std::mem::take(
                &mut in_flight
                    .get_mut(row_doc_id)
                    .expect("the row is in flight")
                    .0,
            );
            if !(again || dirty) {
                in_flight.remove(row_doc_id);
                return;
            }
        }
    }

    /// One step of the row, and whether it must run again.
    async fn step_once(&self, row_doc_id: &str) -> (bool, Result<StepOutcome>) {
        let _permit = self
            .permits
            .acquire()
            .await
            .expect("the step semaphore is never closed");
        let outcome = run_step(&self.runtime, row_doc_id).await;
        let again = match &outcome {
            Ok(StepOutcome::Stale) => true,
            Ok(_) => {
                self.failures
                    .lock()
                    .expect("step failures lock")
                    .remove(row_doc_id);
                false
            }
            Err(error) => match error.downcast_ref::<StepFailure>() {
                None => {
                    tracing::warn!(row_doc_id, error = %format!("{error:#}"), "job step did not run");
                    false
                }
                Some(failure) => match self.step_failed(row_doc_id, failure).await {
                    Ok(verdict) => verdict == FailureVerdict::Retry,
                    Err(error) => {
                        tracing::warn!(row_doc_id, error = %format!("{error:#}"), "job step failure was not settled");
                        false
                    }
                },
            },
        };
        (again, outcome)
    }

    /// Reads the row and its children's observations, counts the failure and,
    /// on the third, fails the row closed with `plugin_job_step_failed`.
    async fn step_failed(&self, row_doc_id: &str, error: &StepFailure) -> Result<FailureVerdict> {
        let access = ConfigAccess::local(self.runtime.node.clone());
        let mut lifecycle = load_lifecycle(&access, row_doc_id).await?;
        let row = read_step_row(&access, row_doc_id).await?;
        let observed = observe_live_children(&access, &row, lifecycle.node_did()).await?;
        self.failures
            .lock()
            .expect("step failures lock")
            .entry(row_doc_id.to_owned())
            .or_default()
            .read = Some((row.clone(), observed));
        let verdict = self.on_step_failure(row_doc_id, error);
        if verdict == FailureVerdict::Settle {
            kill(
                &access,
                &mut lifecycle,
                &row,
                KillCause::FailClosed(Refusal::StepFailed),
            )
            .await?;
            self.failures
                .lock()
                .expect("step failures lock")
                .remove(row_doc_id);
        }
        Ok(verdict)
    }

    /// Counts one step invocation failure of the row, whose row and children's
    /// observations `step_failed` has just read.
    pub fn on_step_failure(&self, row_doc_id: &str, error: &StepFailure) -> FailureVerdict {
        tracing::warn!(row_doc_id, %error, "job step failed");
        let mut failures = self.failures.lock().expect("step failures lock");
        let state = failures
            .get_mut(row_doc_id)
            .expect("the failing row's state is read");
        let (row, observed) = state.read.as_ref().expect("the failing row is read");
        let observe = |key: &str| {
            observed
                .get(key)
                .copied()
                .unwrap_or(KillObservation::Unresolved)
        };
        let counted = StepRow {
            consecutive_failures: state.count,
            ..row.clone()
        };
        let (outcome, refusal) = step_failure_decision(&counted, &observe);
        state.count = outcome.row.consecutive_failures;
        match refusal {
            Some(Refusal::StepFailed) => FailureVerdict::Settle,
            _ => FailureVerdict::Retry,
        }
    }
}

pub(super) fn settlement_of(output: &JobStepOutput) -> Option<Settlement> {
    match output {
        JobStepOutput::Result { result, .. } => Some(Settlement::Result(result.clone())),
        JobStepOutput::Fail { fail, .. } => Some(Settlement::Fail(fail.clone())),
        JobStepOutput::Suspend { suspend, .. } => Some(Settlement::Suspend(suspend.clone())),
        JobStepOutput::Effects { .. } => None,
    }
}

pub(super) fn writes_of(output: &JobStepOutput) -> &[DataWrite] {
    match output {
        JobStepOutput::Result { writes, .. }
        | JobStepOutput::Fail { writes, .. }
        | JobStepOutput::Suspend { writes, .. } => writes,
        JobStepOutput::Effects { effects } => &effects.writes,
    }
}

pub(super) fn scratch_of(output: &JobStepOutput) -> &[JobScratch] {
    match output {
        JobStepOutput::Result { scratch, .. } | JobStepOutput::Suspend { scratch, .. } => scratch,
        JobStepOutput::Effects { effects } => &effects.scratch,
        JobStepOutput::Fail { .. } => &[],
    }
}

pub(super) fn progress_of(output: &JobStepOutput) -> Option<&JobProgress> {
    match output {
        JobStepOutput::Result { progress, .. }
        | JobStepOutput::Fail { progress, .. }
        | JobStepOutput::Suspend { progress, .. } => progress.as_ref(),
        JobStepOutput::Effects { effects } => effects.progress.as_ref(),
    }
}

#[derive(Deserialize)]
struct StoredRow {
    session_id: String,
    node_did: String,
    #[serde(default)]
    requester_did: Option<String>,
    request_doc_id: String,
    #[serde(default)]
    lifecycle_state: Option<String>,
}

/// The admission row (`admit::StepRow`) of a stored job row, read through
/// `txn`. The one place a job row becomes a `StepRow`.
pub(crate) async fn read_step_row_in_txn(
    txn: &ConfigApplyTxn<'_>,
    row_doc_id: &str,
) -> Result<StepRow> {
    let mut row = read_row(txn, row_doc_id).await?;
    if row.root != row_doc_id {
        let root = read_row(txn, &row.root).await?;
        row.root_call = Some(root_call_of(&root));
    }
    Ok(row)
}

/// [`read_step_row_in_txn`] in a read-only transaction of `access`.
pub(crate) async fn read_step_row(access: &ConfigAccess, row_doc_id: &str) -> Result<StepRow> {
    access
        .transact_readonly("plugin_job.read_step_row", |txn| {
            Box::pin(read_step_row_in_txn(txn, row_doc_id))
        })
        .await
}

/// One row's own facts; `root_call` is the caller's to fill from the chain root.
async fn read_row(txn: &ConfigApplyTxn<'_>, row_doc_id: &str) -> Result<StepRow> {
    let stored = txn
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ session_id node_did requester_did request_doc_id lifecycle_state }} }}"#,
            escape_graphql_string(row_doc_id)
        ))
        .await?;
    let stored: StoredRow = serde_json::from_value(
        stored
            .pointer("/data/AgentToolCall/0")
            .cloned()
            .with_context(|| format!("job row {row_doc_id} does not exist"))?,
    )
    .with_context(|| format!("decoding job row {row_doc_id}"))?;
    let hop = txn
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ request_hop }} }}"#,
            escape_graphql_string(&stored.request_doc_id)
        ))
        .await?;
    let caller_hop = hop
        .pointer("/data/AgentRequest/0")
        .with_context(|| format!("the caller request of job row {row_doc_id} does not exist"))?
        .get("request_hop")
        .and_then(Value::as_u64)
        .map_or(0, |hop| u32::try_from(hop).unwrap_or(u32::MAX));
    let text = load_tool_call_arguments_in_txn(
        txn,
        row_doc_id,
        &stored.node_did,
        &stored.session_id,
        stored.requester_did.as_deref(),
    )
    .await?;
    let call: CallArgs = serde_json::from_str(&text)
        .with_context(|| format!("decoding the accepted arguments of job row {row_doc_id}"))?;
    let pin = read_pin(txn, row_doc_id).await?;
    let progress = read_progress(txn, row_doc_id).await?;
    let lifecycle = stored
        .lifecycle_state
        .as_deref()
        .and_then(ToolCallState::from_persisted)
        .unwrap_or(ToolCallState::Running);
    let state = job_state(lifecycle, progress.as_ref());
    let chain = chain_of(txn, row_doc_id, pin.as_ref()).await?;
    let result_bytes = pin
        .as_ref()
        .map_or(0, |pin| pin.ceiling_at_pin.result_bytes);
    let children = derive_children_in_txn(txn, &chain, result_bytes)
        .await?
        .into_iter()
        .map(|(child, _)| child)
        .collect();
    Ok(StepRow {
        root: pin.as_ref().map_or_else(
            || row_doc_id.to_owned(),
            |pin| pin.chain_root_doc_id.clone(),
        ),
        row_doc_id: row_doc_id.to_owned(),
        caller_session: stored.session_id,
        caller_cwd: pin.as_ref().map(|pin| pin.cwd.clone()).unwrap_or_default(),
        handle: pin
            .as_ref()
            .map(|pin| pin.handle.clone())
            .unwrap_or_default(),
        caller_hop,
        call: StepCall {
            argv: call.argv,
            args: call
                .args
                .map(|args| serde_json::to_string(&args))
                .transpose()?,
            path: call.path,
            script: call.script,
            agent_budget: call.agent_budget,
        },
        root_call: None,
        resume: read_step_state(txn, row_doc_id)
            .await?
            .and_then(|state| state.resume),
        pin,
        children,
        spent: progress.as_ref().map_or(0, |progress| progress.spawned),
        observed_terminal: progress
            .as_ref()
            .map_or(0, |progress| progress.observed_terminal_children),
        consecutive_failures: 0,
        state,
    })
}

/// Stores a running job row's progress at revision 1 (spawned 2, one child
/// observed terminal), as adoption and the first step leave it.
#[cfg(test)]
pub(super) async fn store_running_progress(
    admission: &crate::tool_call_lifecycle::admission_fixture::PublishedAdmission,
) {
    use super::records::{write_progress_in_txn, HostProgressFacts};
    use crate::config_client::ConfigAccess;
    use gents_protocol::job::{JobBudget, JobProgress};
    let doc_id = admission.tool.doc_id().unwrap().to_owned();
    ConfigAccess::transact_local_write(&admission.node, None, "test.driver.progress", |txn| {
        let doc_id = doc_id.clone();
        Box::pin(async move {
            let guest = JobProgress {
                title: "audit".to_owned(),
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
                last_event: "scanning".to_owned(),
                last_event_at: None,
                question: None,
                observed_terminal_children: 0,
                revision: 0,
            };
            let host = HostProgressFacts {
                spawned: 2,
                budget: JobBudget {
                    total: 4,
                    spent: 2,
                    remaining: 2,
                },
                observed_terminal_children: 1,
                state: JobState::Running,
                pause_reason: None,
            };
            write_progress_in_txn(txn, &doc_id, 0, guest, host).await
        })
    })
    .await
    .unwrap();
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::config_client::ConfigAccess;
    use crate::graphql::escape_graphql_string;
    use crate::plugin_job::records::read_progress;
    use crate::tool_call_lifecycle::admission_fixture::{
        published_admission, PublishedAdmission, PublishedAdmissionOptions,
    };
    use crate::tool_call_lifecycle::AwaitMode;
    use gents_protocol::job::{
        JobControlKind, JobControlOutcome, JobFailure, JobPauseReason, JobProgress, JobState,
        JobSuspend, JobSuspendKind,
    };
    use gents_protocol::plugin::{PluginExecutionReceipt, PluginExecutionVerdict};
    use serde_json::{json, Value};

    use super::super::admit::Refusal;
    use super::super::control::control_outcome;

    const HOST_REFUSALS: [Refusal; 6] = [
        Refusal::Stalled,
        Refusal::PolicyDenied,
        Refusal::NotAvailable,
        Refusal::EngineChanged,
        Refusal::DepthExceeded,
        Refusal::StepFailed,
    ];

    fn evidence(input: &str) -> PluginExecutionReceipt {
        PluginExecutionReceipt {
            coordinate: "fixture/engine".into(),
            artifact_digest: "sha256:engine".into(),
            input_digest: input.into(),
            output_digest: Some("sha256:output".into()),
            authority: None,
            limits: None,
            verdict: PluginExecutionVerdict::Success,
        }
    }

    /// A running background job row with its invocation receipt published and
    /// no stored progress.
    async fn bare_job_row(name: &str) -> PublishedAdmission {
        let mut admission = published_admission(PublishedAdmissionOptions {
            name: name.to_owned(),
            real_identity: true,
            await_mode: AwaitMode::Background,
            job_pin: true,
            ..Default::default()
        })
        .await
        .unwrap();
        assert!(admission
            .tool
            .publish_background_receipt("started")
            .await
            .unwrap());
        admission
    }

    /// A running job row with its stored progress at revision 1 and the last
    /// step's evidence staged.
    async fn job_row(name: &str) -> PublishedAdmission {
        let mut admission = bare_job_row(name).await;
        store_running_progress(&admission).await;
        admission.tool.stage_plugin_receipt(Some(evidence("step")));
        admission
    }

    async fn finish(admission: PublishedAdmission) {
        admission.node.shutdown().await;
        let _ = std::fs::remove_dir_all(admission.path);
    }

    async fn stored_row(admission: &PublishedAdmission) -> Value {
        let response = ConfigAccess::local(admission.node.clone())
            .execute(&format!(
                r#"{{ AgentToolCall(docID: "{}") {{ lifecycle_state plugin_execution_receipt }} }}"#,
                escape_graphql_string(admission.tool.doc_id().unwrap()),
            ))
            .await
            .unwrap();
        response["data"]["AgentToolCall"][0].clone()
    }

    /// The row's closed canonical tool output.
    async fn stored_output(admission: &PublishedAdmission) -> String {
        let tool = &admission.tool;
        match crate::background_tools::observe_canonical_tool_output_with_access(
            &ConfigAccess::local(admission.node.clone()),
            tool.doc_id().unwrap(),
            tool.request_doc_id().unwrap(),
            tool.session_id(),
            tool.node_did(),
            tool.requester_did(),
        )
        .await
        .unwrap()
        {
            crate::background_tools::CanonicalToolOutputObservation::Closed(output) => output,
            crate::background_tools::CanonicalToolOutputObservation::Open(output) => {
                panic!("job row output is still open: {output}")
            }
        }
    }

    async fn stored_progress(admission: &PublishedAdmission) -> JobProgress {
        read_progress(
            &ConfigAccess::local(admission.node.clone()),
            admission.tool.doc_id().unwrap(),
        )
        .await
        .unwrap()
        .unwrap()
    }

    async fn settled(name: &str, settlement: Settlement) -> (PublishedAdmission, Value) {
        let mut admission = job_row(name).await;
        settle_plugin_job(&mut admission.tool, settlement)
            .await
            .unwrap();
        let row = stored_row(&admission).await;
        (admission, row)
    }

    fn payload(output: &str) -> Value {
        serde_json::from_str(output).unwrap()
    }

    #[tokio::test]
    async fn result_settles_completed() {
        let (admission, row) =
            settled("driver-result", Settlement::Result(json!({"findings": 3}))).await;
        assert_eq!(row["lifecycle_state"], "completed");
        assert_eq!(
            payload(&stored_output(&admission).await),
            json!({"findings": 3})
        );
        finish(admission).await;
    }

    #[tokio::test]
    async fn fail_settles_failed_with_the_guest_code() {
        let (admission, row) = settled(
            "driver-fail",
            Settlement::Fail(JobFailure {
                code: "audit_rejected".to_owned(),
                message: "the audit found a blocker".to_owned(),
            }),
        )
        .await;
        assert_eq!(row["lifecycle_state"], "failed");
        let output = payload(&stored_output(&admission).await);
        assert_eq!(output["status"], "failed");
        assert_eq!(output["code"], "audit_rejected");
        finish(admission).await;
    }

    #[tokio::test]
    async fn suspend_pause_settles_completed_paused_with_pause_reason() {
        let (admission, row) = settled(
            "driver-pause",
            Settlement::Suspend(JobSuspend {
                kind: JobSuspendKind::Pause,
                reason: Some(JobPauseReason::BackOff),
                message: "waiting for the rate limit".to_owned(),
                question: None,
            }),
        )
        .await;
        assert_eq!(row["lifecycle_state"], "completed");
        assert_eq!(
            payload(&stored_output(&admission).await)["status"],
            "paused"
        );
        let progress = stored_progress(&admission).await;
        assert_eq!(progress.state, JobState::Paused);
        assert_eq!(progress.pause_reason, Some(JobPauseReason::BackOff));
        assert_eq!(progress.revision, 2);
        assert_eq!(progress.spawned, 2);
        assert_eq!(progress.observed_terminal_children, 1);
        assert_eq!(progress.last_event, "scanning");
        finish(admission).await;
    }

    #[tokio::test]
    async fn pause_or_await_without_stored_progress_fails_and_leaves_the_row_running() {
        for settlement in [
            Settlement::Control(JobControlKind::Pause),
            Settlement::Suspend(JobSuspend {
                kind: JobSuspendKind::AwaitUser,
                reason: None,
                message: "needs a decision".to_owned(),
                question: Some("ship it?".to_owned()),
            }),
        ] {
            let mut admission = bare_job_row("driver-no-progress").await;
            let error = settle_plugin_job(&mut admission.tool, settlement.clone())
                .await
                .unwrap_err();
            assert!(
                format!("{error:#}").contains("no stored progress"),
                "{settlement:?}: {error:#}"
            );
            assert_eq!(
                stored_row(&admission).await["lifecycle_state"],
                "running",
                "{settlement:?}"
            );
            finish(admission).await;
        }
    }

    #[tokio::test]
    async fn await_user_settles_completed_awaiting_user_with_the_question() {
        let (admission, row) = settled(
            "driver-await",
            Settlement::Suspend(JobSuspend {
                kind: JobSuspendKind::AwaitUser,
                reason: None,
                message: "needs a decision".to_owned(),
                question: Some("ship it?".to_owned()),
            }),
        )
        .await;
        assert_eq!(row["lifecycle_state"], "completed");
        let output = payload(&stored_output(&admission).await);
        assert_eq!(output["status"], "awaiting_user");
        assert_eq!(output["question"], "ship it?");
        assert_eq!(
            stored_progress(&admission).await.state,
            JobState::AwaitingUser
        );
        finish(admission).await;
    }

    #[tokio::test]
    async fn host_refusals_settle_failed_with_their_plugin_job_code() {
        for refusal in HOST_REFUSALS {
            let name = format!("driver-refused-{}", refusal.code().unwrap());
            let (admission, row) = settled(&name, Settlement::Refused(refusal)).await;
            assert_eq!(row["lifecycle_state"], "failed", "{refusal:?}");
            let output = payload(&stored_output(&admission).await);
            assert_eq!(output["status"], "failed", "{refusal:?}");
            assert_eq!(output["code"], refusal.code().unwrap(), "{refusal:?}");
            finish(admission).await;
        }
    }

    #[tokio::test]
    async fn max_wall_secs_settles_timed_out() {
        let (admission, row) =
            settled("driver-timed-out", Settlement::Refused(Refusal::TimedOut)).await;
        assert_eq!(row["lifecycle_state"], "timedOut");
        assert_eq!(
            payload(&stored_output(&admission).await)["code"],
            "plugin_job_timed_out"
        );
        finish(admission).await;
    }

    #[tokio::test]
    async fn stop_control_settles_cancelled_and_pause_control_settles_paused_by_user() {
        let (stopped, row) = settled(
            "driver-control-stop",
            Settlement::Control(JobControlKind::Stop),
        )
        .await;
        assert_eq!(row["lifecycle_state"], "cancelled");
        finish(stopped).await;

        let (paused, row) = settled(
            "driver-control-pause",
            Settlement::Control(JobControlKind::Pause),
        )
        .await;
        assert_eq!(row["lifecycle_state"], "completed");
        assert_eq!(payload(&stored_output(&paused).await)["status"], "paused");
        let progress = stored_progress(&paused).await;
        assert_eq!(progress.state, JobState::Paused);
        assert_eq!(progress.pause_reason, Some(JobPauseReason::User));
        finish(paused).await;
    }

    #[test]
    fn host_code_payloads_name_their_next_call() {
        for refusal in HOST_REFUSALS.into_iter().chain([Refusal::TimedOut]) {
            let terminal =
                terminal(Settlement::Refused(refusal), "jobs", "session-a:call-1").unwrap();
            assert_eq!(terminal.output["code"], refusal.code().unwrap());
            let next = terminal.output["next"].as_str().unwrap();
            assert!(next.starts_with("Next: "), "{refusal:?}: {next}");
            assert!(next.contains("jobs"), "{refusal:?}: {next}");
        }
        assert_eq!(
            terminal(Settlement::Refused(Refusal::Terminal), "jobs", "s:c"),
            None
        );
    }

    #[tokio::test]
    async fn settled_job_row_holds_exactly_one_receipt() {
        let (admission, row) = settled("driver-receipt", Settlement::Result(json!("done"))).await;
        let stored: PluginExecutionReceipt =
            serde_json::from_value(row["plugin_execution_receipt"].clone()).unwrap();
        assert_eq!(stored, evidence("step"));

        let mut late = crate::tool_call_lifecycle::ToolCallLifecycle::load_by_doc_id(
            admission.node.clone(),
            admission.tool.doc_id().unwrap(),
            admission.tool.node_did(),
            admission.tool.session_id(),
            admission.tool.requester_did(),
        )
        .await
        .unwrap()
        .unwrap();
        // A late settler adopts the durable terminal and writes no second receipt.
        late.stage_plugin_receipt(Some(evidence("another step")));
        settle_plugin_job(&mut late, Settlement::Result(json!("again")))
            .await
            .unwrap();
        let row = stored_row(&admission).await;
        assert_eq!(row["lifecycle_state"], "completed");
        assert_eq!(
            serde_json::from_value::<PluginExecutionReceipt>(
                row["plugin_execution_receipt"].clone()
            )
            .unwrap(),
            evidence("step")
        );
        finish(admission).await;
    }

    #[tokio::test]
    async fn running_job_row_holds_no_receipt() {
        let (admission, row) =
            settled("driver-running", Settlement::Refused(Refusal::Terminal)).await;
        assert_eq!(row["lifecycle_state"], "running");
        assert!(row["plugin_execution_receipt"].is_null());
        finish(admission).await;
    }

    #[tokio::test]
    async fn non_inline_settlement_appends_one_continuation_completion() {
        let (admission, _) = settled(
            "driver-completion",
            Settlement::Fail(JobFailure {
                code: "audit_rejected".to_owned(),
                message: "the audit found a blocker".to_owned(),
            }),
        )
        .await;
        let access = ConfigAccess::local(admission.node.clone());
        let key = crate::background_completion::background_completion_notification_message_key(
            admission.tool.doc_id().unwrap(),
            "tool",
        );
        let notices = access
            .execute(&format!(
                r#"{{ AgentMessage(filter: {{ message_key: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
                escape_graphql_string(&key),
            ))
            .await
            .unwrap();
        assert_eq!(notices["data"]["AgentMessage"].as_array().unwrap().len(), 1);
        let requests = access
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ session_id: {{ _eq: "{}" }} }}) {{ request_id request_hop }} }}"#,
                escape_graphql_string(admission.tool.session_id()),
            ))
            .await
            .unwrap();
        let requests = requests["data"]["AgentRequest"].as_array().unwrap();
        assert_eq!(requests.len(), 2, "the caller's request and one wake");
        assert!(
            requests
                .iter()
                .all(|request| request["request_hop"] == requests[0]["request_hop"]),
            "a Continuation wake keeps the caller's hop: {requests:?}"
        );
        finish(admission).await;
    }

    async fn persist_step_state(admission: &PublishedAdmission, evidence: &PluginExecutionReceipt) {
        let doc_id = admission.tool.doc_id().unwrap().to_owned();
        ConfigAccess::transact_local_write(
            &admission.node,
            None,
            "test.driver.step_state",
            |txn| {
                let doc_id = doc_id.clone();
                Box::pin(async move {
                    super::super::records::write_step_state_in_txn(txn, &doc_id, evidence, None)
                        .await
                })
            },
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn settle_of_a_lifecycle_loaded_by_doc_id_publishes_the_persisted_evidence() {
        let admission = bare_job_row("driver-persisted-evidence").await;
        store_running_progress(&admission).await;
        persist_step_state(&admission, &evidence("persisted")).await;
        let mut loaded = crate::tool_call_lifecycle::ToolCallLifecycle::load_by_doc_id(
            admission.node.clone(),
            admission.tool.doc_id().unwrap(),
            admission.tool.node_did(),
            admission.tool.session_id(),
            admission.tool.requester_did(),
        )
        .await
        .unwrap()
        .unwrap();
        settle_plugin_job(&mut loaded, Settlement::Refused(Refusal::Stalled))
            .await
            .unwrap();
        let row = stored_row(&admission).await;
        assert_eq!(row["lifecycle_state"], "failed");
        let stored: PluginExecutionReceipt =
            serde_json::from_value(row["plugin_execution_receipt"].clone()).unwrap();
        assert_eq!(
            stored,
            PluginExecutionReceipt {
                output_digest: None,
                verdict: PluginExecutionVerdict::ExecutionError,
                ..evidence("persisted")
            }
        );
        finish(admission).await;
    }

    #[tokio::test]
    async fn staged_evidence_wins_over_the_persisted_one() {
        let mut admission = job_row("driver-staged-wins").await;
        persist_step_state(&admission, &evidence("persisted")).await;
        admission
            .tool
            .stage_plugin_receipt(Some(evidence("staged")));
        settle_plugin_job(&mut admission.tool, Settlement::Result(json!("ok")))
            .await
            .unwrap();
        let row = stored_row(&admission).await;
        assert_eq!(
            serde_json::from_value::<PluginExecutionReceipt>(
                row["plugin_execution_receipt"].clone()
            )
            .unwrap(),
            evidence("staged")
        );
        finish(admission).await;
    }

    use crate::plugin_job::input::tests::{live_settings, new_job, request, resume_block};
    use crate::plugin_job::keys::{job_session_id, job_spawn_key};
    use crate::plugin_job::records::{write_progress_in_txn, HostProgressFacts};
    use gents_protocol::job::{JobBudget, JobPolicy};

    fn running_progress() -> JobProgress {
        JobProgress {
            title: "audit".to_owned(),
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
            last_event: "scanning".to_owned(),
            last_event_at: None,
            question: None,
            observed_terminal_children: 0,
            revision: 0,
        }
    }

    #[tokio::test]
    async fn read_step_row_carries_the_pin_progress_and_derived_children() {
        let live = live_settings(JobPolicy::default());
        let mut job = new_job(Some(&live), None).await;
        let row = job
            .row(
                "workflow",
                "call-1",
                json!({"argv": ["run"], "args": {"n": 1}, "agent_budget": 7}),
            )
            .await;
        let pin = job.pin(&row, None);
        job.write_pin(&row, &pin).await;
        job.write_progress(&row, JobState::Running, 1).await;
        request(
            &job.node,
            &row,
            "child-1",
            &job_session_id(&row, "slot-a", 0),
            &job_spawn_key(&row, "slot-a", 0),
            "agent-a",
            "processing",
            Some("Alpha"),
            "2026-01-01T00:00:00Z",
            None,
        )
        .await;
        let step = read_step_row(&job.access, &row).await.unwrap();
        let progress = read_progress(&job.access, &row).await.unwrap().unwrap();
        assert_eq!(step.pin, Some(pin.clone()));
        assert_eq!(step.root, pin.chain_root_doc_id);
        assert_eq!(step.root, row);
        assert_eq!(step.row_doc_id, row);
        assert_eq!(step.caller_session, "session-1");
        assert_eq!(step.caller_cwd, pin.cwd);
        assert_eq!(step.handle, pin.handle);
        assert_eq!(step.caller_hop, 0);
        assert_eq!(step.call.argv, ["run"]);
        assert_eq!(step.call.args.as_deref(), Some(r#"{"n":1}"#));
        assert_eq!(step.call.agent_budget, Some(7));
        assert_eq!(step.root_call, None);
        assert_eq!(step.spent, progress.spawned);
        assert_eq!(step.spent, 1);
        assert_eq!(step.observed_terminal, progress.observed_terminal_children);
        assert_eq!(step.state, JobState::Running);
        let derived = crate::plugin_job::input::derive_children(
            &job.access,
            std::slice::from_ref(&row),
            pin.ceiling_at_pin.result_bytes,
        )
        .await
        .unwrap();
        assert_eq!(derived.len(), 1);
        assert_eq!(
            step.children,
            derived
                .into_iter()
                .map(|(child, _)| child)
                .collect::<Vec<_>>()
        );
        assert_eq!(step.resume, None);

        let block = resume_block("prior");
        job.write_step_state(&row, Some(&block)).await;
        let step = read_step_row(&job.access, &row).await.unwrap();
        assert_eq!(step.resume, Some(block));
        job.node.shutdown().await;
    }

    #[tokio::test]
    async fn read_step_row_in_txn_sees_the_transactions_own_writes() {
        let live = live_settings(JobPolicy::default());
        let mut job = new_job(Some(&live), None).await;
        let row = job
            .row("workflow", "call-1", json!({"argv": ["run"]}))
            .await;
        job.write_pin(&row, &job.pin(&row, None)).await;
        job.write_progress(&row, JobState::Running, 1).await;
        let row = row.as_str();
        let spent =
            ConfigAccess::transact_local_write(&job.node, None, "test.driver.own_writes", |txn| {
                Box::pin(async move {
                    let host = HostProgressFacts {
                        spawned: 3,
                        budget: JobBudget {
                            total: 4,
                            spent: 3,
                            remaining: 1,
                        },
                        observed_terminal_children: 0,
                        state: JobState::Running,
                        pause_reason: None,
                    };
                    write_progress_in_txn(txn, row, 1, running_progress(), host).await?;
                    Ok(read_step_row_in_txn(txn, row).await?.spent)
                })
            })
            .await
            .unwrap();
        assert_eq!(spent, 3);
        job.node.shutdown().await;
    }

    use crate::pack::{write_home_install, HomePackInstall, PackKind};
    use crate::plugin::store;
    use crate::plugin::tests::constant_output_wat;
    use crate::plugin::tests::executor::installed_plugin;
    use crate::plugin_job::input::tests::{insert, node_did, Job};
    use gents_protocol::job::{
        DynamicMode, JobChildState, JobOrigin, PluginJobDeclaration, PluginJobSettings,
    };

    /// A running job row of `acme/engine` (dynamic origin, under a policy that
    /// runs dynamic code, progress at revision 1) whose installed guest returns
    /// one output on every step.
    pub(crate) struct Stepper {
        pub(crate) job: Job,
        pub(crate) row: String,
        installed: InstalledPlugin,
    }

    fn step_settings(targets: &[&str]) -> PluginJobSettings {
        let ids: Vec<String> = targets.iter().map(|target| (*target).to_owned()).collect();
        let mut settings = live_settings(JobPolicy {
            dynamic: DynamicMode::Run,
            dynamic_targets: ids.clone(),
            ..Default::default()
        });
        settings.agent_targets = ids;
        settings
    }

    /// The inference chain the job fixture's Agent names, so its Tools can be
    /// rewritten under reference validation.
    async fn write_inference(job: &Job) {
        use crate::config_client::{DesiredStateApplyDocument, DesiredStateApplyPlan};
        let documents = [
            (
                crate::Collection::InferenceProfile,
                json!({"node_did": node_did(), "profile_id": "inference", "backend_id": "backend",
                       "model_name": "test-model"}),
            ),
            (
                crate::Collection::InferenceBackend,
                json!({"node_did": node_did(), "backend_id": "backend", "name": "Test inference",
                       "provider_kind": "OpenAiCompatible", "endpoint": "http://127.0.0.1:1/v1",
                       "auth": {"kind": "unauthenticated"}}),
            ),
        ]
        .into_iter()
        .map(|(collection, value)| DesiredStateApplyDocument {
            collection,
            add: value.clone(),
            update: value,
        })
        .collect();
        let plan = DesiredStateApplyPlan::new(documents).unwrap();
        ConfigAccess::transact_local_write(&job.node, None, "test.driver.inference", |txn| {
            let plan = &plan;
            Box::pin(async move { crate::config_client::apply_desired_state_plan(txn, plan).await })
        })
        .await
        .unwrap();
    }

    pub(crate) async fn write_step_tools(job: &Job, settings: Option<&PluginJobSettings>) {
        let tools: crate::document_config::Tools = serde_json::from_value(json!({
            "tools_id": "tools",
            "node_did": node_did(),
            "agents": {"enabled": true, "target_ids": ["t-a", "t-b"]},
            "integrations": {"plugins": [{"plugin": "acme/engine", "job": settings}]},
        }))
        .unwrap();
        crate::config_client::write_tools_document(&job.access, &tools)
            .await
            .unwrap();
    }

    /// Installs `output` as the guest of `acme/engine` under the job's home,
    /// recorded as an operator install.
    fn install_engine(job: &Job, output: &Value) -> InstalledPlugin {
        install_engine_wat(job, &constant_output_wat(output.to_string().as_bytes()))
    }

    fn install_engine_wat(job: &Job, wat: &str) -> InstalledPlugin {
        let (source, record) = installed_plugin(wat, None);
        let hex = record.digest.strip_prefix("sha256:").unwrap();
        let home = &job.runtime.home;
        store::store_bytes(home, hex, &store::read_bytes(source.path(), hex).unwrap()).unwrap();
        let record = InstalledPlugin {
            namespace: "acme".into(),
            name: "engine".into(),
            owner_pack_coordinate: Some("acme/engine".into()),
            declaration: crate::pack::PackPlugin {
                job: PluginJobDeclaration(true),
                ..record.declaration.clone()
            },
            ..record
        };
        store::write_record(home, &record).unwrap();
        write_home_install(
            home,
            &HomePackInstall {
                coordinate: "acme/engine".into(),
                version: "1.0.0".into(),
                digest: record.digest.clone(),
                kind: PackKind::Plugins,
                assets: String::new(),
                dependency_assets: vec![],
                plugins: vec![],
                ux: Vec::new(),
                installed_at: "2026-01-01T00:00:00Z".into(),
                installed_via: Some(crate::pack::InstalledVia::Operator),
            },
        )
        .unwrap();
        record
    }

    pub(crate) async fn start_running(job: &Job, row: &str) {
        let mut lifecycle =
            ToolCallLifecycle::load_by_doc_id(job.node.clone(), row, node_did(), "session-1", None)
                .await
                .unwrap()
                .unwrap();
        lifecycle.start_running().await.unwrap();
    }

    /// A running pinned job row of `job` (its own chain root), progress at revision 1.
    pub(crate) async fn pinned_job_row(job: &mut Job, call_id: &str) -> String {
        let row = job.row("workflow", call_id, json!({"argv": ["run"]})).await;
        job.write_pin(&row, &job.pin(&row, None)).await;
        job.write_progress(&row, JobState::Running, 1).await;
        start_running(job, &row).await;
        row
    }

    /// Ends `row` with `settlement` through the production settle owner.
    pub(crate) async fn settle_row(job: &Job, row: &str, settlement: Settlement) {
        let mut lifecycle = load_lifecycle(&job.access, row).await.unwrap();
        settle_plugin_job(&mut lifecycle, settlement).await.unwrap();
    }

    #[tokio::test]
    async fn read_step_row_state_follows_the_row_lifecycle_first() {
        let mut job = new_job(Some(&live_settings(JobPolicy::default())), None).await;
        let cases = [
            (
                Some(Settlement::Fail(JobFailure {
                    code: "engine_boom".into(),
                    message: "boom".into(),
                })),
                JobState::Failed,
            ),
            (
                Some(Settlement::Control(JobControlKind::Stop)),
                JobState::Cancelled,
            ),
            (
                Some(Settlement::Refused(Refusal::TimedOut)),
                JobState::TimedOut,
            ),
            (Some(Settlement::Result(json!("done"))), JobState::Completed),
            (
                Some(Settlement::Control(JobControlKind::Pause)),
                JobState::Paused,
            ),
            (
                Some(Settlement::Suspend(JobSuspend {
                    kind: JobSuspendKind::AwaitUser,
                    reason: None,
                    message: "needs a decision".into(),
                    question: Some("ship it?".into()),
                })),
                JobState::AwaitingUser,
            ),
            (None, JobState::Running),
        ];
        for (index, (settlement, expected)) in cases.into_iter().enumerate() {
            let row = pinned_job_row(&mut job, &format!("call-{}", index + 1)).await;
            if let Some(settlement) = settlement {
                settle_row(&job, &row, settlement).await;
            }
            let stored = read_step_row(&job.access, &row).await.unwrap();
            assert_eq!(stored.state, expected, "case {}", index + 1);
        }
        job.node.shutdown().await;
    }

    #[tokio::test]
    async fn control_outcome_of_a_settled_row_read_back_is_already_terminal() {
        let mut job = new_job(Some(&live_settings(JobPolicy::default())), None).await;
        let row = pinned_job_row(&mut job, "call-1").await;
        settle_row(&job, &row, Settlement::Result(json!("done"))).await;
        let stored = read_step_row(&job.access, &row).await.unwrap();
        assert_eq!(
            control_outcome(&stored, JobControlKind::Stop, &|_| {
                KillObservation::CausedTerminal
            }),
            JobControlOutcome::AlreadyTerminal {
                state: JobState::Completed
            }
        );
        job.node.shutdown().await;
    }

    pub(crate) async fn stepper(output: Value) -> Stepper {
        stepper_wat(&constant_output_wat(output.to_string().as_bytes())).await
    }

    async fn stepper_wat(wat: &str) -> Stepper {
        let settings = step_settings(&["t-a", "t-b"]);
        let mut job = new_job(Some(&settings), None).await;
        write_inference(&job).await;
        for agent in ["a", "b"] {
            insert(
                &job.node,
                "Agent",
                json!({"agent_id": agent, "node_did": node_did(), "context_id": "context",
                       "inference_profile_id": "inference", "enabled": true}),
            )
            .await;
            insert(
                &job.node,
                "AgentTarget",
                json!({"target_id": format!("t-{agent}"), "node_did": node_did(),
                       "target_node_did": node_did(), "agent_id": agent, "name": agent}),
            )
            .await;
        }
        write_step_tools(&job, Some(&settings)).await;
        let installed = install_engine_wat(&job, wat);
        let row = job
            .row("workflow", "call-1", json!({"argv": ["run"]}))
            .await;
        let pin = JobPin {
            origin: JobOrigin::Dynamic,
            engine_digest: installed.digest.clone(),
            source_digest: "sha256:script".to_owned(),
            adopted_at: u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap(),
            agent_budget: 4,
            ceiling_at_pin: crate::plugin_job::settings::effective_ceiling(Some(&settings)),
            ..job.pin(&row, None)
        };
        job.write_pin(&row, &pin).await;
        job.write_progress(&row, JobState::Running, 1).await;
        start_running(&job, &row).await;
        Stepper {
            job,
            row,
            installed,
        }
    }

    impl Stepper {
        pub(crate) async fn row_json(&self, row: &str) -> Value {
            self.job
                .access
                .execute(&format!(
                    r#"{{ AgentToolCall(docID: "{}") {{ lifecycle_state plugin_execution_receipt }} }}"#,
                    escape_graphql_string(row),
                ))
                .await
                .unwrap()["data"]["AgentToolCall"][0]
                .clone()
        }

        async fn lifecycle(&self, row: &str) -> ToolCallLifecycle {
            load_lifecycle(&self.job.access, row).await.unwrap()
        }

        /// The row's closed canonical output.
        pub(crate) async fn output(&self, row: &str) -> Value {
            let tool = self.lifecycle(row).await;
            match crate::background_tools::observe_canonical_tool_output_with_access(
                &self.job.access,
                tool.doc_id().unwrap(),
                tool.request_doc_id().unwrap(),
                tool.session_id(),
                tool.node_did(),
                tool.requester_did(),
            )
            .await
            .unwrap()
            {
                crate::background_tools::CanonicalToolOutputObservation::Closed(output) => {
                    serde_json::from_str(&output).unwrap()
                }
                crate::background_tools::CanonicalToolOutputObservation::Open(output) => {
                    panic!("job row output is still open: {output}")
                }
            }
        }

        async fn run(&self, row: &str) -> StepOutcome {
            run_step(&self.job.runtime, row).await.unwrap()
        }

        pub(crate) async fn progress(&self, row: &str) -> JobProgress {
            read_progress(&self.job.access, row).await.unwrap().unwrap()
        }

        async fn state(&self, row: &str) -> Option<crate::plugin_job::records::JobStepState> {
            read_step_state(&self.job.access, row).await.unwrap()
        }

        /// Bumps the row's progress to the next revision, as a concurrent
        /// writer between a step's input and its transaction would.
        async fn bump(&self, row: &str) {
            let progress = self.progress(row).await;
            let host = HostProgressFacts {
                spawned: progress.spawned,
                budget: progress.budget.clone(),
                observed_terminal_children: progress.observed_terminal_children,
                state: JobState::Running,
                pause_reason: None,
            };
            let revision = progress.revision;
            ConfigAccess::transact_local_write(&self.job.node, None, "test.driver.bump", |txn| {
                let (progress, host) = (progress.clone(), host.clone());
                Box::pin(
                    async move { write_progress_in_txn(txn, row, revision, progress, host).await },
                )
            })
            .await
            .unwrap();
        }

        /// One step run from `input`, built before the row moved on.
        async fn step_from(&self, row: &str, input: JobStepInput, revision: u64) -> StepOutcome {
            let mut lifecycle = self.lifecycle(row).await;
            step_on(
                &self.job.runtime,
                &mut lifecycle,
                &self.installed,
                input,
                revision,
            )
            .await
            .unwrap()
        }

        async fn input(&self, row: &str) -> (JobStepInput, u64) {
            build_step_input(&self.job.access, &self.job.runtime, row, StepKind::Later)
                .await
                .unwrap()
        }

        /// The receipt the guest's own call on `input` leaves.
        async fn receipt_of(&self, input: &JobStepInput) -> PluginExecutionReceipt {
            let caller = &input.caller;
            let jobs = JobCaller::new(
                self.job.node.clone(),
                ProcessControlScope {
                    request_id: caller.request_id.clone(),
                    session_id: caller.session_id.clone(),
                    node_did: caller.node_did.clone(),
                    requester_did: None,
                },
                StampContext::JobStep {
                    node_did: caller.node_did.clone(),
                    session_id: caller.session_id.clone(),
                    request_doc_id: caller.request_doc_id.clone(),
                    tool_call_doc_id: input.job.row_doc_id.clone(),
                    job_row_doc_id: input.job.row_doc_id.clone(),
                    chain_root_doc_id: input.job.chain_root_doc_id.clone(),
                },
            )
            .with_children(input.job.chain.clone(), input.ceiling.result_bytes);
            let data = DataCaller::new(self.job.node.clone(), &caller.node_did).unwrap();
            self.job
                .runtime
                .plugins
                .call_job_step_with_receipt(
                    &self.installed,
                    serde_json::to_value(input).unwrap(),
                    Some(data),
                    jobs,
                )
                .await
                .1
        }

        pub(crate) async fn close(self) {
            self.job.node.shutdown().await;
        }
    }

    fn stored_receipt(row: &Value) -> PluginExecutionReceipt {
        serde_json::from_value(row["plugin_execution_receipt"].clone()).unwrap()
    }

    struct LiveTools;

    impl gents_loop::tool_effects::ToolEffectDispatcher for LiveTools {
        fn call<'a>(
            &'a self,
            _ordinal: u32,
            _name: &'a str,
            _arguments: &'a str,
        ) -> gents_loop::tool::BoxFuture<
            'a,
            Result<
                crate::tool_call_lifecycle::ToolOutcome,
                gents_loop::tool_effects::ToolEffectError,
            >,
        > {
            Box::pin(async { unreachable!("a job step reached a tool dispatcher") })
        }
    }

    #[tokio::test]
    async fn job_step_runs_without_a_tool_dispatcher() {
        let step = stepper(json!({"result": "done"})).await;
        let live =
            || Some(Arc::new(LiveTools) as Arc<dyn gents_loop::tool_effects::ToolEffectDispatcher>);
        // A granted ordinary call under the same live dispatcher is offered
        // tools, so the step's `false` below is the job entry's, not an
        // absent dispatcher's.
        let (input, _) = step.input(&step.row).await;
        let ordinary = gents_loop::tool_effects::scope(
            live(),
            step.job.runtime.plugins.call_data_bound_with_receipt(
                &step.installed,
                serde_json::to_value(&input).unwrap(),
                None,
                true,
            ),
        )
        .await
        .1;
        assert!(ordinary.authority.unwrap().host_tools);
        assert_eq!(
            gents_loop::tool_effects::scope(live(), step.run(&step.row)).await,
            StepOutcome::Settled
        );
        let evidence = step.state(&step.row).await.unwrap().evidence.unwrap();
        let authority = evidence.authority.unwrap();
        assert!(!authority.host_tools);
        assert_eq!(evidence.verdict, PluginExecutionVerdict::Success);
        step.close().await;
    }

    #[tokio::test]
    async fn failed_slot_shows_state_failed_while_the_row_keeps_running() {
        let step = stepper(json!({"effects": {}})).await;
        request(
            &step.job.node,
            &step.row,
            "child-1",
            &job_session_id(&step.row, "slot-a", 0),
            &job_spawn_key(&step.row, "slot-a", 0),
            "a",
            "failed",
            Some("Alpha"),
            "2026-01-01T00:00:00Z",
            Some("2026-01-01T00:01:00Z"),
        )
        .await;
        let (input, _) = step.input(&step.row).await;
        assert_eq!(input.children["slot-a"].state, JobChildState::Failed);
        assert_eq!(step.run(&step.row).await, StepOutcome::Committed);
        assert_eq!(step.row_json(&step.row).await["lifecycle_state"], "running");
        assert_eq!(step.progress(&step.row).await.observed_terminal_children, 1);
        step.close().await;
    }

    #[tokio::test]
    async fn budget_exceeded_pause_settles_paused() {
        let step = stepper(json!({"suspend": {
            "kind": "pause", "reason": "budget_exceeded", "message": "out of budget"
        }}))
        .await;
        assert_eq!(step.run(&step.row).await, StepOutcome::Settled);
        assert_eq!(
            step.row_json(&step.row).await["lifecycle_state"],
            "completed"
        );
        assert_eq!(step.output(&step.row).await["status"], "paused");
        let progress = step.progress(&step.row).await;
        assert_eq!(progress.state, JobState::Paused);
        assert_eq!(progress.pause_reason, Some(JobPauseReason::BudgetExceeded));
        step.close().await;
    }

    #[tokio::test]
    async fn engine_change_fails_the_job_closed() {
        let step = stepper(json!({"result": "done"})).await;
        install_engine(&step.job, &json!({"result": "changed"}));
        assert_eq!(
            step.run(&step.row).await,
            StepOutcome::Refused(Refusal::EngineChanged)
        );
        assert_eq!(step.row_json(&step.row).await["lifecycle_state"], "failed");
        assert_eq!(
            step.output(&step.row).await["code"],
            "plugin_job_engine_changed"
        );
        step.close().await;
    }

    #[tokio::test]
    async fn narrowed_live_ceiling_is_reread_inside_the_step_transaction() {
        let step = stepper(json!({"effects": {"spawn": [{
            "slot": "slot-a", "attempt": 0, "target": "t-a", "prompt": "audit", "title": "Alpha"
        }]}}))
        .await;
        let (input, revision) = step.input(&step.row).await;
        assert_eq!(input.ceiling.targets, ["t-a", "t-b"]);
        write_step_tools(&step.job, Some(&step_settings(&["t-b"]))).await;
        assert_eq!(
            step.step_from(&step.row, input, revision).await,
            StepOutcome::Refused(Refusal::PolicyDenied)
        );
        assert_eq!(step.row_json(&step.row).await["lifecycle_state"], "failed");
        assert_eq!(
            step.output(&step.row).await["code"],
            "plugin_job_policy_denied"
        );
        step.close().await;
    }

    #[tokio::test]
    async fn stale_step_is_discarded_and_restepped_from_fresh_input() {
        let step = stepper(json!({"result": "done"})).await;
        let (input, revision) = step.input(&step.row).await;
        step.bump(&step.row).await;
        assert_eq!(
            step.step_from(&step.row, input, revision).await,
            StepOutcome::Stale
        );
        assert_eq!(step.row_json(&step.row).await["lifecycle_state"], "running");
        assert_eq!(step.progress(&step.row).await.revision, 2);
        assert_eq!(step.state(&step.row).await, None);

        assert_eq!(step.run(&step.row).await, StepOutcome::Settled);
        assert_eq!(
            step.row_json(&step.row).await["lifecycle_state"],
            "completed"
        );
        assert_eq!(step.progress(&step.row).await.revision, 3);
        step.close().await;
    }

    #[tokio::test]
    async fn terminal_with_a_live_child_fails_closed() {
        let step = stepper(json!({"result": "done"})).await;
        request(
            &step.job.node,
            &step.row,
            "child-1",
            &job_session_id(&step.row, "slot-a", 0),
            &job_spawn_key(&step.row, "slot-a", 0),
            "a",
            "processing",
            Some("Alpha"),
            "2026-01-01T00:00:00Z",
            None,
        )
        .await;
        assert_eq!(
            step.run(&step.row).await,
            StepOutcome::Refused(Refusal::PolicyDenied)
        );
        assert_eq!(step.row_json(&step.row).await["lifecycle_state"], "failed");
        assert_eq!(
            step.output(&step.row).await["code"],
            "plugin_job_policy_denied"
        );
        step.close().await;
    }

    #[tokio::test]
    async fn stall_fails_closed() {
        let step = stepper(json!({"effects": {}})).await;
        let (input, _) = step.input(&step.row).await;
        let own = step.receipt_of(&input).await;
        let earlier = evidence("persisted");
        ConfigAccess::transact_local_write(&step.job.node, None, "test.driver.earlier", |txn| {
            let (row, earlier) = (step.row.clone(), earlier.clone());
            Box::pin(async move { write_step_state_in_txn(txn, &row, &earlier, None).await })
        })
        .await
        .unwrap();
        assert_eq!(
            step.run(&step.row).await,
            StepOutcome::Refused(Refusal::Stalled)
        );
        let row = step.row_json(&step.row).await;
        assert_eq!(row["lifecycle_state"], "failed");
        assert_eq!(step.output(&step.row).await["code"], "plugin_job_stalled");
        let receipt = stored_receipt(&row);
        assert_eq!(receipt.input_digest, own.input_digest);
        assert_ne!(receipt.input_digest, earlier.input_digest);
        step.close().await;
    }

    #[tokio::test]
    async fn result_with_scratch_completes_with_the_file_present() {
        let step = stepper(json!({
            "result": "done",
            "scratch": [{"name": "notes.md", "content": "hello"}],
        }))
        .await;
        let (input, _) = step.input(&step.row).await;
        assert_eq!(step.run(&step.row).await, StepOutcome::Settled);
        assert_eq!(
            step.row_json(&step.row).await["lifecycle_state"],
            "completed"
        );
        let file = Path::new(&input.caller.cwd)
            .join(".gents/workflows/runs")
            .join(&input.caller.tool_call_id)
            .join("notes.md");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello");
        std::fs::remove_file(&file).unwrap();
        assert_eq!(
            step.run(&step.row).await,
            StepOutcome::Refused(Refusal::Terminal)
        );
        assert!(
            !file.exists(),
            "a stale kick of a terminal row must not invoke the guest and recreate scratch"
        );
        step.close().await;
    }

    #[tokio::test]
    async fn committed_step_persists_its_evidence_and_a_stale_step_does_not() {
        let step = stepper(json!({"effects": {"progress": {
            "title": "audit", "state": "running", "phases": [], "active": [], "spawned": 0,
            "budget": {"total": 4, "spent": 1, "remaining": 3}, "last_event": "again",
            "observed_terminal_children": 0
        }}}))
        .await;
        let block = resume_block("prior");
        step.job.write_step_state(&step.row, Some(&block)).await;
        // One observed-but-unaccounted terminal child, so the no-op step is not a stall.
        request(
            &step.job.node,
            &step.row,
            "child-1",
            &job_session_id(&step.row, "slot-a", 0),
            &job_spawn_key(&step.row, "slot-a", 0),
            "a",
            "completed",
            Some("Alpha"),
            "2026-01-01T00:00:00Z",
            Some("2026-01-01T00:01:00Z"),
        )
        .await;
        let (input, _) = step.input(&step.row).await;
        let expected = step.receipt_of(&input).await;
        assert_eq!(step.run(&step.row).await, StepOutcome::Committed);
        let first = step.state(&step.row).await.unwrap();
        assert_eq!(first.evidence, Some(expected));
        assert_eq!(first.resume, Some(block));
        assert!(step.row_json(&step.row).await["plugin_execution_receipt"].is_null());

        let (input, revision) = step.input(&step.row).await;
        step.bump(&step.row).await;
        assert_eq!(
            step.step_from(&step.row, input, revision).await,
            StepOutcome::Stale
        );
        assert_eq!(
            step.state(&step.row).await.unwrap().evidence,
            first.evidence
        );
        step.close().await;
    }

    #[tokio::test]
    async fn terminal_step_settles_inside_the_step_transaction() {
        let step = stepper(json!({"result": "done"})).await;
        let before = step.progress(&step.row).await.revision;
        crate::tool_call_lifecycle::delivery::fail_next_terminal_for_test(&step.row);
        assert_eq!(step.run(&step.row).await, StepOutcome::Settled);
        // The conflict at the terminal compare re-ran the step transaction
        // whole, so the terminal and the progress write commit together; a
        // terminal settled after the step committed would leave one run.
        let runs = STEP_TRANSACTION_RUNS
            .lock()
            .unwrap()
            .iter()
            .filter(|row| **row == step.row)
            .count();
        assert_eq!(runs, 2);
        let row = step.row_json(&step.row).await;
        assert_eq!(row["lifecycle_state"], "completed");
        assert_eq!(step.progress(&step.row).await.revision, before + 1);
        let evidence = step.state(&step.row).await.unwrap().evidence.unwrap();
        assert_eq!(stored_receipt(&row), evidence);
        step.close().await;
    }

    use crate::plugin_job::admit::StepChild;
    use crate::plugin_job::control::KillAction;

    fn failing_row(consecutive_failures: u32) -> StepRow {
        StepRow {
            root: "root".to_owned(),
            row_doc_id: "root".to_owned(),
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
            pin: None,
            children: vec![StepChild {
                retry_key: job_spawn_key("root", "a", 1),
                slot: "a".to_owned(),
                attempt: 1,
                target: ("node".to_owned(), "general".to_owned()),
                state: JobChildState::Live,
                messages: 0,
            }],
            spent: 1,
            observed_terminal: 0,
            consecutive_failures,
            state: JobState::Running,
        }
    }

    #[test]
    fn step_failure_decision_is_lean_on_invocation_failure() {
        let observe = |_: &str| KillObservation::CausedLiveLocal;
        let row = failing_row(1);
        let (second, refusal) = step_failure_decision(&row, &observe);
        assert_eq!(
            second.row,
            StepRow {
                consecutive_failures: 2,
                ..row
            }
        );
        assert!(second.killed.is_empty());
        assert_eq!(refusal, None);

        let row = failing_row(2);
        let (third, refusal) = step_failure_decision(&row, &observe);
        assert_eq!(
            third.row,
            StepRow {
                consecutive_failures: 3,
                state: JobState::Failed,
                ..row.clone()
            }
        );
        let stopped = kill_decision(&row, KillCause::Stop, &observe).killed;
        assert_eq!(
            stopped,
            [(job_spawn_key("root", "a", 1), KillAction::InterruptCaused)]
        );
        assert_eq!(third.killed, stopped);
        assert_eq!(refusal, Some(Refusal::StepFailed));
        assert_eq!(refusal.unwrap().code(), Some("plugin_job_step_failed"));
    }

    /// Waits until `driver` has no row in flight.
    async fn idle(driver: &JobDriver) {
        tokio::time::timeout(std::time::Duration::from_secs(60), async {
            while !driver.in_flight.lock().unwrap().is_empty() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the driver goes idle");
    }

    /// A child request of `step`'s row that is still processing.
    async fn live_child(step: &Stepper, node_did: &str) -> String {
        insert(
            &step.job.node,
            "AgentRequest",
            json!({
                "request_id": "child-1",
                "purpose": "normal",
                "node_did": node_did,
                "requester_did": null,
                "session_id": job_session_id(&step.row, "slot-a", 0),
                "agent_id": "a",
                "lifecycle_state": "processing",
                "retry_key": job_spawn_key(&step.row, "slot-a", 0),
                "created_at": "2026-01-01T00:00:00Z",
                "caused_by_parent_tool_call_doc_id": step.row,
                "caused_by_parent_tool_call_id": "call",
                "caused_by_parent_request_id": "parent",
                "caused_by_parent_request_doc_id": "parent-doc",
            }),
        )
        .await
    }

    #[tokio::test]
    async fn two_wakes_during_a_step_produce_exactly_one_rerun() {
        let step = stepper(json!({"effects": {}})).await;
        live_child(&step, "did-owner").await;
        let driver = JobDriver::new(step.job.runtime.clone());
        // The first kick puts the row in flight; the other two arrive while
        // its step is in flight.
        for _ in 0..3 {
            driver.kick(&step.row);
        }
        idle(&driver).await;
        assert_eq!(step.progress(&step.row).await.revision, 3, "two steps");
        assert_eq!(step.row_json(&step.row).await["lifecycle_state"], "running");
        step.close().await;
    }

    #[tokio::test]
    async fn concurrent_steps_are_bounded_by_the_semaphore() {
        let step = stepper(json!({"effects": {}})).await;
        live_child(&step, "did-owner").await;
        let driver = JobDriver::new(step.job.runtime.clone());
        assert_eq!(driver.permits.available_permits(), 4);
        let held = driver.permits.acquire_many(4).await.unwrap();
        driver.kick(&step.row);
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert_eq!(
            step.progress(&step.row).await.revision,
            1,
            "no permit, no step"
        );
        assert!(driver.in_flight.lock().unwrap().contains_key(&step.row));
        drop(held);
        idle(&driver).await;
        assert_eq!(step.progress(&step.row).await.revision, 2);
        step.close().await;
    }

    #[tokio::test]
    async fn three_consecutive_step_failures_settle_step_failed_with_the_next_call() {
        let step = stepper(json!("not a step output")).await;
        let earlier = evidence("earlier");
        ConfigAccess::transact_local_write(&step.job.node, None, "test.driver.earlier", |txn| {
            let (row, earlier) = (step.row.clone(), earlier.clone());
            Box::pin(async move { write_step_state_in_txn(txn, &row, &earlier, None).await })
        })
        .await
        .unwrap();
        let driver = JobDriver::new(step.job.runtime.clone());
        driver.kick(&step.row);
        idle(&driver).await;
        let row = step.row_json(&step.row).await;
        assert_eq!(row["lifecycle_state"], "failed");
        let output = step.output(&step.row).await;
        assert_eq!(output["code"], "plugin_job_step_failed");
        let next = output["next"].as_str().unwrap();
        assert!(
            next.contains("workflow status") && next.contains("workflow resume"),
            "{next}"
        );
        assert_eq!(
            stored_receipt(&row),
            PluginExecutionReceipt {
                output_digest: None,
                verdict: PluginExecutionVerdict::ExecutionError,
                ..earlier
            }
        );
        assert_eq!(
            step.progress(&step.row).await.revision,
            1,
            "nothing committed"
        );
        step.close().await;
    }

    #[tokio::test]
    async fn two_failures_then_a_success_do_not_settle() {
        let step = stepper(json!({"effects": {}})).await;
        live_child(&step, "did-owner").await;
        let driver = JobDriver::new(step.job.runtime.clone());
        let trap = StepFailure::Trap("boom".to_owned());
        for _ in 0..2 {
            assert_eq!(
                driver.step_failed(&step.row, &trap).await.unwrap(),
                FailureVerdict::Retry
            );
        }
        driver.kick(&step.row);
        idle(&driver).await;
        assert_eq!(step.progress(&step.row).await.revision, 2, "the step ran");
        assert_eq!(
            driver.step_failed(&step.row, &trap).await.unwrap(),
            FailureVerdict::Retry,
            "the success reset the count"
        );
        assert_eq!(step.row_json(&step.row).await["lifecycle_state"], "running");
        step.close().await;
    }

    #[tokio::test]
    async fn step_failures_without_a_remembered_row_read_it_and_settle_on_the_third() {
        let step = stepper(json!({"effects": {}})).await;
        let child = live_child(&step, node_did()).await;
        let driver = JobDriver::new(step.job.runtime.clone());
        let before = read_step_row(&step.job.access, &step.row).await.unwrap();
        let observed = observe_live_children(&step.job.access, &before, node_did())
            .await
            .unwrap();
        let expected = kill_decision(&before, KillCause::Stop, &|key: &str| observed[key]).killed;
        assert_eq!(
            expected,
            [(
                job_spawn_key(&step.row, "slot-a", 0),
                KillAction::InterruptCaused
            )]
        );
        let trap = StepFailure::Trap("before the input".to_owned());
        for verdict in [
            FailureVerdict::Retry,
            FailureVerdict::Retry,
            FailureVerdict::Settle,
        ] {
            assert_eq!(driver.step_failed(&step.row, &trap).await.unwrap(), verdict);
        }
        assert_eq!(step.row_json(&step.row).await["lifecycle_state"], "failed");
        assert_eq!(
            step.output(&step.row).await["code"],
            "plugin_job_step_failed"
        );
        assert!(
            crate::interrupt::fetch_interrupt_requested_at_by_doc_id(&step.job.node, &child)
                .await
                .unwrap()
                .is_some(),
            "the live child is interrupted"
        );
        step.close().await;
    }

    #[tokio::test]
    async fn a_guest_trap_is_a_step_failure() {
        let step = stepper_wat(
            r#"(module (memory (export "memory") 1) (func (export "_start") unreachable))"#,
        )
        .await;
        let error = run_step(&step.job.runtime, &step.row).await.unwrap_err();
        assert!(
            matches!(
                error.downcast_ref::<StepFailure>(),
                Some(StepFailure::Trap(_))
            ),
            "{error:#}"
        );
        assert_eq!(step.row_json(&step.row).await["lifecycle_state"], "running");
        step.close().await;
    }
}
