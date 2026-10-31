use super::*;

impl DefraSessionHook {
    pub(in crate::tools::call::hook::persistence) async fn persist_wait_tool_call(
        &self,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
    ) -> anyhow::Result<ToolCallHookAction> {
        let (session_id, request_id, parent_deadline_at, _seq) =
            self.ensure_assistant_turn_sequence().await?;
        let requester_did = self.active_requester_did().await;
        let mut lifecycle = self
            .adopt_accepted_tool_dispatch(
                internal_call_id,
                tool_call_id.as_deref(),
                &request_id,
                &session_id,
                WAIT_PROCESS_TOOL_NAME,
                args,
                parent_deadline_at,
                crate::tools::call::lifecycle::AwaitMode::Foreground,
            )
            .await?;
        lifecycle.start_running().await?;

        let parsed = match serde_json::from_str::<WaitToolArgs>(args) {
            Ok(args) => args,
            Err(error) => {
                let result = background_invalid_tool_arguments_payload(
                    WAIT_PROCESS_TOOL_NAME,
                    "/",
                    format!("invalid wait_process arguments: {error}"),
                );
                return self
                    .complete_control_tool_call(&mut lifecycle, WAIT_PROCESS_TOOL_NAME, result)
                    .await;
            }
        };
        let background_tool_call_id = parsed.tool_call_id.trim();
        if background_tool_call_id.is_empty() {
            let result = background_invalid_tool_arguments_payload(
                WAIT_PROCESS_TOOL_NAME,
                "/tool_call_id",
                "tool_call_id is required",
            );
            return self
                .complete_control_tool_call(&mut lifecycle, WAIT_PROCESS_TOOL_NAME, result)
                .await;
        }

        let caller = ProcessControlScope {
            request_id,
            session_id,
            node_did: self.node_did.clone(),
            requester_did,
        };
        let wait_policy = match self
            .load_authorized_background_tool(&caller, background_tool_call_id)
            .await
        {
            Ok(target) => self
                .background_tool_registry
                .timeouts()
                .for_execution(target.tool_name(), target.selected_service_id()),
            Err(_) => crate::tools::surface::BackgroundTimeoutPolicy::default(),
        }
        .wait;
        let wait_deadline_at = chrono::Utc::now()
            + chrono::Duration::from_std(wait_policy.wait_for(parsed.timeout_secs))
                .context("wait timeout exceeds the representable range")?;
        self.in_flight_lifecycles
            .lock()
            .await
            .insert(internal_call_id.to_string(), lifecycle);
        let result = match self
            .await_background_tool(
                &caller,
                background_tool_call_id,
                parent_deadline_at,
                wait_deadline_at,
            )
            .await
        {
            Ok(result) => result,
            Err(error) => {
                let Some(mut lifecycle) =
                    self.take_owned_in_flight_lifecycle(internal_call_id).await
                else {
                    anyhow::bail!(
                        "wait_process lifecycle was terminalized while its observation failed"
                    );
                };
                if lifecycle.is_deadline_expired(chrono::Utc::now()) {
                    let _ = lifecycle.timeout().await?;
                    return Ok(ToolCallHookAction::terminate(
                        "request deadline exceeded while waiting for background process",
                    ));
                }
                let result = background_invalid_tool_arguments_payload(
                    WAIT_PROCESS_TOOL_NAME,
                    "/tool_call_id",
                    format!("{error:#}"),
                );
                return self
                    .complete_control_tool_call(&mut lifecycle, WAIT_PROCESS_TOOL_NAME, result)
                    .await;
            }
        };
        let Some(mut lifecycle) = self.take_owned_in_flight_lifecycle(internal_call_id).await
        else {
            anyhow::bail!("wait_process lifecycle was terminalized before observation completion");
        };
        if lifecycle.is_deadline_expired(chrono::Utc::now()) {
            let _ = lifecycle.timeout().await?;
            return Ok(ToolCallHookAction::terminate(
                "request deadline exceeded while waiting for background process",
            ));
        }
        self.complete_control_tool_call(&mut lifecycle, WAIT_PROCESS_TOOL_NAME, result)
            .await
    }

    pub(in crate::tools::call::hook::persistence) async fn persist_list_background_tools_tool_call(
        &self,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
    ) -> anyhow::Result<ToolCallHookAction> {
        let (session_id, request_id, deadline_at, _seq) =
            self.ensure_assistant_turn_sequence().await?;
        let mut lifecycle = self
            .adopt_accepted_tool_dispatch(
                internal_call_id,
                tool_call_id.as_deref(),
                &request_id,
                &session_id,
                LIST_PROCESSES_TOOL_NAME,
                args,
                deadline_at,
                crate::tools::call::lifecycle::AwaitMode::Foreground,
            )
            .await?;
        lifecycle.start_running().await?;

        let parsed = match serde_json::from_str::<ListBackgroundToolsArgs>(args) {
            Ok(args) => args,
            Err(error) => {
                let result = background_invalid_tool_arguments_payload(
                    LIST_PROCESSES_TOOL_NAME,
                    "/",
                    format!("invalid list_processes arguments: {error}"),
                );
                return self
                    .complete_control_tool_call(&mut lifecycle, LIST_PROCESSES_TOOL_NAME, result)
                    .await;
            }
        };
        let caller = ProcessControlScope {
            request_id,
            session_id,
            node_did: self.node_did.clone(),
            requester_did: self.active_requester_did().await,
        };
        let response = handle_list_background_tools(
            &self.node,
            &caller,
            &self.node_did,
            &self.background_live_outputs.registry,
            parsed,
        )
        .await?;
        let result = serde_json::to_value(response).map_err(|error| {
            anyhow::anyhow!("serialize list_background_tools response: {error}")
        })?;
        let result = json_string(result);
        self.complete_control_tool_call(&mut lifecycle, LIST_PROCESSES_TOOL_NAME, result)
            .await
    }

    pub(in crate::tools::call::hook::persistence) async fn persist_read_tool_output_tool_call(
        &self,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
    ) -> anyhow::Result<ToolCallHookAction> {
        let (session_id, request_id, deadline_at, _seq) =
            self.ensure_assistant_turn_sequence().await?;
        let mut lifecycle = self
            .adopt_accepted_tool_dispatch(
                internal_call_id,
                tool_call_id.as_deref(),
                &request_id,
                &session_id,
                READ_PROCESS_TOOL_NAME,
                args,
                deadline_at,
                crate::tools::call::lifecycle::AwaitMode::Foreground,
            )
            .await?;
        lifecycle.start_running().await?;

        let parsed = match serde_json::from_str::<ReadToolOutputArgs>(args) {
            Ok(args) => args,
            Err(error) => {
                let result = background_invalid_tool_arguments_payload(
                    READ_PROCESS_TOOL_NAME,
                    "/",
                    format!("invalid read_process arguments: {error}"),
                );
                return self
                    .complete_control_tool_call(&mut lifecycle, READ_PROCESS_TOOL_NAME, result)
                    .await;
            }
        };
        let background_tool_call_id = parsed.tool_call_id.trim().to_string();
        if background_tool_call_id.is_empty() {
            let result = background_invalid_tool_arguments_payload(
                READ_PROCESS_TOOL_NAME,
                "/tool_call_id",
                "tool_call_id is required",
            );
            return self
                .complete_control_tool_call(&mut lifecycle, READ_PROCESS_TOOL_NAME, result)
                .await;
        }

        let caller = ProcessControlScope {
            request_id,
            session_id,
            node_did: self.node_did.clone(),
            requester_did: self.active_requester_did().await,
        };
        let result = match handle_read_tool_output(
            &self.node,
            &caller,
            &self.background_live_outputs.registry,
            parsed,
        )
        .await?
        {
            ReadToolOutputOutcome::Found(response) => {
                let result = serde_json::to_value(response).map_err(|error| {
                    anyhow::anyhow!("serialize read_tool_output response: {error}")
                })?;
                json_string(result)
            }
            ReadToolOutputOutcome::NotBackgrounded => background_invalid_tool_arguments_payload(
                READ_PROCESS_TOOL_NAME,
                "/tool_call_id",
                "tool_call_id must identify an ordinary backgrounded tool call",
            ),
            ReadToolOutputOutcome::NotAuthorized => background_tool_not_allowed_payload(
                READ_PROCESS_TOOL_NAME,
                "/tool_call_id",
                &background_tool_call_id,
                "background tool call is not manageable by this session",
                Vec::new(),
            ),
        };
        self.complete_control_tool_call(&mut lifecycle, READ_PROCESS_TOOL_NAME, result)
            .await
    }
}
