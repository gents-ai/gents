use rig::agent::{HookAction, PromptHook, ToolCallHookAction};
use rig::completion::message::{Message, Text, ToolResult, ToolResultContent, UserContent};
use rig::completion::{CompletionModel, CompletionResponse};
use rig::one_or_many::OneOrMany;
use tracing::Instrument;

use crate::session;
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
                    state.assistant_turn_sequence = None;
                    state.assistant_turn_saved = false;
                    (session_id, sequence, "user")
                }
                Message::Assistant { .. } => {
                    let sequence = match state.assistant_turn_sequence {
                        Some(sequence) if !state.assistant_turn_saved => sequence,
                        Some(sequence) => {
                            anyhow::bail!(
                                "assistant turn for sequence {} already persisted",
                                sequence
                            );
                        }
                        None => {
                            state.sequence += 1;
                            let sequence = state.sequence;
                            state.assistant_turn_sequence = Some(sequence);
                            sequence
                        }
                    };
                    state.assistant_turn_saved = true;
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
    ) -> anyhow::Result<()> {
        let session_id = self
            .state
            .lock()
            .await
            .session_id
            .clone()
            .ok_or_else(|| anyhow::anyhow!("session hook missing session id"))?;

        let tool_call_id = &tool_result.id;
        let stored_result = session::load_tool_call_result(&self.node, &session_id, tool_call_id)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(
                    error = %e,
                    tool_call_id = %tool_call_id,
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

    async fn ensure_assistant_turn_sequence(&self) -> anyhow::Result<(String, u32)> {
        let mut state = self.state.lock().await;
        let session_id = state
            .session_id
            .clone()
            .ok_or_else(|| anyhow::anyhow!("session hook missing session id"))?;

        let sequence = match state.assistant_turn_sequence {
            Some(sequence) => sequence,
            None => {
                state.sequence += 1;
                let sequence = state.sequence;
                state.assistant_turn_sequence = Some(sequence);
                state.assistant_turn_saved = false;
                sequence
            }
        };

        Ok((session_id, sequence))
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

            state.assistant_turn_sequence = None;
            state.assistant_turn_saved = false;
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
        _tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
    ) -> ToolCallHookAction {
        let result: anyhow::Result<()> = async {
            let (session_id, seq) = self.ensure_assistant_turn_sequence().await?;
            let storage_id = internal_call_id.to_string();
            session::save_tool_call(
                &self.node,
                &session_id,
                seq,
                tool_name,
                &storage_id,
                args,
                "called",
            )
            .await?;

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
        let persist_result: anyhow::Result<()> = async {
            let (session_id, should_persist_message) = {
                let state = self.state.lock().await;
                let session_id = state
                    .session_id
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("session hook missing session id"))?;
                (session_id, state.assistant_turn_saved)
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

            let storage_id = internal_call_id.to_string();
            session::complete_tool_call(
                &self.node,
                &session_id,
                &storage_id,
                &truncated.text,
                "completed",
            )
            .await?;

            if should_persist_message {
                let tool_result_message = Message::User {
                    content: OneOrMany::one(UserContent::ToolResult(ToolResult {
                        id: internal_call_id.to_string(),
                        call_id: tool_call_id,
                        content: OneOrMany::one(ToolResultContent::Text(Text {
                            text: truncated.text.clone(),
                        })),
                    })),
                };
                self.persist_message(&tool_result_message).await?;
            }

            Ok(())
        }
        .instrument(tracing::info_span!(
            "tool.result",
            tool_name = %tool_name,
            tool_call_id = %internal_call_id,
        ))
        .await;

        match persist_result {
            Ok(()) => {
                self.record_success();
                HookAction::Continue
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
