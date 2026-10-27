use gents_protocol::job::{JobPauseReason, JobState};

use super::super::delivery::{TerminalFields, TerminalJobProgress};
use super::*;
use crate::config_client::{ConfigAccess, ConfigApplyTxn, IdempotentTransactionRetry};
use crate::plugin_job::records::{read_progress, read_step_state, HostProgressFacts};

/// A plugin job terminal written in a caller's transaction, applied to the
/// lifecycle in memory by `finish_plugin_job_row` once that transaction commits.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PluginJobTerminal {
    won: bool,
    state: ToolCallState,
    output: String,
    reason: Option<String>,
    continuation: bool,
}

impl PluginJobTerminal {
    pub(crate) fn won(&self) -> bool {
        self.won
    }
}

fn terminal_fields(state: ToolCallState, reason: Option<&str>) -> TerminalFields<'_> {
    TerminalFields {
        state,
        failure: matches!(state, ToolCallState::Failed | ToolCallState::TimedOut)
            .then_some(FailureClass::External),
        cancel: match state {
            ToolCallState::Cancelled => Some(CancelCause::UserCancelled),
            ToolCallState::TimedOut => Some(CancelCause::Deadline),
            _ => None,
        },
        completion_reason: reason,
    }
}

impl ToolCallLifecycle {
    /// Running -> terminal for a plugin job row, closing its source with
    /// `output`. A background row has its receipt as the native reply, so its
    /// winner appends one Continuation completion instead; a foreground row
    /// (an inline step-0 answer) publishes `output` as the reply. `progress`
    /// is the job state a paused or awaiting row records, written in the
    /// terminal transaction at the stored revision. Returns whether this
    /// caller won the running compare.
    pub(crate) async fn settle_plugin_job_row(
        &mut self,
        state: ToolCallState,
        output: &str,
        reason: Option<&str>,
        progress: Option<(JobState, Option<JobPauseReason>, Option<String>)>,
    ) -> Result<bool> {
        let terminal = ConfigAccess::transact_local_idempotent(
            &self.node,
            None,
            IdempotentTransactionRetry::Standard,
            "tool_call.settle_plugin_job",
            |txn| {
                Box::pin(
                    self.settle_plugin_job_row_in_txn(txn, state, output, reason, progress, false),
                )
            },
        )
        .await?;
        self.finish_plugin_job_row(terminal).await
    }

    /// Running -> terminal for a plugin job row inside `txn`. `inline`
    /// publishes `output` as the call's own reply and appends no
    /// Continuation completion (a pinned step-0 terminal).
    pub(crate) async fn settle_plugin_job_row_in_txn(
        &self,
        txn: &ConfigApplyTxn<'_>,
        state: ToolCallState,
        output: &str,
        reason: Option<&str>,
        progress: Option<(JobState, Option<JobPauseReason>, Option<String>)>,
        inline: bool,
    ) -> Result<PluginJobTerminal> {
        anyhow::ensure!(
            self.kind() == ToolRowKind::PluginJob,
            "plugin job settlement requires a row with a job pin"
        );
        self.ensure_state(&[ToolCallState::Running, state], "settle_plugin_job")?;
        let doc_id = self
            .doc_id
            .as_deref()
            .context("plugin job row lacks physical identity")?;
        let job_progress = match progress {
            Some((job_state, pause_reason, last_event)) => {
                let stored = match (read_progress(txn, doc_id).await?, job_state) {
                    (Some(stored), _) => Some(stored),
                    (None, JobState::Paused | JobState::AwaitingUser) => {
                        anyhow::bail!(
                            "a paused or awaiting job row has no stored progress to update"
                        )
                    }
                    // A row killed before it pinned has no progress to stamp.
                    (None, _) => None,
                };
                stored.map(|mut stored| {
                    if let Some(last_event) = last_event {
                        stored.last_event = last_event;
                    }
                    TerminalJobProgress {
                        expected_revision: stored.revision,
                        host: HostProgressFacts {
                            spawned: stored.spawned,
                            budget: stored.budget.clone(),
                            observed_terminal_children: stored.observed_terminal_children,
                            state: job_state,
                            pause_reason,
                        },
                        guest: stored,
                    }
                })
            }
            None => None,
        };
        let receipt = match self.plugin_receipt.clone() {
            Some(receipt) => Some(receipt),
            None => {
                let persisted = read_step_state(txn, doc_id)
                    .await?
                    .and_then(|state| state.evidence);
                if persisted.is_none() {
                    tracing::warn!(
                        row_doc_id = %doc_id,
                        "job row has no step evidence; settling without an execution receipt"
                    );
                }
                persisted
            }
        };
        let publish_native_result =
            inline || !(self.await_mode == AwaitMode::Background && !self.is_spawned_background());
        let won = self
            .terminalize_bridge_in_txn(
                txn,
                terminal_fields(state, reason),
                output,
                receipt.as_ref(),
                job_progress,
                publish_native_result,
            )
            .await?;
        Ok(PluginJobTerminal {
            won,
            state,
            output: output.to_owned(),
            reason: reason.map(str::to_owned),
            continuation: won && !inline && self.await_mode == AwaitMode::Background,
        })
    }

    /// After the caller's transaction committed: records the terminal in
    /// memory and appends the Continuation completion it owes. Returns
    /// whether this caller won the running compare.
    pub(crate) async fn finish_plugin_job_row(
        &mut self,
        terminal: PluginJobTerminal,
    ) -> Result<bool> {
        if !terminal.won {
            self.sync_after_lost_running_compare("settle_plugin_job")
                .await?;
            return Ok(false);
        }
        let fields = terminal_fields(terminal.state, terminal.reason.as_deref());
        self.state = fields.state;
        self.failure_class = fields.failure;
        self.cancel_cause = fields.cancel;
        if terminal.continuation {
            let doc_id = self
                .doc_id
                .clone()
                .context("plugin job row lacks physical identity")?;
            let calling_request_id =
                crate::session_message::calling_request_id(self.node.as_ref(), self).await?;
            let status = match terminal.state {
                ToolCallState::Completed => "completed",
                ToolCallState::Cancelled => "cancelled",
                _ => "failed",
            };
            crate::background_completion::append_background_tool_completion(
                &self.node,
                &self.session_id,
                &calling_request_id,
                &doc_id,
                &self.tool_name,
                status,
                &terminal.output,
                terminal.reason.as_deref(),
                crate::lifecycle::RequestHopCause::Continuation,
            )
            .await?;
        }
        Ok(true)
    }
}
