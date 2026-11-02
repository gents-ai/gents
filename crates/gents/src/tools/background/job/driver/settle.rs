use anyhow::Result;
use serde_json::{json, Value};

use gents_protocol::job::{
    JobControlKind, JobFailure, JobPauseReason, JobState, JobSuspend, JobSuspendKind,
};

use super::super::admit::Refusal;
use crate::plugin::job_calls::job_state;
use crate::store::config_client::ConfigApplyTxn;
use crate::tools::call::lifecycle::{PluginJobTerminal, ToolCallLifecycle, ToolCallState};

/// How a job row ends: the guest's terminal output, a host refusal, or a control.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Settlement {
    Result(serde_json::Value),
    Fail(JobFailure),
    Suspend(JobSuspend),
    Refused(Refusal),
    /// `Refused(StepFailed)` with the third step's own error, so the caller
    /// learns what the guest got wrong without reading the host log.
    StepFailed {
        last_error: String,
    },
    Control(JobControlKind),
}

/// What a settlement writes: the row state, its canonical output, the
/// completion reason, and the job state a paused or awaiting row records.
#[derive(Debug, Clone, PartialEq)]
pub(in crate::tools::background::job) struct Terminal {
    pub(in crate::tools::background::job) state: ToolCallState,
    pub(super) output: Value,
    reason: Option<String>,
    /// The job state, pause reason and, for a host-ended row, the last event
    /// the stored progress is stamped with, so a killed row reads as ended. A
    /// guest terminal (`Result`, `Fail`) is stamped by its own step
    /// transaction and carries `None`.
    progress: Option<(JobState, Option<JobPauseReason>, Option<String>)>,
}

/// The settlement table. `None` for `Refused(Terminal)`: the row already ended.
pub(super) fn terminal(settlement: Settlement, tool: &str, address: &str) -> Option<Terminal> {
    let ended = |state, output, reason| Terminal {
        state,
        output,
        reason,
        progress: None,
    };
    let killed = |state, output, reason, event: String| Terminal {
        state,
        output,
        reason,
        progress: Some((job_state(state, None), None, Some(event))),
    };
    let refused = |refusal: Refusal, last_error: Option<&str>| {
        let code = refusal.code()?;
        let (state, status) = if refusal == Refusal::TimedOut {
            (ToolCallState::TimedOut, "timed_out")
        } else {
            (ToolCallState::Failed, "failed")
        };
        let (message, next) = refusal_text(refusal, tool, address);
        let message = match last_error {
            Some(last_error) => format!("{message}; last: {last_error}"),
            None => message.to_owned(),
        };
        Some(killed(
            state,
            json!({"status": status, "code": code, "message": message, "next": next}),
            Some(code.to_owned()),
            message,
        ))
    };
    let paused = |reason: Option<JobPauseReason>, message: &str| Terminal {
        state: ToolCallState::Completed,
        output: json!({"status": "paused", "reason": reason, "message": message}),
        reason: None,
        progress: Some((JobState::Paused, reason, None)),
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
            progress: Some((JobState::AwaitingUser, None, None)),
        },
        Settlement::StepFailed { last_error } => refused(Refusal::StepFailed, Some(&last_error))?,
        Settlement::Refused(refusal) => refused(refusal, None)?,
        Settlement::Control(JobControlKind::Stop) => killed(
            ToolCallState::Cancelled,
            json!({"status": "cancelled", "message": "the job was stopped"}),
            Some("explicit_cancel".to_owned()),
            "the job was stopped".to_owned(),
        ),
        Settlement::Control(JobControlKind::Pause) => {
            paused(Some(JobPauseReason::User), "the job was paused")
        }
    })
}

/// What a host refusal means and the call that follows it.
pub(in crate::tools::background::job) fn refusal_text(
    refusal: Refusal,
    tool: &str,
    address: &str,
) -> (&'static str, String) {
    let inspect =
        format!("Next: `gents job show {address}`; the `{tool}` help says how a run is resumed");
    match refusal {
        Refusal::Stalled => (
            "the job stopped making progress: every child is terminal and the step spawned nothing and sent no message",
            inspect,
        ),
        Refusal::PolicyDenied => ("the job asked for something its policy refuses", inspect),
        Refusal::DynamicCodeDisabled => (
            "`job.policy.dynamic` is `off`; dynamic script execution is disabled",
            format!("Next: call `{tool}` without a `script` to run the installed plugin, or ask the operator to enable dynamic execution in `job.policy.dynamic`"),
        ),
        Refusal::NotAvailable => (
            "the job's plugin or its job grant is no longer available",
            format!("Next: ask the operator to restore `{tool}` and its `job` block, then call `{tool}` again"),
        ),
        Refusal::EngineChanged => (
            "the job's plugin changed since the job was pinned",
            format!("Next: call `{tool}` again to start a new run; `gents job show {address}` keeps the old one"),
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
        Refusal::ResumeUnknown => ("the job's resume named a row that does not exist", inspect),
        Refusal::ResumeNotHead => {
            ("the job's resume named a row that is not the newest of a run", inspect)
        }
        Refusal::PinMismatch => (
            "the job's step-0 pin did not name the script the call carries (its script_id must be the script's sha256 and its source_digest `sha256:` plus that)",
            format!("Next: fix the plugin's pin, then call `{tool}` again"),
        ),
    }
}

/// The settlement's terminal and its canonical output text; `None` for
/// `Refused(Terminal)`.
pub(in crate::tools::background::job) fn terminal_text(
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
