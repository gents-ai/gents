use super::*;

impl DefraSessionHook {
    pub(super) async fn persist_wait_subagent_tool_call(
        &self,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
    ) -> anyhow::Result<ToolCallHookAction> {
        let (_session_id, request_id, parent_deadline_at, _seq) =
            self.ensure_assistant_turn_sequence().await?;
        self.state.lock().await.register_tool_result_identity(
            internal_call_id,
            None,
            tool_call_id.as_deref(),
        );

        let parsed = match serde_json::from_str::<WaitSubagentArgs>(args) {
            Ok(args) => args,
            Err(error) => {
                return Ok(ToolCallHookAction::skip(invalid_tool_arguments_payload(
                    WAIT_SUBAGENT_TOOL_NAME,
                    "/",
                    format!("invalid wait_subagent arguments: {error}"),
                )));
            }
        };
        let child_request_id = parsed.child_request_id.trim();
        if child_request_id.is_empty() {
            return Ok(ToolCallHookAction::skip(invalid_tool_arguments_payload(
                WAIT_SUBAGENT_TOOL_NAME,
                "/child_request_id",
                "child_request_id is required",
            )));
        }

        let parent_context = load_parent_subagent_context(&self.node, &request_id).await?;
        let edge =
            match load_authorized_child_edge(&self.node, &parent_context, child_request_id).await {
                Ok(edge) => edge,
                Err(error) => {
                    return Ok(ToolCallHookAction::skip(service_unavailable_payload(
                        WAIT_SUBAGENT_TOOL_NAME,
                        "/child_request_id",
                        format!(
                        "child subagent request is not available to this parent request: {error}"
                    ),
                        false,
                    )));
                }
            };

        if edge.lifecycle_state == "running" {
            if edge.await_mode == AwaitMode::Background {
                self.foreground_and_track_existing_subagent_bridge(
                    &parent_context,
                    child_request_id,
                    &edge.parent_tool_call_id,
                )
                .await?;
            } else {
                self.track_in_flight_lifecycle_from_storage(
                    &parent_context.session_id,
                    &edge.parent_tool_call_id,
                )
                .await?;
            }
        }

        let result = self
            .await_existing_subagent_bridge(
                &parent_context,
                &edge.parent_tool_call_id,
                &edge.child_request_id,
                &edge.child_session_id,
                &edge.behavior_id,
                parent_deadline_at.min(parent_context.request_deadline_at),
            )
            .await?;

        Ok(ToolCallHookAction::skip(result))
    }

    pub(super) async fn persist_list_subagents_tool_call(
        &self,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
    ) -> anyhow::Result<ToolCallHookAction> {
        let (_session_id, request_id, _deadline_at, _seq) =
            self.ensure_assistant_turn_sequence().await?;
        self.state.lock().await.register_tool_result_identity(
            internal_call_id,
            None,
            tool_call_id.as_deref(),
        );

        let parsed = match serde_json::from_str::<ListSubagentsArgs>(args) {
            Ok(args) => args,
            Err(error) => {
                return Ok(ToolCallHookAction::skip(invalid_tool_arguments_payload(
                    LIST_SUBAGENTS_TOOL_NAME,
                    "/",
                    format!("invalid list_subagents arguments: {error}"),
                )));
            }
        };
        let response =
            handle_list_subagents(&self.node, &request_id, &self.agent_did, parsed).await?;
        let result = serde_json::to_value(response)
            .map_err(|error| anyhow::anyhow!("serialize list_subagents response: {error}"))?;
        Ok(ToolCallHookAction::skip(json_string(result)))
    }

    pub(super) async fn persist_read_subagent_transcript_tool_call(
        &self,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
    ) -> anyhow::Result<ToolCallHookAction> {
        let (_session_id, request_id, _deadline_at, _seq) =
            self.ensure_assistant_turn_sequence().await?;
        self.state.lock().await.register_tool_result_identity(
            internal_call_id,
            None,
            tool_call_id.as_deref(),
        );

        let parsed = match serde_json::from_str::<ReadSubagentTranscriptArgs>(args) {
            Ok(args) => args,
            Err(error) => {
                return Ok(ToolCallHookAction::skip(invalid_tool_arguments_payload(
                    READ_SUBAGENT_TRANSCRIPT_TOOL_NAME,
                    "/",
                    format!("invalid read_subagent_transcript arguments: {error}"),
                )));
            }
        };
        let child_request_id = parsed.child_request_id.trim().to_string();
        if child_request_id.is_empty() {
            return Ok(ToolCallHookAction::skip(invalid_tool_arguments_payload(
                READ_SUBAGENT_TRANSCRIPT_TOOL_NAME,
                "/child_request_id",
                "child_request_id is required",
            )));
        }

        let Some(response) =
            handle_read_subagent_transcript(&self.node, &request_id, parsed).await?
        else {
            return Ok(ToolCallHookAction::skip(tool_not_allowed_payload(
                READ_SUBAGENT_TRANSCRIPT_TOOL_NAME,
                "/child_request_id",
                &child_request_id,
                "child is not a background subagent owned by this parent request",
                Vec::new(),
            )));
        };
        let result = serde_json::to_value(response).map_err(|error| {
            anyhow::anyhow!("serialize read_subagent_transcript response: {error}")
        })?;
        Ok(ToolCallHookAction::skip(json_string(result)))
    }

    pub(super) async fn persist_steer_subagent_tool_call(
        &self,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
    ) -> anyhow::Result<ToolCallHookAction> {
        let (_session_id, request_id, _deadline_at, _seq) =
            self.ensure_assistant_turn_sequence().await?;
        self.state.lock().await.register_tool_result_identity(
            internal_call_id,
            None,
            tool_call_id.as_deref(),
        );

        let parsed = match serde_json::from_str::<SteerSubagentArgs>(args) {
            Ok(args) => args,
            Err(error) => {
                return Ok(ToolCallHookAction::skip(invalid_tool_arguments_payload(
                    STEER_SUBAGENT_TOOL_NAME,
                    "/",
                    format!("invalid steer_subagent arguments: {error}"),
                )));
            }
        };
        let child_request_id = parsed.child_request_id.trim().to_string();
        if child_request_id.is_empty() {
            return Ok(ToolCallHookAction::skip(invalid_tool_arguments_payload(
                STEER_SUBAGENT_TOOL_NAME,
                "/child_request_id",
                "child_request_id is required",
            )));
        }
        let message = parsed.message.trim().to_string();
        if message.is_empty() {
            return Ok(ToolCallHookAction::skip(invalid_tool_arguments_payload(
                STEER_SUBAGENT_TOOL_NAME,
                "/message",
                "message is required",
            )));
        }

        let edge = match load_steer_subagent_target(&self.node, &request_id, &child_request_id)
            .await?
        {
            SteerSubagentTarget::Found(edge) => edge,
            SteerSubagentTarget::NotAuthorized => {
                return Ok(ToolCallHookAction::skip(tool_not_allowed_payload(
                    STEER_SUBAGENT_TOOL_NAME,
                    "/child_request_id",
                    &child_request_id,
                    "child not owned by this parent request",
                    Vec::new(),
                )));
            }
            SteerSubagentTarget::NotBackgrounded => {
                return Ok(ToolCallHookAction::skip(tool_not_allowed_payload(
                    STEER_SUBAGENT_TOOL_NAME,
                    "/child_request_id",
                    &child_request_id,
                    "foreground subagents cannot be steered; call cancel_subagent first",
                    Vec::new(),
                )));
            }
            SteerSubagentTarget::Terminal(state) => {
                return Ok(ToolCallHookAction::skip(invalid_tool_arguments_payload(
                    STEER_SUBAGENT_TOOL_NAME,
                    "/child_request_id",
                    format!("child is in terminal state '{state}'; spawn a new subagent instead"),
                )));
            }
        };

        let mut interrupted_active_request_id = None;
        let mut drained_wake_up_request_ids = Vec::new();
        if parsed.interrupt {
            drained_wake_up_request_ids =
                pending_automated_wakeup_request_ids(&self.node, &edge.child_session_id).await?;
            if let Some(active_request_id) =
                active_session_request_id(&self.node, &edge.child_session_id).await?
            {
                crate::interrupt::interrupt_request(&self.node, &active_request_id).await?;
                let _descendants_cancelled = self
                    .cancel_live_subagent_descendants(&edge.child_session_id)
                    .await?;
                interrupted_active_request_id = Some(active_request_id);
            }
            let post_interrupt_drained = drain_automated_wakeups_returning_ids(
                &self.node,
                &edge.child_session_id,
                "automated wake-up drained because subagent was steered with interrupt=true",
            )
            .await?;
            for request_id in post_interrupt_drained {
                if !drained_wake_up_request_ids
                    .iter()
                    .any(|existing| existing == &request_id)
                {
                    drained_wake_up_request_ids.push(request_id);
                }
            }
        }

        let response = append_steering_request(
            &self.node,
            &request_id,
            &edge,
            &message,
            interrupted_active_request_id,
            drained_wake_up_request_ids,
        )
        .await?;
        let result = serde_json::to_value(response)
            .map_err(|error| anyhow::anyhow!("serialize steer_subagent response: {error}"))?;
        Ok(ToolCallHookAction::skip(json_string(result)))
    }

    pub(super) async fn persist_cancel_subagent_tool_call(
        &self,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
    ) -> anyhow::Result<ToolCallHookAction> {
        let (_session_id, request_id, _deadline_at, _seq) =
            self.ensure_assistant_turn_sequence().await?;
        self.state.lock().await.register_tool_result_identity(
            internal_call_id,
            None,
            tool_call_id.as_deref(),
        );

        let parsed = match serde_json::from_str::<CancelSubagentArgs>(args) {
            Ok(args) => args,
            Err(error) => {
                return Ok(ToolCallHookAction::skip(invalid_tool_arguments_payload(
                    CANCEL_SUBAGENT_TOOL_NAME,
                    "/",
                    format!("invalid cancel_subagent arguments: {error}"),
                )));
            }
        };
        let child_request_id = parsed.child_request_id.trim();
        if child_request_id.is_empty() {
            return Ok(ToolCallHookAction::skip(invalid_tool_arguments_payload(
                CANCEL_SUBAGENT_TOOL_NAME,
                "/child_request_id",
                "child_request_id is required",
            )));
        }
        if parsed
            .reason
            .as_deref()
            .is_some_and(|reason| reason.trim().is_empty())
        {
            return Ok(ToolCallHookAction::skip(invalid_tool_arguments_payload(
                CANCEL_SUBAGENT_TOOL_NAME,
                "/reason",
                "reason must be omitted or non-empty",
            )));
        }

        let parent_context = load_parent_subagent_context(&self.node, &request_id).await?;
        let edge =
            match load_authorized_child_edge(&self.node, &parent_context, child_request_id).await {
                Ok(edge) => edge,
                Err(error) => {
                    return Ok(ToolCallHookAction::skip(service_unavailable_payload(
                        CANCEL_SUBAGENT_TOOL_NAME,
                        "/child_request_id",
                        format!(
                        "child subagent request is not available to this parent request: {error}"
                    ),
                        false,
                    )));
                }
            };

        let reason = parsed
            .reason
            .as_deref()
            .map(str::trim)
            .filter(|reason| !reason.is_empty())
            .unwrap_or("subagent cancelled by parent request");

        let mut queued_drained = crate::interrupt::cancel_subagent_session_queue(
            &self.node,
            &edge.child_session_id,
            reason,
        )
        .await?;
        self.cancel_running_subagent_bridge(
            &parent_context.session_id,
            &edge.parent_tool_call_id,
            "root",
        )
        .await?;
        let active_interrupted =
            crate::interrupt::interrupt_active_session_request(&self.node, &edge.child_session_id)
                .await?;
        let descendants_cancelled = self
            .cancel_live_subagent_descendants(&edge.child_session_id)
            .await?;
        queued_drained += crate::interrupt::cancel_subagent_session_queue(
            &self.node,
            &edge.child_session_id,
            reason,
        )
        .await?;

        Ok(ToolCallHookAction::skip(json_string(json!({
            "ok": true,
            "child_request_id": edge.child_request_id,
            "child_session_id": edge.child_session_id,
            "behavior_id": edge.behavior_id,
            "status": "cancelled",
            "active_interrupted": active_interrupted,
            "descendants_cancelled": descendants_cancelled,
            "queued_drained": queued_drained
        }))))
    }
}
