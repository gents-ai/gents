use super::*;
use crate::hook::HookError;

type Result<T, E = HookError> = std::result::Result<T, E>;

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
    ) -> Result<ToolCallLifecycle, HookError> {
        let Some(lifecycle) =
            ToolCallLifecycle::load(self.node.clone(), &caller.session_id, tool_call_id).await?
        else {
            return Err(HookError::BackgroundToolNotFound {
                tool_call_id: tool_call_id.to_owned(),
            });
        };
        if !caller.authorizes(
            lifecycle.session_id(),
            lifecycle.node_did(),
            lifecycle.requester_did(),
        ) || lifecycle.await_mode() != AwaitMode::Background
        {
            return Err(HookError::BackgroundToolNotManageable {
                tool_call_id: tool_call_id.to_owned(),
            });
        }
        Ok(lifecycle)
    }

    pub(super) async fn await_background_tool(
        &self,
        caller: &ProcessControlScope,
        tool_call_id: &str,
        caller_deadline_at: chrono::DateTime<chrono::Utc>,
        wait_deadline_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<String> {
        let mut changes =
            crate::subscribe_collections(&self.node, ["AgentToolCall", "AgentRequest"])?;
        loop {
            let now = chrono::Utc::now();
            let lifecycle = self
                .load_authorized_background_tool(caller, tool_call_id)
                .await?;
            if matches!(
                lifecycle.kind(),
                crate::tool_call_lifecycle::ToolRowKind::SessionMessage
                    | crate::tool_call_lifecycle::ToolRowKind::PluginJob
            ) {
                return Err(HookError::CallResultArrivesOnlyCompletionMessage {
                    tool_call_id: tool_call_id.to_string(),
                    tool_name: lifecycle.tool_name().to_string(),
                });
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
            if let Ok(None) = tokio::time::timeout(remaining, changes.recv()).await {
                tokio::time::sleep(remaining).await;
            }
        }
    }

    pub(super) async fn cancel_background_tool_lifecycle(
        &self,
        mut lifecycle: ToolCallLifecycle,
        cause: CancelCause,
        completion_reason: &str,
    ) -> Result<
        (
            ToolCallLifecycle,
            bool,
            Option<crate::managed_exec::ProcessStopOutcome>,
        ),
        HookError,
    > {
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
                .ok_or(HookError::CancelledLifecycleMissingIdentity)?
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
    ) -> Result<String> {
        let tool_doc_id = lifecycle
            .doc_id()
            .ok_or(HookError::BackgroundResultRequiresPhysicalToolIdentity)?;
        let result = if lifecycle.is_spawned_background()
            || crate::toolset::is_bash_tool_name(lifecycle.tool_name())
        {
            crate::background_tools::canonical_tool_output(
                self.node.as_ref(),
                tool_doc_id,
                lifecycle
                    .request_doc_id()
                    .ok_or(HookError::SpawnedBackgroundResultRequiresRequestIdentity)?,
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
