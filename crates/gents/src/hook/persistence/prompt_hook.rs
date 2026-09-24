use super::*;

impl DefraSessionHook {
    pub async fn on_completion_call(&self, prompt: &Message, _history: &[Message]) -> HookAction {
        self.on_completion_call_with_context(prompt, _history, None)
            .await
    }

    pub async fn on_completion_call_with_context(
        &self,
        _prompt: &Message,
        _history: &[Message],
        _context: Option<&Message>,
    ) -> HookAction {
        // StreamProcessor owns publication through the active request lease.
        // The hook has no authority to create transcript rows at admission.
        self.state.lock().await.reset_after_user_message();
        self.record_success();
        HookAction::Continue
    }

    pub async fn on_tool_call(
        &self,
        tool_name: &str,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
    ) -> ToolCallHookAction {
        if self.goal_tools_enabled && tool_name == crate::goal::GET_GOAL_TOOL_NAME {
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
                    self.record_success();
                    action
                }
                Err(error) => self.on_tool_persistence_error("persist get_goal tool call", &error),
            };
        }
        if self.goal_tools_enabled && tool_name == crate::goal::UPDATE_GOAL_TOOL_NAME {
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
                    self.record_success();
                    action
                }
                Err(error) => {
                    self.on_tool_persistence_error("persist update_goal tool call", &error)
                }
            };
        }
        if self.goal_creation_enabled && tool_name == crate::goal::CREATE_GOAL_TOOL_NAME {
            let result = self
                .persist_create_goal_tool_call(tool_call_id, internal_call_id, args)
                .instrument(tracing::info_span!(
                    "tool.call",
                    tool_name = %tool_name,
                    tool_call_id = %internal_call_id,
                ))
                .await;
            return match result {
                Ok(action) => {
                    self.record_success();
                    action
                }
                Err(error) => {
                    self.on_tool_persistence_error("persist create_goal tool call", &error)
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
                    self.record_success();
                    action
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
                    self.record_success();
                    action
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
                    self.record_success();
                    action
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
                    self.record_success();
                    action
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
                    self.record_success();
                    action
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
                    self.record_success();
                    action
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
                    self.record_success();
                    action
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
                    self.record_success();
                    action
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
                    self.record_success();
                    action
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
                    self.record_success();
                    action
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
                    self.record_success();
                    action
                }
                Err(e) => self.on_tool_persistence_error("persist cancel_process tool call", &e),
            };
        }

        let result: anyhow::Result<()> = async {
            let (session_id, request_id, deadline_at, _seq) =
                self.ensure_assistant_turn_sequence().await?;
            let mut lc = self
                .adopt_accepted_tool_dispatch(
                    internal_call_id,
                    tool_call_id.as_deref(),
                    &request_id,
                    &session_id,
                    tool_name,
                    args,
                    deadline_at,
                    crate::tool_call_lifecycle::AwaitMode::Foreground,
                    crate::tool_call_lifecycle::CancelPolicy::Cascade,
                )
                .await?;
            lc.start_running().await?;

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
            Ok(()) => {
                self.record_success();
                ToolCallHookAction::Continue
            }
            Err(e) => self.on_tool_persistence_error("persist tool call", &e),
        }
    }

    pub async fn on_tool_result(
        &self,
        tool_name: &str,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
        outcome: &crate::tool_call_lifecycle::ToolOutcome,
    ) -> HookAction {
        use crate::tool_call_lifecycle::ToolOutcome;

        let persist_result: anyhow::Result<HookAction> = async {
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
                    let output_doc_id = lc.doc_id().map(str::to_owned);
                    match outcome {
                        ToolOutcome::TimedOut { .. } => {
                            let _ = lc.timeout().await?;
                        }
                        _ => {
                            let _ = lc.cancel_during_run(CancelCause::Interrupted).await?;
                        }
                    }
                    if let Some(output_doc_id) = output_doc_id {
                        self.release_live_output(&output_doc_id).await;
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
                return Ok(HookAction::Terminate {
                    reason: reason.to_string(),
                });
            }

            // The outcome arrives as data, so there is nothing to classify or
            // strip: the model-facing text is the only text there is.
            let result = outcome.model_facing_text();

            // Background subagent dispatch published its immediate receipt
            // under a separate authored source. Rig still reports the Skip as
            // a completed tool outcome; acknowledging it must not close the
            // running bridge or manufacture a second ToolResult.
            if self
                .in_flight_lifecycles
                .lock()
                .await
                .get(internal_call_id)
                .is_some_and(|lifecycle| {
                    lifecycle.is_subagent_bridge()
                        && lifecycle.await_mode() == crate::tool_call_lifecycle::AwaitMode::Background
                })
            {
                return Ok(HookAction::Continue);
            }

            let tool_call_doc_id = {
                let lifecycles = self.in_flight_lifecycles.lock().await;
                lifecycles
                    .get(internal_call_id)
                    .and_then(|lifecycle| lifecycle.doc_id())
                    .map(str::to_string)
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "on_tool_result: persisted AgentToolCall _docID missing for tool_call_id={internal_call_id}"
                        )
                    })?
            };

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

            // Canonical segments retain the full result exactly once.  Provider
            // narrowing is represented by the delivery header's presentation,
            // never by a spill row or a second truncated payload copy.
            let _ = (&session_id, &tool_call_doc_id, args);

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

            let presentation = self.background_live_outputs.registry
                .take_prepared_presentation(&tool_call_doc_id, result)
                .await?;

            match outcome {
                ToolOutcome::Completed(_) => lc.complete_with_presentation(result, presentation).await?,
                ToolOutcome::Failed { class, denial, .. } => {
                    if let Some(denial) = denial.as_ref() {
                        lc.fail_with_command_denial(result, denial).await?;
                    } else {
                        lc.fail_with_presentation(result, *class, presentation).await?;
                    }
                }
                ToolOutcome::TimedOut { .. } | ToolOutcome::Cancelled => {
                    unreachable!("managed terminals returned above")
                }
            }

            // The lifecycle terminal transition already published the sole
            // canonical ToolResult header with exact physical identity.  Do
            // not append the retired serialized-message projection again.
            let _ = (
                should_persist_message,
                persisted_result_id,
                persisted_call_id,
                model_observation_for_tool_result(tool_name, result),
            );

            self.release_live_output(&tool_call_doc_id).await;

            Ok(HookAction::Continue)
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
            Err(e) => self.on_persistence_error("persist tool result", &e),
        }
    }
}
