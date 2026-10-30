use super::*;

impl DefraSessionHook {
    pub(in crate::hook::persistence) async fn persist_cancel_tool_call(
        &self,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
    ) -> anyhow::Result<ToolCallHookAction> {
        let (session_id, request_id, deadline_at, _seq) =
            self.ensure_assistant_turn_sequence().await?;
        let mut control_lifecycle = self
            .adopt_accepted_tool_dispatch(
                internal_call_id,
                tool_call_id.as_deref(),
                &request_id,
                &session_id,
                CANCEL_PROCESS_TOOL_NAME,
                args,
                deadline_at,
                crate::tool_call_lifecycle::AwaitMode::Foreground,
            )
            .await?;
        control_lifecycle.start_running().await?;

        let parsed = match serde_json::from_str::<CancelToolArgs>(args) {
            Ok(args) => args,
            Err(error) => {
                let result = background_invalid_tool_arguments_payload(
                    CANCEL_PROCESS_TOOL_NAME,
                    "/",
                    format!("invalid cancel_process arguments: {error}"),
                );
                return self
                    .complete_control_tool_call(
                        &mut control_lifecycle,
                        CANCEL_PROCESS_TOOL_NAME,
                        result,
                    )
                    .await;
            }
        };
        let background_tool_call_id = parsed.tool_call_id.trim();
        if background_tool_call_id.is_empty() {
            let result = background_invalid_tool_arguments_payload(
                CANCEL_PROCESS_TOOL_NAME,
                "/tool_call_id",
                "tool_call_id is required",
            );
            return self
                .complete_control_tool_call(
                    &mut control_lifecycle,
                    CANCEL_PROCESS_TOOL_NAME,
                    result,
                )
                .await;
        }
        if parsed
            .reason
            .as_deref()
            .is_some_and(|reason| reason.trim().is_empty())
        {
            let result = background_invalid_tool_arguments_payload(
                CANCEL_PROCESS_TOOL_NAME,
                "/reason",
                "reason must be omitted or non-empty",
            );
            return self
                .complete_control_tool_call(
                    &mut control_lifecycle,
                    CANCEL_PROCESS_TOOL_NAME,
                    result,
                )
                .await;
        }

        let caller = ProcessControlScope {
            request_id: request_id.clone(),
            session_id: session_id.clone(),
            node_did: self.node_did.clone(),
            requester_did: self.active_requester_did().await,
        };
        let lifecycle = match self
            .load_authorized_background_tool(&caller, background_tool_call_id)
            .await
        {
            Ok(lifecycle) => lifecycle,
            Err(error) => {
                let result = background_invalid_tool_arguments_payload(
                    CANCEL_PROCESS_TOOL_NAME,
                    "/tool_call_id",
                    format!("{error:#}"),
                );
                return self
                    .complete_control_tool_call(
                        &mut control_lifecycle,
                        CANCEL_PROCESS_TOOL_NAME,
                        result,
                    )
                    .await;
            }
        };
        if lifecycle.is_terminal() {
            let result = self
                .background_tool_envelope(lifecycle, "explicit_cancel")
                .await?;
            return self
                .complete_control_tool_call(
                    &mut control_lifecycle,
                    CANCEL_PROCESS_TOOL_NAME,
                    result,
                )
                .await;
        }

        if lifecycle.kind() == crate::tool_call_lifecycle::ToolRowKind::SessionMessage {
            let mut lifecycle = lifecycle;
            let (status, request_id) =
                match crate::session::message::kill(&self.node, &mut lifecycle).await? {
                    crate::session::message::KillOutcome::Interrupting { request_id } => {
                        ("interrupting", Some(request_id))
                    }
                    crate::session::message::KillOutcome::Cancelled => ("cancelled", None),
                    crate::session::message::KillOutcome::Settled => ("already_terminal", None),
                };
            let result = json_string(json!({
                "ok": true,
                "tool_call_id": background_tool_call_id,
                "status": status,
                "request_id": request_id,
                "error": null
            }));
            return self
                .complete_control_tool_call(
                    &mut control_lifecycle,
                    CANCEL_PROCESS_TOOL_NAME,
                    result,
                )
                .await;
        }
        if lifecycle.kind() == crate::tool_call_lifecycle::ToolRowKind::PluginJob {
            let result = match crate::plugin_job::control::control_by_doc_id(
                &crate::config_client::ConfigAccess::local(self.node.clone()),
                &caller,
                lifecycle
                    .doc_id()
                    .context("job cancellation lacks physical identity")?,
                gents_protocol::job::JobControlKind::Stop,
            )
            .await
            {
                Ok(outcome) => json_string(json!({
                    "ok": true,
                    "tool_call_id": background_tool_call_id,
                    "status": match outcome {
                        gents_protocol::job::JobControlOutcome::Applied { .. } => "cancelled",
                        gents_protocol::job::JobControlOutcome::AlreadyTerminal { .. } => {
                            "already_terminal"
                        }
                    },
                    "request_id": null,
                    "error": null
                })),
                Err(error) => background_invalid_tool_arguments_payload(
                    CANCEL_PROCESS_TOOL_NAME,
                    "/tool_call_id",
                    error.to_string(),
                ),
            };
            return self
                .complete_control_tool_call(
                    &mut control_lifecycle,
                    CANCEL_PROCESS_TOOL_NAME,
                    result,
                )
                .await;
        }
        let notification_reason = parsed
            .reason
            .as_deref()
            .map(str::trim)
            .unwrap_or("explicit_cancel");
        let result = match crate::tool_control::cancel_loaded_background_tool_call(
            self.node.clone(),
            &self.background_executions,
            lifecycle,
            notification_reason,
        )
        .await?
        {
            crate::tool_control::CancelBackgroundToolCallOutcome::Cancelled { .. } => {
                cancel_process_reply(
                    background_tool_call_id,
                    crate::managed_exec::ProcessStopOutcome::Stopped,
                )
            }
            crate::tool_control::CancelBackgroundToolCallOutcome::Unverified => {
                cancel_process_reply(
                    background_tool_call_id,
                    crate::managed_exec::ProcessStopOutcome::StillRunning,
                )
            }
            crate::tool_control::CancelBackgroundToolCallOutcome::Lost => cancel_process_reply(
                background_tool_call_id,
                crate::managed_exec::ProcessStopOutcome::NotOwned,
            ),
            crate::tool_control::CancelBackgroundToolCallOutcome::AlreadyTerminal { state } => {
                json_string(json!({
                    "ok": true, "tool_call_id": background_tool_call_id, "status": "already_terminal", "state": state, "error": null
                }))
            }
            crate::tool_control::CancelBackgroundToolCallOutcome::NotBackground
            | crate::tool_control::CancelBackgroundToolCallOutcome::NotFound => {
                json_string(json!({
                    "ok": false, "tool_call_id": background_tool_call_id, "status": "not_found", "error": null
                }))
            }
        };
        self.complete_control_tool_call(&mut control_lifecycle, CANCEL_PROCESS_TOOL_NAME, result)
            .await
    }
}

fn cancel_process_reply(
    tool_call_id: &str,
    process: crate::managed_exec::ProcessStopOutcome,
) -> String {
    use crate::managed_exec::CancelProcessReply;
    let reply = process.cancel_reply();
    let error = match reply {
        CancelProcessReply::Cancelled => serde_json::Value::Null,
        CancelProcessReply::Lost => json!({
            "reason": "process_lost",
            "message": "this runtime could not prove it owned the process, or the process ended while no runtime observed it; its termination was not verified",
            "failure_class": "external"
        }),
        CancelProcessReply::Unverified => json!({
            "reason": "stop_unverified",
            "message": "the process was signalled but is still observed running",
            "failure_class": "external"
        }),
    };
    json_string(json!({
        "ok": reply == CancelProcessReply::Cancelled,
        "tool_call_id": tool_call_id,
        "status": reply.as_str(),
        "process": process.as_str(),
        "error": error
    }))
}
