use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result};
use gents_protocol::job::{
    JobPin, JobState, JobStepInput, JobStepOutput, JOB_GUEST_STATE_MAX_BYTES,
};
use gents_protocol::plugin::PluginExecutionReceipt;

use super::super::admit::{admit_step, fresh_spawns, Refusal};
use super::super::control::{kill, KillCause};
use super::super::effects::{
    interrupt_slots, recover_step_commit, stage_recorded_effects_in_txn, write_scratch, JobContext,
};
use super::super::input::{build_step_input, StepKind};
use super::super::keys::job_session_id;
use super::super::records::{
    read_pin, read_progress, write_progress_in_txn, write_step_state_in_txn, HostProgressFacts,
    RecordError,
};
use super::context::step_context;
use super::output::{guest_state_of, progress_of, scratch_of, settlement_of, writes_of};
use super::rows::{load_lifecycle, read_lifecycle_state, read_step_row, read_step_row_in_txn};
use super::settle::settle_plugin_job_in_txn;
use super::{step_failure, JobRuntime, StepFailure};
use crate::background_tools::ProcessControlScope;
use crate::config_client::{ConfigAccess, ConfigApplyTxn};
use crate::plugin::data::{DataCaller, StampContext};
use crate::plugin::job_calls::JobCaller;
use crate::plugin::store::{parse_coordinate, read_record, InstalledPlugin};
use crate::plugin::GuestTrap;
use crate::request::lifecycle::SessionMessageCause;
use crate::session::origin::SessionScope;
use crate::tool_call_lifecycle::{PluginJobTerminal, ToolCallLifecycle, ToolCallState};

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
    Refused(Refusal),
    Stopped,
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
    if !super::super::recovery::step_invocation_allowed(row_running, session_stopped) {
        if !row_running {
            return Ok(StepOutcome::Refused(Refusal::Terminal));
        }
        kill(&access, &mut lifecycle, KillCause::Stop).await?;
        return Ok(StepOutcome::Settled);
    }
    let pin = read_pin(&access, row_doc_id)
        .await?
        .with_context(|| format!("job row {row_doc_id} has no job_pin"))?;
    let (namespace, name) = parse_coordinate(&pin.plugin_ref)?;
    let installed = match read_record(&runtime.home, namespace, name) {
        Ok(record) if record.digest == pin.engine_digest => record,
        Ok(_) => return fail_closed(&access, &mut lifecycle, Refusal::EngineChanged).await,
        Err(_) => return fail_closed(&access, &mut lifecycle, Refusal::NotAvailable).await,
    };
    let (input, revision) = build_step_input(&access, runtime, row_doc_id, StepKind::Later).await?;
    step_on(runtime, &mut lifecycle, &installed, input, revision).await
}

/// The step of `input`, built at progress `revision`, on `installed`.
pub(super) async fn step_on(
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
            // A non-trap call error counts too: it raises no progress
            // revision, so recovery would otherwise re-step the row every sweep.
            let error = anyhow::Error::new(if trapped {
                StepFailure::Trap(format!("{error:#}"))
            } else {
                StepFailure::NotRun(format!("{error:#}"))
            });
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
    // Checked before the step transaction so the driver counts it.
    if let Some(bytes) = guest_state_of(&output)
        .map(|state| state.to_string().len())
        .filter(|bytes| *bytes > JOB_GUEST_STATE_MAX_BYTES)
    {
        let too_large = RecordError::GuestStateTooLarge {
            bytes,
            max: JOB_GUEST_STATE_MAX_BYTES,
        };
        return Err(
            anyhow::Error::new(StepFailure::MalformedOutput(too_large.to_string()))
                .context(format!("the step output of job row {row_doc_id}")),
        );
    }

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
        caller_requester_did: lifecycle.requester_did().map(str::to_owned),
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
        Stepped::Refused(refusal) => {
            lifecycle.stage_plugin_receipt(Some(receipt));
            kill(&access, lifecycle, KillCause::FailClosed(refusal)).await?;
            Ok(StepOutcome::Refused(refusal))
        }
        Stepped::Stopped => {
            lifecycle.stage_plugin_receipt(Some(receipt));
            kill(&access, lifecycle, KillCause::Stop).await?;
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
pub(super) static STEP_TRANSACTION_RUNS: Mutex<Vec<String>> = Mutex::new(Vec::new());

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
        return Ok(Stepped::Stopped);
    }
    let (ctx, installed) = step_context(txn, runtime, input, pin, output, lifecycle).await?;
    let admitted = match admit_step(&ctx, &row, output) {
        Ok(admitted) => admitted,
        Err(refusal) => return Ok(Stepped::Refused(refusal.refusal)),
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
        crate::plugin::application_write::apply_plugin_writes_in_txn(
            txn,
            &grant,
            &stamps,
            writes.to_vec(),
        )
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
    write_step_state_in_txn(txn, row_doc_id, receipt, None, guest_state_of(output)).await?;
    // (f)
    let Some(settlement) = settlement_of(output) else {
        return Ok(Stepped::Committed(None));
    };
    Ok(Stepped::Committed(
        settle_plugin_job_in_txn(txn, lifecycle, settlement, false).await?,
    ))
}

/// Fails the job closed with `refusal`, the step having committed nothing.
async fn fail_closed(
    access: &ConfigAccess,
    lifecycle: &mut ToolCallLifecycle,
    refusal: Refusal,
) -> Result<StepOutcome> {
    kill(access, lifecycle, KillCause::FailClosed(refusal)).await?;
    Ok(StepOutcome::Refused(refusal))
}
