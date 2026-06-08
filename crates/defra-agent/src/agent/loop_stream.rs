//! Owned multi-turn completion→tool loop (issue #400, decision D6).
//!
//! This replaces rig's `Agent::stream_prompt` *producer* with our own stream
//! generator, while keeping rig as the provider/streaming *client*
//! (`CompletionModel::stream`, the `Message` family, and the streaming decode
//! types). The generator mirrors rig's `agent::prompt_request::streaming::send`:
//! build a request from the running message history, stream one completion,
//! accumulate assistant content, and — when the turn produced tool calls —
//! execute them, thread their results back into the history, and loop. When a
//! turn produces no tool calls, it yields a terminal `FinalResponse`.
//!
//! The generator yields rig's own `MultiTurnStreamItem` (kept per decision D3),
//! so the existing `StreamProcessor` consumer and the `inference.rs` lifecycle
//! envelope around it consume the owned loop with no changes — only the stream
//! *source* moves from `Agent::stream_prompt` to `run_loop_stream`.
//!
//! Tool side-effects (lifecycle tracking, truncation/spill, persistence) are
//! NOT reimplemented here: the generator calls the existing
//! `DefraSessionHook::on_tool_call` / `on_tool_result` methods directly (the
//! former `PromptHook` callbacks). The generator owns only the orchestration:
//! request construction, turn iteration, deadline/cancellation-aware dispatch,
//! native result bounding, and message threading. Because the bounded result is
//! threaded into the conversation by construction, the in-loop truncation gap
//! (#401) is closed natively without the recorder shim.

use std::sync::Arc;
use std::time::Duration;

use async_stream::try_stream;
use chrono::{DateTime, Utc};
use futures::{Stream, StreamExt};
use rig::agent::{HookAction, MultiTurnStreamItem, PromptHook, StreamingError, ToolCallHookAction};
use rig::completion::message::{ToolCall, ToolResult, ToolResultContent, UserContent};
use rig::completion::{CompletionError, CompletionModel, CompletionRequest, GetTokenUsage, Message, Usage};
use rig::message::ToolChoice;
use rig::one_or_many::OneOrMany;
use rig::streaming::{StreamedAssistantContent, StreamedUserContent};
use rig::tool::ToolDyn;

use super::stream_processor::AssistantTurnAccumulator;
use crate::hook::DefraSessionHook;
use crate::tool_call_lifecycle::runtime::{
    cancelled_result, current_tool_runtime_context, timeout_result,
};
use crate::truncation::{tool_result_truncation_mode, truncate_text, TruncationLimits};

#[cfg(test)]
mod tests;

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

/// Drive the owned multi-turn loop, producing a stream of `MultiTurnStreamItem`s.
///
/// `prompt` is the new user message; `history` is the prior conversation
/// (without the new prompt). `tools` are dispatched by name when the model
/// calls them — they must be the *unwrapped* tools; the generator applies the
/// deadline/cancellation envelope and result bounding itself.
pub(crate) fn run_loop_stream<M>(
    model: M,
    hook: Option<DefraSessionHook>,
    prompt: Message,
    history: Vec<Message>,
    tools: Arc<Vec<Box<dyn ToolDyn>>>,
    config: LoopConfig,
) -> impl Stream<Item = Result<MultiTurnStreamItem<M::StreamingResponse>, StreamingError>>
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

            // Mirror rig's per-turn `on_completion_call` (prompt_request/streaming.rs
            // fires it inside the turn loop): on turn 1 this creates the session and
            // persists the user prompt; the hook's own state dedupes later turns.
            // A `None` hook is a non-persisting call (compaction/title summaries).
            if let Some(hook) = hook.as_ref() {
                let history_snapshot: Vec<Message> =
                    history.iter().chain(prior.iter()).cloned().collect();
                if let HookAction::Terminate { reason } =
                    <DefraSessionHook as PromptHook<M>>::on_completion_call(
                        hook,
                        &current_prompt,
                        &history_snapshot,
                    )
                    .await
                {
                    Err(StreamingError::Completion(CompletionError::ResponseError(reason)))?;
                }
            }

            let request = build_request(&model, current_prompt, &history, prior, tools.as_slice(), &config).await?;

            let mut stream = model
                .stream(request)
                .await
                .map_err(StreamingError::Completion)?;

            // Accumulate assistant content twice over: `accumulator` builds the
            // assistant message we thread back into `new_messages` for the next
            // turn (reasoning/tool-call/text ordering handled there), while the
            // yielded items drive the consumer's own accumulation/persistence.
            let mut accumulator = AssistantTurnAccumulator::default();
            let mut tool_calls: Vec<(ToolCall, String)> = Vec::new();
            let mut turn_text = String::new();

            while let Some(item) = stream.next().await {
                match item.map_err(StreamingError::Completion)? {
                    StreamedAssistantContent::Text(text) => {
                        turn_text.push_str(&text.text);
                        accumulator.push_text(&text.text);
                        yield MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::Text(text));
                    }
                    StreamedAssistantContent::Reasoning(reasoning) => {
                        accumulator.push_reasoning(reasoning.clone());
                        yield MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::Reasoning(reasoning));
                    }
                    StreamedAssistantContent::ReasoningDelta { id, reasoning } => {
                        accumulator.push_reasoning_delta(id.clone(), &reasoning);
                        yield MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::ReasoningDelta { id, reasoning });
                    }
                    StreamedAssistantContent::ToolCall { tool_call, internal_call_id } => {
                        accumulator.push_tool_call(tool_call.clone());
                        tool_calls.push((tool_call.clone(), internal_call_id.clone()));
                        yield MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::ToolCall { tool_call, internal_call_id });
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
                yield MultiTurnStreamItem::final_response(&turn_text, aggregated_usage);
                break;
            }

            // Thread the assistant turn (reasoning + tool calls + text) ahead of
            // its tool results, matching rig's history ordering.
            if let Some(assistant_message) = accumulator.take_message() {
                new_messages.push(assistant_message);
            }

            for (tool_call, internal_call_id) in tool_calls {
                let tool_name = tool_call.function.name.clone();
                let tool_args = value_to_json_string(&tool_call.function.arguments);

                // on_tool_call: register the lifecycle / persist the call. May
                // veto (Skip) or abort the whole request (Terminate). With no
                // hook (non-persisting calls) the tool simply executes.
                let call_action = match hook.as_ref() {
                    Some(hook) => {
                        <DefraSessionHook as PromptHook<M>>::on_tool_call(
                            hook,
                            &tool_name,
                            tool_call.call_id.clone(),
                            &internal_call_id,
                            &tool_args,
                        )
                        .await
                    }
                    None => ToolCallHookAction::Continue,
                };

                let bounded_result = match call_action {
                    ToolCallHookAction::Terminate { reason } => {
                        Err(StreamingError::Completion(CompletionError::ResponseError(reason)))?;
                        unreachable!("Err(..)? above ends the stream");
                    }
                    ToolCallHookAction::Skip { reason } => {
                        // Skipped: the rejection reason is the tool result; no
                        // dispatch and no on_tool_result (matches rig).
                        reason
                    }
                    _ => {
                        // Dispatch the unwrapped tool inside our own
                        // deadline/cancellation envelope, then bound the
                        // model-facing result natively (#401) before threading.
                        let full_result =
                            dispatch_tool(tools.as_slice(), &tool_name, tool_args.clone()).await;
                        let (bounded, _, _) = truncate_text(
                            &full_result,
                            tool_result_truncation_mode(&tool_name),
                            &TruncationLimits::default(),
                        );

                        // on_tool_result persists/spills the FULL result and
                        // drives the lifecycle to its terminal state.
                        if let Some(hook) = hook.as_ref() {
                            let result_action =
                                <DefraSessionHook as PromptHook<M>>::on_tool_result(
                                    hook,
                                    &tool_name,
                                    tool_call.call_id.clone(),
                                    &internal_call_id,
                                    &tool_args,
                                    &full_result,
                                )
                                .await;
                            if let HookAction::Terminate { reason } = result_action {
                                Err(StreamingError::Completion(CompletionError::ResponseError(
                                    reason,
                                )))?;
                            }
                        }
                        bounded
                    }
                };

                // Thread the bounded result into history for the next turn and
                // forward it to the consumer.
                let content = ToolResultContent::from_tool_output(bounded_result);
                let user_content = match tool_call.call_id.clone() {
                    Some(call_id) => UserContent::tool_result_with_call_id(
                        tool_call.id.clone(),
                        call_id,
                        content.clone(),
                    ),
                    None => UserContent::tool_result(tool_call.id.clone(), content.clone()),
                };
                new_messages.push(Message::User {
                    content: OneOrMany::one(user_content),
                });

                let tool_result = ToolResult {
                    id: tool_call.id.clone(),
                    call_id: tool_call.call_id.clone(),
                    content,
                };
                yield MultiTurnStreamItem::StreamUserItem(StreamedUserContent::ToolResult {
                    tool_result,
                    internal_call_id,
                });
            }
        }
    }
}

/// Drive the owned loop to completion and return the final assistant text,
/// for the non-streaming call sites (`oneshot`, `compaction`, title generation)
/// that previously used rig's `Agent::prompt`. Tool side-effects still run via
/// the hook when present; intermediate stream items are discarded.
pub(crate) async fn run_loop_to_text<M>(
    model: M,
    hook: Option<DefraSessionHook>,
    prompt: Message,
    history: Vec<Message>,
    tools: Arc<Vec<Box<dyn ToolDyn>>>,
    config: LoopConfig,
) -> Result<String, StreamingError>
where
    M: CompletionModel + 'static,
    M::StreamingResponse: 'static,
{
    let stream = run_loop_stream(model, hook, prompt, history, tools, config);
    futures::pin_mut!(stream);
    let mut final_text = String::new();
    while let Some(item) = stream.next().await {
        if let MultiTurnStreamItem::FinalResponse(final_response) = item? {
            final_text = final_response.response().to_string();
        }
    }
    Ok(final_text)
}

/// Serialize tool-call arguments the way rig does (`json_utils::value_to_json_string`):
/// a JSON string passes through unquoted, anything else is rendered as JSON.
fn value_to_json_string(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(string) => string.clone(),
        other => other.to_string(),
    }
}

/// Time left until `deadline_at`, or `None` if unbounded. Mirrors the helper in
/// `tool_call_lifecycle::runtime`.
fn deadline_remaining(deadline_at: Option<DateTime<Utc>>) -> Option<Duration> {
    let deadline_at = deadline_at?;
    let now = Utc::now();
    if now >= deadline_at {
        return Some(Duration::ZERO);
    }
    Some((deadline_at - now).to_std().unwrap_or(Duration::ZERO))
}

/// Dispatch one tool call by name, applying the active request's
/// deadline/cancellation envelope (when a tool runtime scope is in effect).
/// Returns the tool's full (unbounded) output, or a managed-terminal marker
/// string that `on_tool_result` classifies into a timed-out/cancelled outcome.
async fn dispatch_tool(tools: &[Box<dyn ToolDyn>], name: &str, args: String) -> String {
    let Some(tool) = tools.iter().find(|tool| tool.name() == name) else {
        return format!("error: unknown tool '{name}'");
    };

    let Some(scope) = current_tool_runtime_context() else {
        return tool.call(args).await.unwrap_or_else(|error| error.to_string());
    };

    if deadline_remaining(scope.deadline_at).is_some_and(|remaining| remaining.is_zero()) {
        return timeout_result(scope.deadline_at);
    }

    let deadline_at = scope.deadline_at;
    let mut deadline = Box::pin(async move {
        match deadline_remaining(deadline_at) {
            Some(remaining) => tokio::time::sleep(remaining).await,
            None => std::future::pending::<()>().await,
        }
    });

    tokio::select! {
        biased;
        _ = scope.cancellation_token.cancelled() => cancelled_result(),
        _ = &mut deadline => timeout_result(scope.deadline_at),
        result = tool.call(args) => result.unwrap_or_else(|error| error.to_string()),
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
