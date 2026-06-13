use super::*;

impl DefraSessionHook {
    pub(super) async fn persist_background_tool_call(
        &self,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
    ) -> anyhow::Result<ToolCallHookAction> {
        let (session_id, request_id, deadline_at, seq) =
            self.ensure_assistant_turn_sequence().await?;
        self.state.lock().await.register_tool_result_identity(
            internal_call_id,
            None,
            tool_call_id.as_deref(),
        );

        let parsed = match serde_json::from_str::<BackgroundToolArgs>(args) {
            Ok(args) => args,
            Err(error) => {
                return self
                    .fail_background_meta_tool_call(
                        session_id,
                        request_id,
                        deadline_at,
                        seq,
                        internal_call_id,
                        SPAWN_PROCESS_TOOL_NAME,
                        args,
                        FailureClass::ArgumentInvalid,
                        background_invalid_tool_arguments_payload(
                            SPAWN_PROCESS_TOOL_NAME,
                            "/",
                            format!("invalid spawn_process arguments: {error}"),
                        ),
                    )
                    .await;
            }
        };

        let target_name = parsed.tool_name.trim();
        if target_name.is_empty() {
            return self
                .fail_background_meta_tool_call(
                    session_id,
                    request_id,
                    deadline_at,
                    seq,
                    internal_call_id,
                    SPAWN_PROCESS_TOOL_NAME,
                    args,
                    FailureClass::ArgumentInvalid,
                    background_invalid_tool_arguments_payload(
                        SPAWN_PROCESS_TOOL_NAME,
                        "/tool_name",
                        "tool_name is required",
                    ),
                )
                .await;
        }

        let Some(target_tool) = self.background_tool_registry.get(target_name) else {
            return self
                .fail_background_meta_tool_call(
                    session_id,
                    request_id,
                    deadline_at,
                    seq,
                    internal_call_id,
                    SPAWN_PROCESS_TOOL_NAME,
                    args,
                    FailureClass::ServiceUnavailable,
                    background_tool_not_allowed_payload(
                        SPAWN_PROCESS_TOOL_NAME,
                        "/tool_name",
                        target_name,
                        format!(
                            "tool '{target_name}' is not allowed for backgrounding by this behavior"
                        ),
                        self.background_tool_registry.allowlist(),
                    ),
                )
                .await;
        };

        let live_count = count_live_backgrounded_rows(&self.node, &request_id).await?;
        if live_count >= MAX_BACKGROUNDED_TOOLS_PER_PARENT {
            return self
                .fail_background_meta_tool_call(
                    session_id,
                    request_id,
                    deadline_at,
                    seq,
                    internal_call_id,
                    SPAWN_PROCESS_TOOL_NAME,
                    args,
                    FailureClass::ArgumentInvalid,
                    background_budget_exceeded_payload(live_count),
                )
                .await;
        }

        let background_tool_call_id = uuid::Uuid::new_v4().to_string();
        let target_tool_name = target_name.to_string();
        let target_args = serde_json::to_string(&parsed.args)?;
        let mut lifecycle = ToolCallLifecycle::new_background_tool(
            self.node.clone(),
            request_id.clone(),
            session_id.clone(),
            self.agent_did.clone(),
            background_tool_call_id.clone(),
            seq,
            target_tool_name.clone(),
            target_args.clone(),
            deadline_at,
        );
        lifecycle.start_running().await?;

        let cancellation_token = tokio_util::sync::CancellationToken::new();
        self.background_executions
            .insert(background_tool_call_id.clone(), cancellation_token.clone())
            .await;

        let node = self.node.clone();
        let executions = self.background_executions.clone();
        let live_outputs = self.background_live_outputs.clone();
        let execution_call_id = background_tool_call_id.clone();
        let execution_session_id = session_id.clone();
        let execution_request_id = request_id.clone();
        let execution_tool_name = target_tool_name.clone();
        let live_output_writer = live_outputs.writer_for(background_tool_call_id.clone());
        let workspace_cwd = crate::tool_call_lifecycle::runtime::current_tool_runtime_context()
            .and_then(|runtime| runtime.workspace_cwd);
        tokio::spawn(async move {
            let result =
                crate::tool_call_lifecycle::runtime::scope_request_tool_execution_with_workspace_and_live_output(
                    Some(deadline_at),
                    cancellation_token.clone(),
                    workspace_cwd,
                    Some(live_output_writer),
                    async {
                        let tool = target_tool.lock().await;
                        tool.call(target_args).await
                    },
                )
                .await;

            match result {
                Ok(output) => match classify_managed_tool_result(&output) {
                    Some(ManagedToolTerminal::TimedOut) => {
                        if let Err(error) = lifecycle.bridge_failure(ChildTerminal::Dead).await {
                            tracing::warn!(
                                tool_call_id = %execution_call_id,
                                error = %error,
                                "failed to terminalize timed-out background tool"
                            );
                        }
                        if let Err(error) =
                            crate::background_completion::append_background_tool_completion(
                                node.as_ref(),
                                &execution_session_id,
                                &execution_request_id,
                                &execution_call_id,
                                &execution_tool_name,
                                "failed",
                                "",
                                Some("deadline_exceeded"),
                            )
                            .await
                        {
                            tracing::warn!(tool_call_id = %execution_call_id, error = %error, "failed to append timed-out background tool notification");
                        }
                    }
                    Some(ManagedToolTerminal::Cancelled) => {
                        if let Err(error) =
                            lifecycle.bridge_failure(ChildTerminal::Interrupted).await
                        {
                            tracing::warn!(
                                tool_call_id = %execution_call_id,
                                error = %error,
                                "failed to terminalize cancelled background tool"
                            );
                        }
                        if let Err(error) =
                            crate::background_completion::append_background_tool_completion(
                                node.as_ref(),
                                &execution_session_id,
                                &execution_request_id,
                                &execution_call_id,
                                &execution_tool_name,
                                "cancelled",
                                "",
                                Some("parent_cancelled"),
                            )
                            .await
                        {
                            tracing::warn!(tool_call_id = %execution_call_id, error = %error, "failed to append cancelled background tool notification");
                        }
                    }
                    None => {
                        let notification_result = output.clone();
                        if let Err(error) = lifecycle.bridge_complete(output).await {
                            tracing::warn!(
                                tool_call_id = %execution_call_id,
                                error = %error,
                                "failed to complete background tool"
                            );
                        }
                        if let Err(error) =
                            crate::background_completion::append_background_tool_completion(
                                node.as_ref(),
                                &execution_session_id,
                                &execution_request_id,
                                &execution_call_id,
                                &execution_tool_name,
                                "completed",
                                &notification_result,
                                None,
                            )
                            .await
                        {
                            tracing::warn!(tool_call_id = %execution_call_id, error = %error, "failed to append completed background tool notification");
                        }
                    }
                },
                Err(error) => {
                    let reason = format!("{error:#}");
                    let failure_class = classify_runtime_error(&reason);
                    if let Err(error) = lifecycle
                        .bridge_failure(ChildTerminal::Failed {
                            reason: reason.clone(),
                            failure_class,
                        })
                        .await
                    {
                        tracing::warn!(
                            tool_call_id = %execution_call_id,
                            error = %error,
                            "failed to fail background tool"
                        );
                    }
                    if let Err(error) =
                        crate::background_completion::append_background_tool_completion(
                            node.as_ref(),
                            &execution_session_id,
                            &execution_request_id,
                            &execution_call_id,
                            &execution_tool_name,
                            "failed",
                            &reason,
                            Some("tool_failed"),
                        )
                        .await
                    {
                        tracing::warn!(tool_call_id = %execution_call_id, error = %error, "failed to append failed background tool notification");
                    }
                }
            }

            executions.remove(&execution_call_id).await;
            live_outputs.remove(&execution_call_id).await;
        });

        Ok(self.skip_tool_result(
            SPAWN_PROCESS_TOOL_NAME,
            json_string(json!({
                "ok": true,
                "tool_call_id": background_tool_call_id,
                "tool_name": target_tool_name,
                "await_mode": "background",
                "status": "running"
            })),
        ))
    }

    pub(super) async fn persist_wait_tool_call(
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

        let parsed = match serde_json::from_str::<WaitToolArgs>(args) {
            Ok(args) => args,
            Err(error) => {
                return Ok(self.skip_tool_result(
                    WAIT_PROCESS_TOOL_NAME,
                    background_invalid_tool_arguments_payload(
                        WAIT_PROCESS_TOOL_NAME,
                        "/",
                        format!("invalid wait_process arguments: {error}"),
                    ),
                ));
            }
        };
        let background_tool_call_id = parsed.tool_call_id.trim();
        if background_tool_call_id.is_empty() {
            return Ok(self.skip_tool_result(
                WAIT_PROCESS_TOOL_NAME,
                background_invalid_tool_arguments_payload(
                    WAIT_PROCESS_TOOL_NAME,
                    "/tool_call_id",
                    "tool_call_id is required",
                ),
            ));
        }

        let result = match self
            .await_background_tool(&request_id, background_tool_call_id, parent_deadline_at)
            .await
        {
            Ok(result) => result,
            Err(error) => {
                return Ok(self.skip_tool_result(
                    WAIT_PROCESS_TOOL_NAME,
                    background_invalid_tool_arguments_payload(
                        WAIT_PROCESS_TOOL_NAME,
                        "/tool_call_id",
                        format!("{error:#}"),
                    ),
                ));
            }
        };
        Ok(self.skip_tool_result(WAIT_PROCESS_TOOL_NAME, result))
    }

    pub(super) async fn persist_list_background_tools_tool_call(
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

        let parsed = match serde_json::from_str::<ListBackgroundToolsArgs>(args) {
            Ok(args) => args,
            Err(error) => {
                return Ok(self.skip_tool_result(
                    LIST_PROCESSES_TOOL_NAME,
                    background_invalid_tool_arguments_payload(
                        LIST_PROCESSES_TOOL_NAME,
                        "/",
                        format!("invalid list_processes arguments: {error}"),
                    ),
                ));
            }
        };
        let response = handle_list_background_tools(
            &self.node,
            &request_id,
            &self.agent_did,
            &self.background_live_outputs,
            parsed,
        )
        .await?;
        let result = serde_json::to_value(response).map_err(|error| {
            anyhow::anyhow!("serialize list_background_tools response: {error}")
        })?;
        Ok(self.skip_tool_result(LIST_PROCESSES_TOOL_NAME, json_string(result)))
    }

    pub(super) async fn persist_read_tool_output_tool_call(
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

        let parsed = match serde_json::from_str::<ReadToolOutputArgs>(args) {
            Ok(args) => args,
            Err(error) => {
                return Ok(self.skip_tool_result(
                    READ_PROCESS_TOOL_NAME,
                    background_invalid_tool_arguments_payload(
                        READ_PROCESS_TOOL_NAME,
                        "/",
                        format!("invalid read_process arguments: {error}"),
                    ),
                ));
            }
        };
        let background_tool_call_id = parsed.tool_call_id.trim().to_string();
        if background_tool_call_id.is_empty() {
            return Ok(self.skip_tool_result(
                READ_PROCESS_TOOL_NAME,
                background_invalid_tool_arguments_payload(
                    READ_PROCESS_TOOL_NAME,
                    "/tool_call_id",
                    "tool_call_id is required",
                ),
            ));
        }

        match handle_read_tool_output(
            &self.node,
            &request_id,
            &self.background_live_outputs,
            parsed,
        )
        .await?
        {
            ReadToolOutputOutcome::Found(response) => {
                let result = serde_json::to_value(response).map_err(|error| {
                    anyhow::anyhow!("serialize read_tool_output response: {error}")
                })?;
                Ok(self.skip_tool_result(READ_PROCESS_TOOL_NAME, json_string(result)))
            }
            ReadToolOutputOutcome::NotBackgrounded => Ok(self.skip_tool_result(
                READ_PROCESS_TOOL_NAME,
                background_invalid_tool_arguments_payload(
                    READ_PROCESS_TOOL_NAME,
                    "/tool_call_id",
                    "tool_call_id must identify an ordinary backgrounded tool call",
                ),
            )),
            ReadToolOutputOutcome::NotAuthorized => Ok(self.skip_tool_result(
                READ_PROCESS_TOOL_NAME,
                background_tool_not_allowed_payload(
                    READ_PROCESS_TOOL_NAME,
                    "/tool_call_id",
                    &background_tool_call_id,
                    "background tool call is not owned by this parent request",
                    Vec::new(),
                ),
            )),
        }
    }

    pub(super) async fn persist_cancel_tool_call(
        &self,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
    ) -> anyhow::Result<ToolCallHookAction> {
        let (session_id, request_id, _deadline_at, _seq) =
            self.ensure_assistant_turn_sequence().await?;
        self.state.lock().await.register_tool_result_identity(
            internal_call_id,
            None,
            tool_call_id.as_deref(),
        );

        let parsed = match serde_json::from_str::<CancelToolArgs>(args) {
            Ok(args) => args,
            Err(error) => {
                return Ok(self.skip_tool_result(
                    CANCEL_PROCESS_TOOL_NAME,
                    background_invalid_tool_arguments_payload(
                        CANCEL_PROCESS_TOOL_NAME,
                        "/",
                        format!("invalid cancel_process arguments: {error}"),
                    ),
                ));
            }
        };
        let background_tool_call_id = parsed.tool_call_id.trim();
        if background_tool_call_id.is_empty() {
            return Ok(self.skip_tool_result(
                CANCEL_PROCESS_TOOL_NAME,
                background_invalid_tool_arguments_payload(
                    CANCEL_PROCESS_TOOL_NAME,
                    "/tool_call_id",
                    "tool_call_id is required",
                ),
            ));
        }
        if parsed
            .reason
            .as_deref()
            .is_some_and(|reason| reason.trim().is_empty())
        {
            return Ok(self.skip_tool_result(
                CANCEL_PROCESS_TOOL_NAME,
                background_invalid_tool_arguments_payload(
                    CANCEL_PROCESS_TOOL_NAME,
                    "/reason",
                    "reason must be omitted or non-empty",
                ),
            ));
        }

        let lifecycle = match self
            .load_authorized_background_tool(&request_id, background_tool_call_id)
            .await
        {
            Ok(lifecycle) => lifecycle,
            Err(error) => {
                return Ok(self.skip_tool_result(
                    CANCEL_PROCESS_TOOL_NAME,
                    background_invalid_tool_arguments_payload(
                        CANCEL_PROCESS_TOOL_NAME,
                        "/tool_call_id",
                        format!("{error:#}"),
                    ),
                ));
            }
        };
        if lifecycle.is_terminal() {
            let result = self
                .background_tool_envelope(lifecycle, "explicit_cancel")
                .await?;
            return Ok(self.skip_tool_result(CANCEL_PROCESS_TOOL_NAME, result));
        }

        let notification_tool_name = lifecycle.tool_name().to_string();
        self.cancel_background_tool_lifecycle(lifecycle, CancelCause::UserCancelled)
            .await?;
        let notification_reason = parsed
            .reason
            .as_deref()
            .map(str::trim)
            .unwrap_or("explicit_cancel");
        if let Err(error) = crate::background_completion::append_background_tool_completion(
            self.node.as_ref(),
            &session_id,
            &request_id,
            background_tool_call_id,
            &notification_tool_name,
            "cancelled",
            "",
            Some(notification_reason),
        )
        .await
        {
            tracing::warn!(
                tool_call_id = %background_tool_call_id,
                error = %error,
                "failed to append explicitly cancelled background tool notification"
            );
        }
        Ok(self.skip_tool_result(
            CANCEL_PROCESS_TOOL_NAME,
            json_string(json!({
                "ok": true,
                "tool_call_id": background_tool_call_id,
                "status": "cancelled"
            })),
        ))
    }
}
