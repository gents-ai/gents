use rig::agent::{HookAction, PromptHook, ToolCallHookAction};
use rig::completion::message::{Message, Text, ToolResult, ToolResultContent, UserContent};
use rig::completion::{CompletionModel, CompletionResponse};
use rig::one_or_many::OneOrMany;
use tracing::Instrument;

use crate::config::DEFAULT_DEADLINE_DURATION_SECS;
use crate::session;
use crate::tool_call_lifecycle::runtime::{classify_managed_tool_result, ManagedToolTerminal};
use crate::truncation::{truncate_text, DefraSpillTruncator, TruncationMode, Truncator};

use super::DefraSessionHook;

impl DefraSessionHook {
    pub async fn persist_message(&self, message: &Message) -> anyhow::Result<u32> {
        let (session_id, sequence, role) = {
            let mut state = self.state.lock().await;
            let session_id = state
                .session_id
                .clone()
                .ok_or_else(|| anyhow::anyhow!("session hook missing session id"))?;

            match message {
                Message::User { .. } => {
                    state.sequence += 1;
                    let sequence = state.sequence;
                    state.reset_after_user_message();
                    (session_id, sequence, "user")
                }
                Message::Assistant { .. } => {
                    let sequence = state.persist_assistant_turn()?;
                    (session_id, sequence, "assistant")
                }
                Message::System { .. } => {
                    anyhow::bail!("system messages are not persisted in session history");
                }
            }
        };

        let content = serde_json::to_string(message)?;
        session::save_message(&self.node, &session_id, sequence, role, &content).await?;
        Ok(sequence)
    }

    pub async fn persist_stream_tool_result_message(
        &self,
        tool_result: &ToolResult,
        internal_call_id: &str,
    ) -> anyhow::Result<()> {
        let session_id = self
            .state
            .lock()
            .await
            .session_id
            .clone()
            .ok_or_else(|| anyhow::anyhow!("session hook missing session id"))?;

        let should_persist = {
            let mut state = self.state.lock().await;
            state.mark_stream_tool_result_seen(
                internal_call_id,
                &tool_result.id,
                tool_result.call_id.as_deref(),
            )?
        };
        if !should_persist {
            return Ok(());
        }

        let stored_result =
            session::load_tool_call_result(&self.node, &session_id, internal_call_id)
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(
                        error = %e,
                        tool_call_id = %internal_call_id,
                        "failed to load stored tool result, falling back to stream payload"
                    );
                    let raw = render_tool_result_text(tool_result);
                    let (text, _, _) =
                        truncate_text(&raw, TruncationMode::Head, &self.truncation_limits);
                    text
                });

        let persisted_result = ToolResult {
            id: tool_result.id.clone(),
            call_id: tool_result.call_id.clone(),
            content: OneOrMany::one(ToolResultContent::Text(Text {
                text: stored_result,
            })),
        };

        let message = Message::User {
            content: OneOrMany::one(UserContent::ToolResult(persisted_result)),
        };
        self.persist_message(&message).await?;
        Ok(())
    }

    async fn ensure_assistant_turn_sequence(
        &self,
    ) -> anyhow::Result<(String, String, chrono::DateTime<chrono::Utc>, u32)> {
        let mut state = self.state.lock().await;
        let session_id = state
            .session_id
            .clone()
            .ok_or_else(|| anyhow::anyhow!("session hook missing session id"))?;
        let request_id = state.current_request_id.clone().unwrap_or_else(|| {
            tracing::warn!(
                "tool call has no active request id; persisting with empty request link"
            );
            String::new()
        });
        let deadline_at = state.request_deadline_at.unwrap_or_else(|| {
            tracing::warn!(
                "tool call has no active request deadline; using default lifecycle deadline"
            );
            chrono::Utc::now() + chrono::Duration::seconds(DEFAULT_DEADLINE_DURATION_SECS as i64)
        });

        let sequence = state.begin_or_continue_assistant_turn();

        Ok((session_id, request_id, deadline_at, sequence))
    }
}

impl<M: CompletionModel> PromptHook<M> for DefraSessionHook {
    async fn on_completion_call(&self, prompt: &Message, _history: &[Message]) -> HookAction {
        let result: anyhow::Result<()> = async {
            let mut state = self.state.lock().await;

            if !state.initialized {
                let session_id = session::create_session(&self.node, &state.agent_name).await?;
                state.session_id = Some(session_id);
                state.initialized = true;
            }

            state.reset_after_user_message();
            drop(state);

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

    async fn on_completion_response(
        &self,
        _prompt: &Message,
        response: &CompletionResponse<M::Response>,
    ) -> HookAction {
        let result: anyhow::Result<()> = async {
            let message = Message::Assistant {
                id: response.message_id.clone(),
                content: response.choice.clone(),
            };
            let sequence = self.persist_message(&message).await?;
            self.mark_current_response_materialized(sequence).await?;
            Ok(())
        }
        .await;

        match result {
            Ok(()) => {
                self.record_success();
                HookAction::Continue
            }
            Err(e) => self.on_persistence_error("persist assistant response", &e),
        }
    }

    async fn on_tool_call(
        &self,
        tool_name: &str,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
    ) -> ToolCallHookAction {
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
                internal_call_id.to_string(),
                seq,
                tool_name.to_string(),
                args.to_string(),
                deadline_at,
            );
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

    async fn on_tool_result(
        &self,
        tool_name: &str,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
        result: &str,
    ) -> HookAction {
        let persist_result: anyhow::Result<HookAction> = async {
            if let Some(terminal) = classify_managed_tool_result(result) {
                let lifecycle = self
                    .in_flight_lifecycles
                    .lock()
                    .await
                    .remove(internal_call_id);

                if let Some(mut lc) = lifecycle {
                    match terminal {
                        ManagedToolTerminal::TimedOut => lc.timeout().await?,
                        ManagedToolTerminal::Cancelled => lc.cancel_during_run().await?,
                    }
                } else {
                    tracing::debug!(
                        tool_call_id = %internal_call_id,
                        lifecycle_state = ?terminal,
                        "managed terminal tool result arrived after lifecycle was already swept"
                    );
                }

                let reason = match terminal {
                    ManagedToolTerminal::TimedOut => "tool call deadline exceeded",
                    ManagedToolTerminal::Cancelled => "tool call cancelled",
                };
                return Ok(HookAction::Terminate {
                    reason: reason.to_string(),
                });
            }

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
                DefraSpillTruncator::new(self.node.clone(), &self.agent_did, &session_id);
            let truncated = truncator
                .truncate(
                    tool_name,
                    args,
                    result,
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

            if let Some(failure_class) = classify_runtime_failure(result) {
                lc.fail(&truncated.text, failure_class).await?;
            } else {
                lc.complete(&truncated.text).await?;
            }

            if should_persist_message {
                let tool_result_message = Message::User {
                    content: OneOrMany::one(UserContent::ToolResult(ToolResult {
                        id: persisted_result_id,
                        call_id: persisted_call_id,
                        content: OneOrMany::one(ToolResultContent::Text(Text {
                            text: truncated.text.clone(),
                        })),
                    })),
                };
                self.persist_message(&tool_result_message).await?;
            }

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

fn truncation_mode_for(tool_name: &str) -> TruncationMode {
    match tool_name {
        "bash" | "shell" | "command" => TruncationMode::Tail,
        _ => TruncationMode::Head,
    }
}

fn render_tool_result_text(tool_result: &ToolResult) -> String {
    tool_result
        .content
        .iter()
        .filter_map(|content| match content {
            ToolResultContent::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Classify a runtime error string into a FailureClass. Defaults to
/// ToolReturnedError for unknown shapes; managed timeout/cancel markers are
/// handled before this helper so terminal outcomes stay distinct.
#[allow(dead_code)]
fn classify_runtime_error(err: &str) -> crate::tool_call_lifecycle::FailureClass {
    use crate::tool_call_lifecycle::FailureClass;
    if err.contains("timeout") || err.contains("deadline") {
        FailureClass::External // R3 will reroute to lifecycle.timeout()
    } else if err.contains("invalid argument") || err.contains("parse") {
        FailureClass::ArgumentInvalid
    } else if err.contains("unavailable") || err.contains("not found") {
        FailureClass::ServiceUnavailable
    } else if err.contains("transport") || err.contains("connection") {
        FailureClass::Transport
    } else {
        FailureClass::ToolReturnedError
    }
}

fn classify_runtime_failure(result: &str) -> Option<crate::tool_call_lifecycle::FailureClass> {
    if result.starts_with("JsonError:") {
        return Some(crate::tool_call_lifecycle::FailureClass::ArgumentInvalid);
    }
    if result.starts_with("ToolCallError:") {
        return Some(classify_runtime_error(result));
    }
    None
}
