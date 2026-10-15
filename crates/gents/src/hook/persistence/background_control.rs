use super::*;
use anyhow::Context;

impl DefraSessionHook {
    pub(super) async fn take_owned_in_flight_lifecycle(
        &self,
        internal_call_id: &str,
    ) -> Option<ToolCallLifecycle> {
        self.in_flight_lifecycles
            .lock()
            .await
            .remove(internal_call_id)
    }

    /// A background row this session principal manages: a native process or
    /// an `agent_new`/`agent_message` row.
    pub(super) async fn load_authorized_background_tool(
        &self,
        caller: &ProcessControlScope,
        tool_call_id: &str,
    ) -> anyhow::Result<ToolCallLifecycle> {
        let Some(lifecycle) =
            ToolCallLifecycle::load(self.node.clone(), &caller.session_id, tool_call_id).await?
        else {
            anyhow::bail!("background tool call {tool_call_id} was not found");
        };
        if !caller.authorizes(
            lifecycle.session_id(),
            lifecycle.node_did(),
            lifecycle.requester_did(),
        ) || lifecycle.await_mode() != AwaitMode::Background
        {
            anyhow::bail!("background tool call {tool_call_id} is not manageable by this session");
        }
        Ok(lifecycle)
    }

    pub(super) async fn await_background_tool(
        &self,
        caller: &ProcessControlScope,
        tool_call_id: &str,
        caller_deadline_at: chrono::DateTime<chrono::Utc>,
        wait_deadline_at: chrono::DateTime<chrono::Utc>,
    ) -> anyhow::Result<String> {
        loop {
            let now = chrono::Utc::now();
            let lifecycle = self
                .load_authorized_background_tool(caller, tool_call_id)
                .await?;
            if lifecycle.is_session_message() {
                anyhow::bail!(
                    "{tool_call_id} is a {} call; its result arrives only as a completion message, so wait_process does not wait on it",
                    lifecycle.tool_name()
                );
            }
            if lifecycle.is_terminal() {
                return self.background_tool_envelope(lifecycle, "terminal").await;
            }

            // Waiting is observational. Ending or interrupting this caller's
            // turn must not revoke the separately budgeted background job.
            if crate::interrupt::fetch_interrupt_requested_at_scoped(
                &self.node,
                &caller.request_id,
                &caller.node_did,
                caller.requester_did.as_deref(),
            )
            .await?
            .is_some()
            {
                return self
                    .background_tool_envelope(lifecycle, "caller_interrupted")
                    .await;
            }

            if now >= caller_deadline_at {
                return self
                    .background_tool_envelope(lifecycle, "caller_deadline_exceeded")
                    .await;
            }

            // A bounded wait reports the process as still running; it never
            // cancels it. Completion arrives as the background notification.
            if now >= wait_deadline_at {
                return self
                    .background_tool_envelope(lifecycle, "wait_timeout")
                    .await;
            }

            let remaining = (caller_deadline_at.min(wait_deadline_at) - now)
                .to_std()
                .unwrap_or(Duration::from_millis(0));
            tokio::time::sleep(remaining.min(Duration::from_millis(100))).await;
        }
    }

    /// Returns the lifecycle, whether this call won the terminal compare, and
    /// the host stop verdict when it did.
    pub(super) async fn cancel_background_tool_lifecycle(
        &self,
        mut lifecycle: ToolCallLifecycle,
        cause: CancelCause,
        completion_reason: &str,
    ) -> anyhow::Result<(
        ToolCallLifecycle,
        bool,
        Option<crate::managed_exec::ProcessStopOutcome>,
    )> {
        let won_terminal_compare = if lifecycle.is_running() {
            lifecycle
                .cancel_during_run_owned(cause, completion_reason)
                .await?
        } else {
            false
        };
        // Persist the explicit cancellation before waking the worker. If the
        // token fires first, the worker can win the same running-state compare
        // and replace the user's specific cause with generic `interrupted`.
        let process = if won_terminal_compare {
            let doc_id = lifecycle
                .doc_id()
                .context("cancelled background lifecycle lacks physical identity")?
                .to_owned();
            Some(
                self.background_executions
                    .stop_execution(lifecycle.tool_call_id(), &doc_id)
                    .await,
            )
        } else {
            None
        };
        Ok((lifecycle, won_terminal_compare, process))
    }

    pub(super) async fn background_tool_envelope(
        &self,
        lifecycle: ToolCallLifecycle,
        reason: &str,
    ) -> anyhow::Result<String> {
        let tool_doc_id = lifecycle
            .doc_id()
            .context("background result requires physical tool identity")?;
        let result = if lifecycle.is_spawned_background() {
            // A spawned process owns an exact canonical ToolOutput source but,
            // deliberately, no fabricated provider ToolCall block or direct
            // invocation reply. Read its source through the output owner.
            crate::background_tools::canonical_tool_output(
                self.node.as_ref(),
                tool_doc_id,
                lifecycle
                    .request_doc_id()
                    .context("spawned background result requires request identity")?,
                lifecycle.session_id(),
                lifecycle.node_did(),
                lifecycle.requester_did(),
            )
            .await?
        } else {
            let message = load_tool_call_result(
                &crate::config_client::ConfigAccess::Local(self.node.clone()),
                tool_doc_id,
                lifecycle.node_did(),
                lifecycle.session_id(),
                lifecycle.requester_did(),
            )
            .await?;
            crate::tool_call_lifecycle::query::render_tool_result(&message)?
        };
        let status = lifecycle.state().as_str();
        let error = if lifecycle.state() == crate::tool_call_lifecycle::ToolCallState::Completed {
            serde_json::Value::Null
        } else {
            json!({
                "reason": reason,
                "failure_class": "external"
            })
        };
        Ok(json_envelope_with_bounded_result(
            json!({
                "ok": lifecycle.state() == crate::tool_call_lifecycle::ToolCallState::Completed,
                "tool_call_id": lifecycle.tool_call_id(),
                "tool_name": lifecycle.tool_name(),
                "await_mode": "background",
                "status": status,
                "result": serde_json::Value::Null,
                "error": error
            }),
            "result",
            &result,
            lifecycle.tool_name(),
            &self.truncation_limits,
        ))
    }
}
