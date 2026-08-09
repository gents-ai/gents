use super::*;

impl DefraSessionHook {
    async fn authoritative_skip_projection(
        &self,
        internal_call_id: &str,
        streamed_projection: &str,
    ) -> anyhow::Result<String> {
        let (session_id, registered_tool_name) = {
            let state = self.state.lock().await;
            (
                state
                    .session_id
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("session hook missing session id"))?,
                state.tool_result_registration(internal_call_id).0,
            )
        };
        let Some(durable) =
            load_durable_tool_call_identity(&self.node, &session_id, internal_call_id).await?
        else {
            return Ok(streamed_projection.to_string());
        };
        if registered_tool_name
            .as_deref()
            .is_some_and(|registered| registered != durable.tool_name)
        {
            anyhow::bail!(
                "hook-owned tool-result identity for {internal_call_id} disagrees with durable tool name {}",
                durable.tool_name
            );
        }

        if matches!(
            durable.lifecycle_state.as_str(),
            "completed" | "failed" | "timedOut" | "cancelled"
        ) {
            return Ok(load_stored_tool_call_result(
                &self.node,
                &session_id,
                internal_call_id,
                true,
            )
            .await?
            .result);
        }
        if durable.tool_name == SPAWN_SUBAGENT_TOOL_NAME && durable.lifecycle_state == "running" {
            crate::tool_call_lifecycle::query::verify_exact_subagent_receipt_authority(
                &self.node,
                &session_id,
                internal_call_id,
                streamed_projection,
            )
            .await?;
            let (projection, _, _) = truncate_text(
                streamed_projection,
                TruncationMode::Head,
                &self.truncation_limits,
            );
            return Ok(projection);
        }
        anyhow::bail!(
            "hook-owned durable tool call {session_id}:{internal_call_id} returned Skip while {}",
            durable.lifecycle_state
        )
    }

    async fn record_successful_tool_call_action(
        &self,
        internal_call_id: &str,
        action: ToolCallHookAction,
    ) -> ToolCallHookAction {
        let action = match action {
            ToolCallHookAction::Skip { reason } => {
                match self
                    .authoritative_skip_projection(internal_call_id, &reason)
                    .await
                {
                    Ok(reason) => ToolCallHookAction::Skip { reason },
                    Err(error) => {
                        return self.on_tool_persistence_error(
                            "load authoritative hook-owned tool result",
                            &error,
                        );
                    }
                }
            }
            action => action,
        };
        if matches!(action, ToolCallHookAction::Skip { .. }) {
            self.state
                .lock()
                .await
                .mark_hook_owned_tool_result(internal_call_id);
        }
        self.record_success();
        action
    }

    pub async fn on_completion_call(&self, prompt: &Message, _history: &[Message]) -> HookAction {
        self.on_completion_call_with_context(prompt, _history, None)
            .await
    }

    pub async fn on_completion_call_with_context(
        &self,
        prompt: &Message,
        _history: &[Message],
        context: Option<&Message>,
    ) -> HookAction {
        let result: anyhow::Result<()> = async {
            let mut state = self.state.lock().await;

            if !state.initialized {
                let session_id =
                    session::create_session(&self.node, &state.agent_name, &self.agent_did).await?;
                state.session_id = Some(session_id);
                state.initialized = true;
            }

            state.reset_after_user_message();
            drop(state);

            if let Some(context) = context {
                self.persist_message(context).await?;
            }
            self.persist_message(prompt).await?;
            Ok(())
        }
        .await;

        match result {
            Ok(()) => {
                self.record_success();
                HookAction::Continue
            }
            Err(e) => self.on_persistence_error("persist user prompt", &e),
        }
    }

    pub async fn on_tool_call(
        &self,
        tool_name: &str,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
    ) -> ToolCallHookAction {
        self.state
            .lock()
            .await
            .register_tool_result_tool_name(internal_call_id, tool_name);
        if tool_name == crate::goal::GET_GOAL_TOOL_NAME {
            let result = self
                .persist_get_goal_tool_call(tool_call_id, internal_call_id, args)
                .instrument(tracing::info_span!(
                    "tool.call",
                    tool_name = %tool_name,
                    tool_call_id = %internal_call_id,
                ))
                .await;
            return match result {
                Ok(action) => {
                    self.record_successful_tool_call_action(internal_call_id, action)
                        .await
                }
                Err(error) => self.on_tool_persistence_error("persist get_goal tool call", &error),
            };
        }
        if tool_name == crate::goal::UPDATE_GOAL_TOOL_NAME {
            let result = self
                .persist_update_goal_tool_call(tool_call_id, internal_call_id, args)
                .instrument(tracing::info_span!(
                    "tool.call",
                    tool_name = %tool_name,
                    tool_call_id = %internal_call_id,
                ))
                .await;
            return match result {
                Ok(action) => {
                    self.record_successful_tool_call_action(internal_call_id, action)
                        .await
                }
                Err(error) => {
                    self.on_tool_persistence_error("persist update_goal tool call", &error)
                }
            };
        }
        if tool_name == FAN_OUT_AND_SYNTHESIZE_TOOL_NAME {
            let result = self
                .persist_fan_out_and_synthesize_tool_call(tool_call_id, internal_call_id, args)
                .instrument(tracing::info_span!(
                    "tool.call",
                    tool_name = %tool_name,
                    tool_call_id = %internal_call_id,
                ))
                .await;

            return match result {
                Ok(action) => {
                    self.record_successful_tool_call_action(internal_call_id, action)
                        .await
                }
                Err(e) => {
                    self.on_tool_persistence_error("persist fan_out_and_synthesize tool call", &e)
                }
            };
        }
        if tool_name == SPAWN_SUBAGENT_TOOL_NAME {
            let result = self
                .persist_spawn_subagent_tool_call(tool_call_id, internal_call_id, args)
                .instrument(tracing::info_span!(
                    "tool.call",
                    tool_name = %tool_name,
                    tool_call_id = %internal_call_id,
                ))
                .await;

            return match result {
                Ok(action) => {
                    self.record_successful_tool_call_action(internal_call_id, action)
                        .await
                }
                Err(e) => self.on_tool_persistence_error("persist spawn_subagent tool call", &e),
            };
        }
        if tool_name == WAIT_SUBAGENT_TOOL_NAME {
            let result = self
                .persist_wait_subagent_tool_call(tool_call_id, internal_call_id, args)
                .instrument(tracing::info_span!(
                    "tool.call",
                    tool_name = %tool_name,
                    tool_call_id = %internal_call_id,
                ))
                .await;

            return match result {
                Ok(action) => {
                    self.record_successful_tool_call_action(internal_call_id, action)
                        .await
                }
                Err(e) => self.on_tool_persistence_error("persist wait_subagent tool call", &e),
            };
        }
        if tool_name == LIST_SUBAGENTS_TOOL_NAME {
            let result = self
                .persist_list_subagents_tool_call(tool_call_id, internal_call_id, args)
                .instrument(tracing::info_span!(
                    "tool.call",
                    tool_name = %tool_name,
                    tool_call_id = %internal_call_id,
                ))
                .await;

            return match result {
                Ok(action) => {
                    self.record_successful_tool_call_action(internal_call_id, action)
                        .await
                }
                Err(e) => self.on_tool_persistence_error("persist list_subagents tool call", &e),
            };
        }
        if tool_name == READ_SUBAGENT_TOOL_NAME {
            let result = self
                .persist_read_subagent_tool_call(tool_call_id, internal_call_id, args)
                .instrument(tracing::info_span!(
                    "tool.call",
                    tool_name = %tool_name,
                    tool_call_id = %internal_call_id,
                ))
                .await;

            return match result {
                Ok(action) => {
                    self.record_successful_tool_call_action(internal_call_id, action)
                        .await
                }
                Err(e) => self.on_tool_persistence_error("persist read_subagent tool call", &e),
            };
        }
        if tool_name == STEER_SUBAGENT_TOOL_NAME {
            let result = self
                .persist_steer_subagent_tool_call(tool_call_id, internal_call_id, args)
                .instrument(tracing::info_span!(
                    "tool.call",
                    tool_name = %tool_name,
                    tool_call_id = %internal_call_id,
                ))
                .await;

            return match result {
                Ok(action) => {
                    self.record_successful_tool_call_action(internal_call_id, action)
                        .await
                }
                Err(e) => self.on_tool_persistence_error("persist steer_subagent tool call", &e),
            };
        }
        if tool_name == CANCEL_SUBAGENT_TOOL_NAME {
            let result = self
                .persist_cancel_subagent_tool_call(tool_call_id, internal_call_id, args)
                .instrument(tracing::info_span!(
                    "tool.call",
                    tool_name = %tool_name,
                    tool_call_id = %internal_call_id,
                ))
                .await;

            return match result {
                Ok(action) => {
                    self.record_successful_tool_call_action(internal_call_id, action)
                        .await
                }
                Err(e) => self.on_tool_persistence_error("persist cancel_subagent tool call", &e),
            };
        }
        if tool_name == SPAWN_PROCESS_TOOL_NAME {
            let result = self
                .persist_background_tool_call(tool_call_id, internal_call_id, args)
                .instrument(tracing::info_span!(
                    "tool.call",
                    tool_name = %tool_name,
                    tool_call_id = %internal_call_id,
                ))
                .await;

            return match result {
                Ok(action) => {
                    self.record_successful_tool_call_action(internal_call_id, action)
                        .await
                }
                Err(e) => self.on_tool_persistence_error("persist spawn_process tool call", &e),
            };
        }
        if tool_name == WAIT_PROCESS_TOOL_NAME {
            let result = self
                .persist_wait_tool_call(tool_call_id, internal_call_id, args)
                .instrument(tracing::info_span!(
                    "tool.call",
                    tool_name = %tool_name,
                    tool_call_id = %internal_call_id,
                ))
                .await;

            return match result {
                Ok(action) => {
                    self.record_successful_tool_call_action(internal_call_id, action)
                        .await
                }
                Err(e) => self.on_tool_persistence_error("persist wait_process tool call", &e),
            };
        }
        if tool_name == LIST_PROCESSES_TOOL_NAME {
            let result = self
                .persist_list_background_tools_tool_call(tool_call_id, internal_call_id, args)
                .instrument(tracing::info_span!(
                    "tool.call",
                    tool_name = %tool_name,
                    tool_call_id = %internal_call_id,
                ))
                .await;

            return match result {
                Ok(action) => {
                    self.record_successful_tool_call_action(internal_call_id, action)
                        .await
                }
                Err(e) => self.on_tool_persistence_error("persist list_processes tool call", &e),
            };
        }
        if tool_name == READ_PROCESS_TOOL_NAME {
            let result = self
                .persist_read_tool_output_tool_call(tool_call_id, internal_call_id, args)
                .instrument(tracing::info_span!(
                    "tool.call",
                    tool_name = %tool_name,
                    tool_call_id = %internal_call_id,
                ))
                .await;

            return match result {
                Ok(action) => {
                    self.record_successful_tool_call_action(internal_call_id, action)
                        .await
                }
                Err(e) => self.on_tool_persistence_error("persist read_process tool call", &e),
            };
        }
        if tool_name == CANCEL_PROCESS_TOOL_NAME {
            let result = self
                .persist_cancel_tool_call(tool_call_id, internal_call_id, args)
                .instrument(tracing::info_span!(
                    "tool.call",
                    tool_name = %tool_name,
                    tool_call_id = %internal_call_id,
                ))
                .await;

            return match result {
                Ok(action) => {
                    self.record_successful_tool_call_action(internal_call_id, action)
                        .await
                }
                Err(e) => self.on_tool_persistence_error("persist cancel_process tool call", &e),
            };
        }

        let hold_required = self.approval_required_for(tool_name).await;
        let result: anyhow::Result<()> = async {
            let (session_id, request_id, deadline_at, seq) =
                self.ensure_assistant_turn_sequence().await?;
            self.state.lock().await.register_tool_result_identity(
                internal_call_id,
                None,
                tool_call_id.as_deref(),
            );

            let mut lc = crate::tool_call_lifecycle::ToolCallLifecycle::new(
                self.node.clone(),
                request_id,
                session_id,
                self.agent_did.clone(),
                internal_call_id.to_string(),
                seq,
                tool_name.to_string(),
                args.to_string(),
                deadline_at,
            )
            .with_requester_did(self.active_requester_did().await);
            if hold_required {
                lc.hold_for_approval().await?;
            } else {
                lc.start_running().await?;
            }

            self.in_flight_lifecycles
                .lock()
                .await
                .insert(internal_call_id.to_string(), lc);

            Ok(())
        }
        .instrument(tracing::info_span!(
            "tool.call",
            tool_name = %tool_name,
            tool_call_id = %internal_call_id,
        ))
        .await;

        match result {
            Ok(()) if hold_required => {
                self.record_success();
                match self
                    .drive_held_tool_call(tool_name, internal_call_id)
                    .instrument(tracing::info_span!(
                        "tool.approval",
                        tool_name = %tool_name,
                        tool_call_id = %internal_call_id,
                    ))
                    .await
                {
                    Ok(action) => action,
                    Err(e) => self.on_tool_persistence_error("await tool-call approval", &e),
                }
            }
            Ok(()) => {
                self.record_success();
                ToolCallHookAction::Continue
            }
            Err(e) => self.on_tool_persistence_error("persist tool call", &e),
        }
    }

    async fn persist_tool_result_action(
        &self,
        tool_name: &str,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
        outcome: &crate::tool_call_lifecycle::ToolOutcome,
    ) -> (HookAction, Option<String>) {
        use crate::tool_call_lifecycle::ToolOutcome;

        self.release_live_output(internal_call_id).await;
        let persist_result: anyhow::Result<(HookAction, Option<String>)> = async {
            // Managed terminals terminate the turn; they carry no model-facing
            // text and never thread back to the provider.
            if matches!(
                outcome,
                ToolOutcome::TimedOut { .. } | ToolOutcome::Cancelled
            ) {
                let lifecycle = self
                    .in_flight_lifecycles
                    .lock()
                    .await
                    .remove(internal_call_id);

                if let Some(mut lc) = lifecycle {
                    match outcome {
                        ToolOutcome::TimedOut { .. } => {
                            let _ = lc.timeout().await?;
                        }
                        _ => {
                            let _ = lc.cancel_during_run(CancelCause::Interrupted).await?;
                        }
                    }
                } else {
                    tracing::debug!(
                        tool_call_id = %internal_call_id,
                        outcome = ?outcome,
                        "managed terminal tool outcome arrived after lifecycle was already swept"
                    );
                }

                let reason = match outcome {
                    ToolOutcome::TimedOut { .. } => "tool call deadline exceeded",
                    _ => "tool call cancelled",
                };
                return Ok((
                    HookAction::Terminate {
                        reason: reason.to_string(),
                    },
                    None,
                ));
            }

            // The outcome arrives as data, so there is nothing to classify or
            // strip: the model-facing text is the only text there is.
            let result = outcome.model_facing_text();

            let (session_id, should_persist_message, persisted_result_id, persisted_call_id) = {
                let mut state = self.state.lock().await;
                let session_id = state
                    .session_id
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("session hook missing session id"))?;
                let should_persist_message = state.mark_tool_result_seen_for_persisted_turn(
                    internal_call_id,
                    None,
                    tool_call_id.as_deref(),
                );
                let (persisted_result_id, persisted_call_id) =
                    state.tool_result_message_identity(internal_call_id, tool_call_id.as_deref());
                (
                    session_id,
                    should_persist_message,
                    persisted_result_id,
                    persisted_call_id,
                )
            };

            let truncator =
                DefraSpillTruncator::new(self.node.clone(), &self.agent_did, &session_id)
                    .with_requester_did(self.active_requester_did().await)
                    .with_tool_call_id(internal_call_id);
            let result_for_persistence = result;
            let truncated = truncator
                .truncate(
                    tool_name,
                    args,
                    result_for_persistence,
                    truncation_mode_for(tool_name),
                    &self.truncation_limits,
                    None,
                )
                .await?;

            let mut lc = self
                .in_flight_lifecycles
                .lock()
                .await
                .remove(internal_call_id)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "on_tool_result: no in-flight lifecycle for tool_call_id={internal_call_id}"
                    )
                })?;

            let result_fact = truncated.spill_ref.as_ref().ok_or_else(|| {
                anyhow::anyhow!("full tool output was not retained as an exact signed fact")
            })?;

            let authoritative_projection = match outcome {
                ToolOutcome::Completed(_) => {
                    lc.complete_with_result_fact(&truncated.text, result_fact)
                        .await?
                }
                ToolOutcome::Failed { class, denial, .. } => {
                    if let Some(denial) = denial.as_ref() {
                        lc.fail_with_command_denial_and_result_fact(
                            &truncated.text,
                            denial,
                            result_fact,
                        )
                        .await?
                    } else {
                        lc.fail_with_result_fact(&truncated.text, *class, result_fact)
                            .await?
                    }
                }
                ToolOutcome::TimedOut { .. } | ToolOutcome::Cancelled => {
                    unreachable!("managed terminals returned above")
                }
            };

            if should_persist_message {
                let model_observation =
                    model_observation_for_tool_result(tool_name, &authoritative_projection);
                let tool_result_message = Message::User {
                    content: vec![UserContent::ToolResult(ToolResult {
                        id: persisted_result_id,
                        call_id: persisted_call_id,
                        content: vec![ToolResultContent::Text(Text {
                            text: model_observation,
                        })],
                    })],
                };
                self.persist_message(&tool_result_message).await?;
            }

            Ok((HookAction::Continue, Some(authoritative_projection)))
        }
        .instrument(tracing::info_span!(
            "tool.result",
            tool_name = %tool_name,
            tool_call_id = %internal_call_id,
        ))
        .await;

        match persist_result {
            Ok(action) => {
                self.record_success();
                action
            }
            Err(e) => (self.on_persistence_error("persist tool result", &e), None),
        }
    }

    /// Persist a tool result and return the ordinary hook action.
    ///
    /// Non-loop callers use this compatibility surface when they only need
    /// lifecycle side effects. The owned provider loop calls
    /// [`Self::on_tool_result_with_projection`] so it can thread the exact
    /// projection derived from the signed `AgentToolResult` fact.
    pub async fn on_tool_result(
        &self,
        tool_name: &str,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
        outcome: &crate::tool_call_lifecycle::ToolOutcome,
    ) -> HookAction {
        self.persist_tool_result_action(tool_name, tool_call_id, internal_call_id, args, outcome)
            .await
            .0
    }

    pub(crate) async fn on_tool_result_with_projection(
        &self,
        tool_name: &str,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
        outcome: &crate::tool_call_lifecycle::ToolOutcome,
    ) -> (HookAction, Option<String>) {
        self.persist_tool_result_action(tool_name, tool_call_id, internal_call_id, args, outcome)
            .await
    }
}
