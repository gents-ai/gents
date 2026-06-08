//! Owned multi-turn completion→tool loop (issue #400, decision D6).
//!
//! This replaces rig's `Agent::stream_prompt` *producer* with our own stream
//! generator, while keeping rig as the provider/streaming *client*
//! (`CompletionModel::stream`, the `Message` family, and the streaming decode
//! types). The generator mirrors rig's `agent::prompt_request::streaming::send`:
//! build a request from the running message history, stream one completion,
//! accumulate assistant content, and — when the turn produced tool calls —
//! execute them, thread their results back into the history, and loop. When a
//! turn produces no tool calls, it yields a terminal [`LoopFinalResponse`].
//!
//! The yielded [`LoopStreamItem`] mirrors rig's `MultiTurnStreamItem` so the
//! existing `StreamProcessor` consumer (and the `inference.rs` lifecycle
//! envelope around it) retarget to it mechanically.

use async_stream::try_stream;
use futures::{Stream, StreamExt};
use rig::agent::StreamingError;
use rig::completion::message::ToolCall;
use rig::completion::{CompletionError, CompletionModel, CompletionRequest, GetTokenUsage, Message, Usage};
use rig::message::ToolChoice;
use rig::streaming::{StreamedAssistantContent, StreamedUserContent};
use rig::tool::ToolDyn;

#[cfg(test)]
mod tests;

/// An item produced by the owned loop stream.
///
/// Variant-for-variant mirror of rig's `MultiTurnStreamItem<R>` so consumers
/// that previously matched on the rig type port across by name only.
pub(crate) enum LoopStreamItem<R> {
    /// Streamed assistant content for the current turn (text, reasoning, or a
    /// tool call), forwarded verbatim to the consumer.
    StreamAssistantItem(StreamedAssistantContent<R>),
    /// A tool result produced by our own in-loop tool execution, threaded back
    /// into the conversation for the next turn.
    StreamUserItem(StreamedUserContent),
    /// The terminal response: the loop ended on a turn with no tool calls.
    FinalResponse(LoopFinalResponse),
}

/// Terminal payload of a completed loop: the concatenated final-turn assistant
/// text plus the usage aggregated across every turn.
pub(crate) struct LoopFinalResponse {
    response: String,
    usage: Usage,
}

impl LoopFinalResponse {
    pub(crate) fn response(&self) -> &str {
        &self.response
    }

    pub(crate) fn usage(&self) -> Usage {
        self.usage
    }
}

/// Per-request configuration for the loop, mirroring the agent-builder knobs we
/// previously handed to rig (`completion_factory::configure_agent_builder`).
pub(crate) struct LoopConfig {
    pub(crate) preamble: Option<String>,
    pub(crate) temperature: Option<f64>,
    pub(crate) max_tokens: Option<u64>,
    pub(crate) additional_params: Option<serde_json::Value>,
    pub(crate) tool_choice: Option<ToolChoice>,
    /// Maximum number of tool round-trips before the loop fails with a
    /// max-turns error. Matches rig's `default_max_turns` semantics: a turn
    /// that produces a text response (no tool calls) always gets to run.
    pub(crate) max_turns: usize,
}

/// Drive the owned multi-turn loop, producing a stream of [`LoopStreamItem`]s.
///
/// `prompt` is the new user message; `history` is the prior conversation
/// (without the new prompt). `tools` are dispatched by name when the model
/// calls them.
pub(crate) fn run_loop_stream<M>(
    model: M,
    prompt: Message,
    history: Vec<Message>,
    tools: Vec<Box<dyn ToolDyn>>,
    config: LoopConfig,
) -> impl Stream<Item = Result<LoopStreamItem<M::StreamingResponse>, StreamingError>>
where
    M: CompletionModel + 'static,
    M::StreamingResponse: 'static,
{
    try_stream! {
        // The running set of messages produced this request. The last element
        // is always the "prompt" for the next turn (rig semantics): initially
        // the user message, later the trailing tool-result user message.
        let mut new_messages: Vec<Message> = vec![prompt];
        let mut aggregated_usage = Usage::new();
        let mut current_turn: usize = 0;

        loop {
            if current_turn > config.max_turns {
                Err(StreamingError::Completion(CompletionError::ResponseError(format!(
                    "owned loop exceeded max turns ({})",
                    config.max_turns
                ))))?;
            }
            current_turn += 1;

            let current_prompt = new_messages
                .last()
                .cloned()
                .expect("new_messages always retains at least the initial prompt");
            let prior = &new_messages[..new_messages.len() - 1];
            let request = build_request(&model, current_prompt, &history, prior, &tools, &config).await?;

            let mut stream = model
                .stream(request)
                .await
                .map_err(StreamingError::Completion)?;

            let mut turn_text = String::new();
            let mut tool_calls: Vec<(ToolCall, String)> = Vec::new();

            while let Some(item) = stream.next().await {
                match item.map_err(StreamingError::Completion)? {
                    StreamedAssistantContent::Text(text) => {
                        turn_text.push_str(&text.text);
                        yield LoopStreamItem::StreamAssistantItem(StreamedAssistantContent::Text(text));
                    }
                    StreamedAssistantContent::Reasoning(reasoning) => {
                        yield LoopStreamItem::StreamAssistantItem(StreamedAssistantContent::Reasoning(reasoning));
                    }
                    StreamedAssistantContent::ReasoningDelta { id, reasoning } => {
                        yield LoopStreamItem::StreamAssistantItem(StreamedAssistantContent::ReasoningDelta { id, reasoning });
                    }
                    StreamedAssistantContent::ToolCall { tool_call, internal_call_id } => {
                        tool_calls.push((tool_call.clone(), internal_call_id.clone()));
                        yield LoopStreamItem::StreamAssistantItem(StreamedAssistantContent::ToolCall { tool_call, internal_call_id });
                    }
                    StreamedAssistantContent::ToolCallDelta { .. } => {
                        // Informational only; the full `ToolCall` is emitted
                        // separately and is what we accumulate.
                    }
                    StreamedAssistantContent::Final(raw) => {
                        if let Some(usage) = raw.token_usage() {
                            aggregated_usage += usage;
                        }
                    }
                }
            }

            if tool_calls.is_empty() {
                yield LoopStreamItem::FinalResponse(LoopFinalResponse {
                    response: turn_text,
                    usage: aggregated_usage,
                });
                break;
            }

            // Tool execution + result threading lands in the next increment.
            let _ = &tool_calls;
            Err(StreamingError::Completion(CompletionError::ResponseError(
                "owned loop tool execution not yet implemented".to_string(),
            )))?;
        }
    }
}

/// Build the per-turn [`CompletionRequest`], replicating rig's
/// `agent::completion::build_completion_request` for the non-RAG path:
/// the preamble becomes a leading `Message::System`, followed by the prior
/// conversation, with `prompt` appended last by `completion_request`.
async fn build_request<M: CompletionModel>(
    model: &M,
    prompt: Message,
    history: &[Message],
    prior: &[Message],
    tools: &[Box<dyn ToolDyn>],
    config: &LoopConfig,
) -> Result<CompletionRequest, StreamingError> {
    let mut tool_defs = Vec::with_capacity(tools.len());
    for tool in tools {
        tool_defs.push(tool.definition(String::new()).await);
    }

    let chat_history: Vec<Message> = config
        .preamble
        .as_ref()
        .map(|preamble| Message::system(preamble.clone()))
        .into_iter()
        .chain(history.iter().cloned())
        .chain(prior.iter().cloned())
        .collect();

    let mut builder = model
        .completion_request(prompt)
        .messages(chat_history)
        .temperature_opt(config.temperature)
        .max_tokens_opt(config.max_tokens)
        .additional_params_opt(config.additional_params.clone())
        .tools(tool_defs);

    if let Some(tool_choice) = &config.tool_choice {
        builder = builder.tool_choice(tool_choice.clone());
    }

    Ok(builder.build())
}
