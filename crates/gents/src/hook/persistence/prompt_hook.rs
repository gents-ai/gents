use super::*;
use crate::hook::{decide_persistence_outcome, FailurePolicy, HookError, PolicyDecision};

type Result<T, E = HookError> = std::result::Result<T, E>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HookOwnedTool {
    GetGoal,
    UpdateGoal,
    CreateGoal,
    AgentControl,
    SessionMessage,
    SpawnProcess,
    WaitProcess,
    ListProcesses,
    ReadProcess,
    CancelProcess,
}

impl HookOwnedTool {
    pub(crate) const ROUTES: [(&'static str, Self); 14] = [
        (crate::goal::GET_GOAL_TOOL_NAME, Self::GetGoal),
        (crate::goal::UPDATE_GOAL_TOOL_NAME, Self::UpdateGoal),
        (crate::goal::CREATE_GOAL_TOOL_NAME, Self::CreateGoal),
        (
            crate::toolset::AGENT_INTERRUPT_TOOL_NAME,
            Self::AgentControl,
        ),
        (crate::toolset::AGENT_LIST_TOOL_NAME, Self::AgentControl),
        (
            crate::toolset::AGENT_SESSION_READ_TOOL_NAME,
            Self::AgentControl,
        ),
        (
            crate::toolset::AGENT_SESSION_LIST_TOOL_NAME,
            Self::AgentControl,
        ),
        (AGENT_NEW_TOOL_NAME, Self::SessionMessage),
        (
            crate::toolset::AGENT_MESSAGE_TOOL_NAME,
            Self::SessionMessage,
        ),
        (SPAWN_PROCESS_TOOL_NAME, Self::SpawnProcess),
        (WAIT_PROCESS_TOOL_NAME, Self::WaitProcess),
        (LIST_PROCESSES_TOOL_NAME, Self::ListProcesses),
        (READ_PROCESS_TOOL_NAME, Self::ReadProcess),
        (CANCEL_PROCESS_TOOL_NAME, Self::CancelProcess),
    ];

    pub(crate) fn of(tool_name: &str) -> Option<Self> {
        Self::ROUTES
            .iter()
            .find(|(name, _)| *name == tool_name)
            .map(|(_, route)| *route)
    }

    fn persistence_context(self) -> &'static str {
        match self {
            Self::GetGoal => "persist get_goal tool call",
            Self::UpdateGoal => "persist update_goal tool call",
            Self::CreateGoal => "persist create_goal tool call",
            Self::AgentControl => "persist agents tool call",
            Self::SessionMessage => "persist session-message tool call",
            Self::SpawnProcess => "persist spawn_process tool call",
            Self::WaitProcess => "persist wait_process tool call",
            Self::ListProcesses => "persist list_processes tool call",
            Self::ReadProcess => "persist read_process tool call",
            Self::CancelProcess => "persist cancel_process tool call",
        }
    }
}

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
        let owned = HookOwnedTool::of(tool_name).filter(|route| match route {
            HookOwnedTool::GetGoal | HookOwnedTool::UpdateGoal => self.goal_tools_enabled,
            HookOwnedTool::CreateGoal => self.goal_creation_enabled,
            _ => true,
        });
        if let Some(route) = owned {
            let result = async {
                match route {
                    HookOwnedTool::GetGoal => {
                        self.persist_get_goal_tool_call(tool_call_id, internal_call_id, args)
                            .await
                    }
                    HookOwnedTool::UpdateGoal => {
                        self.persist_update_goal_tool_call(tool_call_id, internal_call_id, args)
                            .await
                    }
                    HookOwnedTool::CreateGoal => {
                        self.persist_create_goal_tool_call(tool_call_id, internal_call_id, args)
                            .await
                    }
                    HookOwnedTool::AgentControl => {
                        self.persist_agent_control_tool_call(
                            tool_name,
                            tool_call_id,
                            internal_call_id,
                            args,
                        )
                        .await
                    }
                    HookOwnedTool::SessionMessage => {
                        self.persist_session_message_tool_call(
                            tool_name,
                            tool_call_id,
                            internal_call_id,
                            args,
                        )
                        .await
                    }
                    HookOwnedTool::SpawnProcess => {
                        self.persist_background_tool_call(tool_call_id, internal_call_id, args)
                            .await
                    }
                    HookOwnedTool::WaitProcess => {
                        self.persist_wait_tool_call(tool_call_id, internal_call_id, args)
                            .await
                    }
                    HookOwnedTool::ListProcesses => {
                        self.persist_list_background_tools_tool_call(
                            tool_call_id,
                            internal_call_id,
                            args,
                        )
                        .await
                    }
                    HookOwnedTool::ReadProcess => {
                        self.persist_read_tool_output_tool_call(
                            tool_call_id,
                            internal_call_id,
                            args,
                        )
                        .await
                    }
                    HookOwnedTool::CancelProcess => {
                        self.persist_cancel_tool_call(tool_call_id, internal_call_id, args)
                            .await
                    }
                }
            }
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
                    self.on_tool_persistence_error(route.persistence_context(), &error)
                }
            };
        }

        if let Some(binding) = self.job_tools.get(tool_name) {
            let result = self
                .persist_job_tool_call(binding, tool_call_id, internal_call_id, args)
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
                    self.on_tool_persistence_error("persist job tool call", &error)
                }
            };
        }

        let result: Result<(), HookError> = async {
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

    /// Dispatch a job tool: step 0 runs in the foreground, and the call ends
    /// here unless its output adopts a job.
    async fn persist_job_tool_call(
        &self,
        binding: &crate::plugin_job::JobBinding,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
    ) -> Result<ToolCallHookAction> {
        let tool_name = binding.tool_name.as_str();
        let (session_id, request_id, deadline_at, _seq) =
            self.ensure_assistant_turn_sequence().await?;
        let mut lifecycle = self
            .adopt_accepted_tool_dispatch(
                internal_call_id,
                tool_call_id.as_deref(),
                &request_id,
                &session_id,
                tool_name,
                args,
                deadline_at,
                AwaitMode::Foreground,
            )
            .await?;
        lifecycle.start_running().await?;
        let payload = match serde_json::from_str(args) {
            Ok(args) => {
                crate::plugin_job::dispatch::run_step0(self, &mut lifecycle, binding, args).await?
            }
            Err(error) => {
                let payload = invalid_tool_arguments_payload(
                    tool_name,
                    "/",
                    format!("invalid {tool_name} arguments: {error}"),
                );
                lifecycle
                    .fail(&payload, FailureClass::ArgumentInvalid)
                    .await?;
                payload
            }
        };
        Ok(self.skip_tool_result(tool_name, payload))
    }

    pub async fn on_tool_admission_rejected(
        &self,
        tool_name: &str,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
        outcome: &crate::tool_call_lifecycle::ToolOutcome,
    ) -> HookAction {
        let result: Result<(), HookError> = async {
            let crate::tool_call_lifecycle::ToolOutcome::Failed {
                class,
                denial,
                text,
            } = outcome
            else {
                return Err(HookError::AdmissionOutcomeNotFailed);
            };
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
                )
                .await?;
            match denial {
                Some(denial) => Ok(lc.spawn_failed_with_command_denial(text, denial).await?),
                None => Ok(lc.spawn_failed(*class, text).await?),
            }
        }
        .instrument(tracing::info_span!(
            "tool.admission_rejected",
            tool_name = %tool_name,
            tool_call_id = %internal_call_id,
        ))
        .await;

        match result {
            Ok(()) => {
                self.record_success();
                HookAction::Continue
            }
            Err(e) => self.on_persistence_error("persist tool admission rejection", &e),
        }
    }

    pub async fn on_tool_result(
        &self,
        tool_name: &str,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        _args: &str,
        outcome: &crate::tool_call_lifecycle::ToolOutcome,
    ) -> HookAction {
        use crate::tool_call_lifecycle::ToolOutcome;

        let is_plugin_effect = self
            .effect_lifecycles
            .lock()
            .await
            .contains_key(internal_call_id);
        let persist_result: Result<HookAction, HookError> = async {
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
                    let receipt = if let Some(doc_id) = lc.doc_id() {
                        self.background_live_outputs
                            .registry
                            .plugin_receipt(doc_id)
                            .await
                    } else {
                        None
                    };
                    lc.stage_plugin_receipt(receipt);
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

                if is_plugin_effect {
                    return Ok(HookAction::Continue);
                }
                let reason = match outcome {
                    ToolOutcome::TimedOut { .. } => "tool call deadline exceeded",
                    _ => "tool call cancelled",
                };
                return Ok(HookAction::Terminate {
                    reason: reason.to_string(),
                });
            }

            let result = outcome.model_facing_text();

            let tool_call_doc_id = {
                let lifecycles = self.in_flight_lifecycles.lock().await;
                lifecycles
                    .get(internal_call_id)
                    .and_then(|lifecycle| lifecycle.doc_id())
                    .map(str::to_string)
                    .ok_or_else(|| HookError::ToolResultDocIdMissing {
                        internal_call_id: internal_call_id.to_owned(),
                    })?
            };

            {
                let mut state = self.state.lock().await;
                state.register_tool_result_identity(
                    internal_call_id,
                    None,
                    tool_call_id.as_deref(),
                );
                if state.session_id.is_none() {
                    return Err(HookError::MissingSessionId);
                }
            }

            let mut lc = self
                .in_flight_lifecycles
                .lock()
                .await
                .remove(internal_call_id)
                .ok_or_else(|| HookError::ToolResultLifecycleMissing {
                    internal_call_id: internal_call_id.to_owned(),
                })?;

            lc.stage_plugin_receipt(
                self.background_live_outputs
                    .registry
                    .plugin_receipt(&tool_call_doc_id)
                    .await,
            );
            lc.stage_deferred_writes(
                self.background_live_outputs
                    .registry
                    .deferred_writes(&tool_call_doc_id)
                    .await,
            );

            let presentation = self
                .background_live_outputs
                .registry
                .take_prepared_presentation(&tool_call_doc_id, result)
                .await?;

            let bounded = presentation
                .is_none()
                .then(|| {
                    bounded_tool_result_with_presentation(
                        tool_name,
                        result,
                        &self.truncation_limits,
                    )
                })
                .and_then(|(bounded, presentation)| Some((bounded, presentation?)));
            match (outcome, bounded) {
                (
                    ToolOutcome::Failed {
                        denial: Some(denial),
                        ..
                    },
                    _,
                ) => {
                    lc.fail_with_command_denial(result, denial).await?;
                }
                (ToolOutcome::Completed(_), Some((bounded, presentation))) => {
                    lc.complete_raw_with_presentation(result, &bounded, presentation)
                        .await?;
                }
                (ToolOutcome::Failed { class, .. }, Some((bounded, presentation))) => {
                    lc.fail_raw_with_presentation(result, &bounded, *class, presentation)
                        .await?;
                }
                (ToolOutcome::Completed(_), None) => {
                    lc.complete_with_presentation(result, presentation).await?
                }
                (ToolOutcome::Failed { class, .. }, None) => {
                    lc.fail_with_presentation(result, *class, presentation)
                        .await?;
                }
                (ToolOutcome::TimedOut { .. } | ToolOutcome::Cancelled, _) => {
                    unreachable!("managed terminals returned above")
                }
            }

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
            Err(e) => {
                if is_plugin_effect {
                    match decide_persistence_outcome(
                        FailurePolicy::FailClosed,
                        &self.counters,
                        "persist plugin effect",
                        &e,
                    ) {
                        PolicyDecision::Terminate(reason) => HookAction::Terminate { reason },
                        PolicyDecision::Continue => {
                            unreachable!("plugin effects require durable results")
                        }
                    }
                } else {
                    self.on_persistence_error("persist tool result", &e)
                }
            }
        }
    }
}
