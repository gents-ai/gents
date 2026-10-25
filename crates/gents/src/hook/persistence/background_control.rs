use super::*;
use anyhow::Context;
use futures::StreamExt;

/// Document invalidations are live-only; quiet and closed feeds still require
/// bounded durable observation without resubmitting or cancelling the process.
const BACKGROUND_TOOL_RESCAN_INTERVAL: Duration = Duration::from_secs(5);

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
        let changes = crate::subscribe_collections(&self.node, ["AgentToolCall", "AgentRequest"])?;
        let changes = futures::stream::unfold(changes, |mut changes| async move {
            changes.recv().await.map(|batch| (batch, changes))
        });
        futures::pin_mut!(changes);
        self.await_background_tool_observing(
            caller,
            tool_call_id,
            caller_deadline_at,
            wait_deadline_at,
            changes,
        )
        .await
    }

    pub(in crate::hook) async fn await_background_tool_observing<S>(
        &self,
        caller: &ProcessControlScope,
        tool_call_id: &str,
        caller_deadline_at: chrono::DateTime<chrono::Utc>,
        wait_deadline_at: chrono::DateTime<chrono::Utc>,
        mut changes: S,
    ) -> anyhow::Result<String>
    where
        S: futures::Stream<Item = events::DocumentChangeBatch> + Unpin,
    {
        let mut rescan = tokio::time::interval_at(
            tokio::time::Instant::now() + BACKGROUND_TOOL_RESCAN_INTERVAL,
            BACKGROUND_TOOL_RESCAN_INTERVAL,
        );
        rescan.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut changes_open = true;
        loop {
            let lifecycle = self
                .load_authorized_background_tool(caller, tool_call_id)
                .await?;
            if matches!(
                lifecycle.kind(),
                crate::tool_call_lifecycle::ToolRowKind::SessionMessage
                    | crate::tool_call_lifecycle::ToolRowKind::PluginJob
            ) {
                anyhow::bail!(
                    "{tool_call_id} is a {} call; its result arrives only as a completion message, so wait_process does not wait on it",
                    lifecycle.tool_name()
                );
            }
            if lifecycle.is_terminal() {
                return self.background_tool_envelope(lifecycle, "terminal").await;
            }

            // Waiting is observational; ending this caller's turn must not
            // revoke the separately budgeted background job.
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

            let now = chrono::Utc::now();
            if now >= caller_deadline_at {
                return self
                    .background_tool_envelope(lifecycle, "caller_deadline_exceeded")
                    .await;
            }

            // A bounded wait reports the process as still running; it never
            // cancels it.
            if now >= wait_deadline_at {
                return self
                    .background_tool_envelope(lifecycle, "wait_timeout")
                    .await;
            }

            let remaining = (caller_deadline_at.min(wait_deadline_at) - now)
                .to_std()
                .unwrap_or(Duration::from_millis(0));
            tokio::select! {
                biased;
                _ = tokio::time::sleep(remaining) => {}
                _ = rescan.tick() => {}
                batch = changes.next(), if changes_open => {
                    changes_open = batch.is_some();
                }
            }
        }
    }

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
        let result = if lifecycle.is_spawned_background()
            || crate::toolset::is_bash_tool_name(lifecycle.tool_name())
        {
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
                &crate::config_client::ConfigAccess::local(self.node.clone()),
                tool_doc_id,
                lifecycle.node_did(),
                lifecycle.session_id(),
                lifecycle.requester_did(),
            )
            .await?;
            crate::tool_call_lifecycle::query::render_tool_result(&message)?
        };
        let now = chrono::Utc::now();
        let started_at = lifecycle
            .started_at()
            .map(|at| at.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true));
        let projection = gents_protocol::wait_status::WaitStatusProjection::from_parts(
            Some(lifecycle.state().as_str()),
            started_at.as_deref(),
            None,
            now,
        );
        let evidence = crate::background_tools::OutputEvidence::from_text(&result);
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
                "started_at": projection.started_at.map(|at| at.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)),
                "elapsed_seconds": projection.running_elapsed_seconds,
                "available_bytes": evidence.total_bytes,
                "preview": evidence.preview,
                "preview_truncated": evidence.preview_truncated,
                "read_at": now.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true),
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
