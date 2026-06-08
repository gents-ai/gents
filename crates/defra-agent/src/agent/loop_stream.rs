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

use async_stream::try_stream;
use futures::{Stream, StreamExt};
use rig::agent::{MultiTurnStreamItem, StreamingError};
use crate::llm::{HookAction, ToolCallHookAction};
use rig::completion::message::{ToolCall, ToolResult, ToolResultContent, UserContent};
use rig::completion::{
    CompletionModel, CompletionRequest, GetTokenUsage, Message, PromptError, Usage,
};

use crate::llm::ToolChoice;
use rig::one_or_many::OneOrMany;
use rig::streaming::{StreamedAssistantContent, StreamedUserContent};
use crate::llm::tool::ToolDyn;

use super::stream_processor::AssistantTurnAccumulator;
use crate::hook::DefraSessionHook;
use crate::tool_call_lifecycle::runtime::{
    cancelled_result, current_tool_runtime_context, deadline_remaining, timeout_result,
};
use crate::truncation::{tool_result_truncation_mode, truncate_text, TruncationLimits};

#[cfg(test)]
mod tests;

/// Per-request configuration for the loop, mirroring the agent-builder knobs we
/// previously handed to rig (`completion_factory::configure_agent_builder`).
#[derive(Clone)]
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
        // Provider-input chokepoint: every completion request in the system is
        // born in this loop (daemon inference, oneshot, compaction summarize,
        // title, subagent children), so sanitizing the caller-provided history
        // ONCE at entry guarantees provider-valid input for every consumer —
        // no call site can forget the boundary. Only the loaded history is
        // sanitized: the loop's own threaded messages (`new_messages`) are
        // provider-valid by construction, and sanitizing them mid-flight would
        // mis-drop a tool call whose result rides as the next turn's prompt.
        let history = crate::compaction::sanitize_history_for_provider(history);
        // The running set of messages produced this request. The last element
        // is always the "prompt" for the next turn (rig semantics): initially
        // the user message, later the trailing tool-result user message.
        let mut new_messages: Vec<Message> = vec![prompt];
        let mut aggregated_usage = Usage::new();
        let mut current_turn: usize = 0;

        loop {
            // rig semantics: `max_turns` is the number of tool round-trips, so up
            // to `max_turns + 1` completions are allowed (the extra one produces
            // the final text answer after the last tool call). Matches rig's
            // `current_max_turns > self.max_turns + 1` break.
            //
            // Emit as `StreamingError::Prompt(MaxTurnsError)` — the same variant
            // rig uses — so `classify_completion_error` treats turn exhaustion as a
            // PERMANENT failure. A generic `Completion(ResponseError)` would be
            // classified transient and retried, re-running tools on each attempt.
            if current_turn > config.max_turns + 1 {
                let prompt = new_messages
                    .last()
                    .cloned()
                    .expect("new_messages always retains at least the initial prompt");
                let chat_history =
                    error_chat_history(&history, &new_messages[..new_messages.len() - 1]);
                Err(StreamingError::Prompt(Box::new(PromptError::MaxTurnsError {
                    max_turns: config.max_turns,
                    chat_history: Box::new(chat_history),
                    prompt: Box::new(prompt),
                })))?;
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
                    hook.on_completion_call(&current_prompt, &history_snapshot).await
                {
                    Err(StreamingError::Prompt(Box::new(PromptError::PromptCancelled {
                        chat_history: error_chat_history(&history, &new_messages),
                        reason,
                    })))?;
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
            // `pending_results` holds each tool call's bounded result, executed
            // inline as its ToolCall arrives (see below) and threaded/yielded only
            // once the turn's stream has drained.
            let mut accumulator = AssistantTurnAccumulator::default();
            let mut pending_results: Vec<(ToolCall, String, String)> = Vec::new();
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
                        // Yield the tool call first (so the consumer registers its
                        // stream-call identity), then execute it immediately — rig
                        // runs each tool the moment its ToolCall arrives. Executing
                        // here, rather than after the turn's stream drains, means
                        // the lifecycle / AgentToolCall row exists before the
                        // provider can stall on the rest of the stream; otherwise
                        // the daemon liveness timeout fires with no in-flight call
                        // to cancel and the tool is silently lost. The bounded
                        // result is threaded and yielded only after the loop, so
                        // the assistant turn (all its tool calls) still persists as
                        // one message ahead of its results.
                        yield MultiTurnStreamItem::StreamAssistantItem(
                            StreamedAssistantContent::ToolCall {
                                tool_call: tool_call.clone(),
                                internal_call_id: internal_call_id.clone(),
                            },
                        );

                        let tool_name = tool_call.function.name.clone();
                        let tool_args = value_to_json_string(&tool_call.function.arguments);

                        // on_tool_call: register the lifecycle / persist the call.
                        // May veto (Skip) or abort the whole request (Terminate).
                        // With no hook (non-persisting calls) the tool just runs.
                        let call_action = match hook.as_ref() {
                            Some(hook) => {
                                hook.on_tool_call(
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
                                Err(StreamingError::Prompt(Box::new(
                                    PromptError::PromptCancelled {
                                        chat_history: error_chat_history(&history, &new_messages),
                                        reason,
                                    },
                                )))?;
                                unreachable!("Err(..)? above ends the stream");
                            }
                            ToolCallHookAction::Skip { reason } => {
                                // Skipped: the rejection reason is the tool result;
                                // no dispatch and no on_tool_result (matches rig).
                                reason
                            }
                            _ => {
                                // Dispatch the unwrapped tool inside our own
                                // deadline/cancellation envelope, then bound the
                                // model-facing result natively (#401) before
                                // threading.
                                let full_result = dispatch_tool(
                                    tools.as_slice(),
                                    &tool_name,
                                    tool_args.clone(),
                                )
                                .await;
                                let (bounded, _, _) = truncate_text(
                                    &full_result,
                                    tool_result_truncation_mode(&tool_name),
                                    &TruncationLimits::default(),
                                );

                                // on_tool_result persists/spills the FULL result
                                // and drives the lifecycle to its terminal state.
                                if let Some(hook) = hook.as_ref() {
                                    let result_action = hook
                                        .on_tool_result(
                                            &tool_name,
                                            tool_call.call_id.clone(),
                                            &internal_call_id,
                                            &tool_args,
                                            &full_result,
                                        )
                                        .await;
                                    if let HookAction::Terminate { reason } = result_action {
                                        Err(StreamingError::Prompt(Box::new(
                                            PromptError::PromptCancelled {
                                                chat_history: error_chat_history(
                                                    &history,
                                                    &new_messages,
                                                ),
                                                reason,
                                            },
                                        )))?;
                                    }
                                }
                                bounded
                            }
                        };

                        pending_results.push((tool_call, internal_call_id, bounded_result));
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

            if pending_results.is_empty() {
                yield MultiTurnStreamItem::final_response(&turn_text, aggregated_usage);
                break;
            }

            // Thread the assistant turn (text + reasoning + tool calls) ahead of
            // its tool results, matching rig's history ordering. Carry the
            // provider message id (captured into `stream.message_id` from the
            // stream's `MessageId` event) onto the threaded message — rig threads
            // this same id, and OpenAI Responses / ChatGPT Codex follow-up
            // requests reference prior `msg_` ids, so dropping it breaks them.
            if let Some(mut assistant_message) = accumulator.take_message() {
                if let Message::Assistant { id, .. } = &mut assistant_message {
                    *id = stream.message_id.clone();
                }
                new_messages.push(assistant_message);
            }

            // The tools already ran inline as their ToolCalls arrived; now that the
            // assistant turn is complete, thread each bounded result into history
            // and forward it to the consumer (which persists the assistant turn on
            // the first tool result, so results must trail the whole turn).
            for (tool_call, internal_call_id, bounded_result) in pending_results {
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

/// Drive the owned loop to completion and return the final assistant text, for
/// the non-streaming call sites (`oneshot`, `compaction`, title generation) that
/// previously used rig's `Agent::prompt`.
///
/// When a hook is present (one-shot), this persists the transcript exactly as the
/// daemon's `StreamProcessor` does — assistant turns and tool-result messages —
/// so one-shot sessions store the full reply, not just the prompt. (The daemon
/// path has its own `StreamProcessor`; this is the equivalent for the collected,
/// non-streaming path, minus the live-streaming/response-doc bits.) With no hook
/// (compaction/title) nothing is persisted. Persistence honors the hook's
/// `FailurePolicy` via `apply_persistence_policy` — exactly as `StreamProcessor`
/// does — so a fail-closed hook (one-shot's default) terminates the run on a
/// persistence error rather than silently dropping the transcript.
pub(crate) async fn run_loop_to_text<M>(
    model: M,
    hook: Option<DefraSessionHook>,
    prompt: Message,
    history: Vec<Message>,
    tools: Arc<Vec<Box<dyn ToolDyn>>>,
    config: LoopConfig,
) -> anyhow::Result<String>
where
    M: CompletionModel + 'static,
    M::StreamingResponse: 'static,
{
    let stream = run_loop_stream(model, hook.clone(), prompt, history, tools, config);
    futures::pin_mut!(stream);
    let mut accumulator = AssistantTurnAccumulator::default();
    let mut final_text = String::new();

    while let Some(item) = stream.next().await {
        match item.map_err(|error| anyhow::anyhow!("one-shot loop stream error: {error}"))? {
            MultiTurnStreamItem::StreamAssistantItem(content) => match content {
                StreamedAssistantContent::Text(text) => accumulator.push_text(&text.text),
                StreamedAssistantContent::Reasoning(reasoning) => {
                    accumulator.push_reasoning(reasoning)
                }
                StreamedAssistantContent::ReasoningDelta { id, reasoning } => {
                    accumulator.push_reasoning_delta(id, &reasoning)
                }
                StreamedAssistantContent::ToolCall {
                    tool_call,
                    internal_call_id,
                } => {
                    if let Some(hook) = hook.as_ref() {
                        hook.register_stream_tool_call_identity(
                            &internal_call_id,
                            &tool_call.id,
                            tool_call.call_id.as_deref(),
                        )
                        .await;
                    }
                    accumulator.push_tool_call(tool_call);
                }
                _ => {}
            },
            MultiTurnStreamItem::StreamUserItem(StreamedUserContent::ToolResult {
                tool_result,
                internal_call_id,
            }) => {
                if let Some(hook) = hook.as_ref() {
                    if let Some(message) = accumulator.take_message() {
                        hook.apply_persistence_policy(
                            hook.persist_message(&message).await.map(|_| ()),
                            "persist one-shot assistant turn",
                        )?;
                    }
                    hook.apply_persistence_policy(
                        hook.persist_stream_tool_result_message(&tool_result, &internal_call_id)
                            .await,
                        "persist one-shot tool result",
                    )?;
                }
            }
            MultiTurnStreamItem::FinalResponse(final_response) => {
                accumulator.reconcile_text(final_response.response());
                if let Some(hook) = hook.as_ref() {
                    if let Some(message) = accumulator.take_message() {
                        hook.apply_persistence_policy(
                            hook.persist_message(&message).await.map(|_| ()),
                            "persist one-shot final assistant turn",
                        )?;
                    }
                }
                final_text = final_response.response().to_string();
            }
            _ => {}
        }
    }
    Ok(final_text)
}

/// Conversation for a terminal `PromptError` payload: the caller-provided
/// history followed by the messages this loop has threaded. Mirrors rig, whose
/// `MaxTurnsError` / `PromptCancelled` carry input history *plus* new messages —
/// classification aside, this keeps prior context in an inspected error.
fn error_chat_history(history: &[Message], new_messages: &[Message]) -> Vec<Message> {
    history.iter().chain(new_messages.iter()).cloned().collect()
}

/// The rag/context text handed to each tool's `definition`, mirroring rig's
/// selection (`completion.rs`): the current prompt's user text if it has any,
/// otherwise the most recent user-text message across `history` + `prior`. A
/// tool-result turn's prompt carries no text, so without the fallback a
/// prompt-aware tool would lose the task/subagent/manual prompt the provider
/// still sees. Built-in Defra tools ignore it; this preserves parity for custom
/// or embedding tools.
fn current_rag_text(prompt: &Message, history: &[Message], prior: &[Message]) -> String {
    if let Some(text) = message_user_text(prompt) {
        return text;
    }
    history
        .iter()
        .chain(prior.iter())
        .rev()
        .find_map(message_user_text)
        .unwrap_or_default()
}

/// First text block of a user message, mirroring rig's `Message::rag_text`
/// (`pub(crate)` there). `None` for non-user messages and text-free prompts
/// (e.g. tool-result turns).
fn message_user_text(message: &Message) -> Option<String> {
    if let Message::User { content } = message {
        for item in content.iter() {
            if let UserContent::Text(text) = item {
                return Some(text.text.clone());
            }
        }
    }
    None
}

/// Serialize tool-call arguments the way rig does (`json_utils::value_to_json_string`):
/// a JSON string passes through unquoted, anything else is rendered as JSON.
fn value_to_json_string(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(string) => string.clone(),
        other => other.to_string(),
    }
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
        return tool
            .call(args)
            .await
            .unwrap_or_else(|error| error.to_string());
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
    // The current prompt's rag text (with rig's history fallback) is handed to
    // each tool's `definition` so prompt-aware (dynamic) tools can tailor their
    // schema. Built-in tools ignore it; this preserves parity for custom tools.
    // Definitions are native; converted to rig's at the provider boundary
    // (Layer A) for the outgoing request.
    let rag_text = current_rag_text(&prompt, history, prior);
    let mut tool_defs = Vec::with_capacity(tools.len());
    for tool in tools {
        let native = tool.definition(rag_text.clone()).await;
        tool_defs.push(crate::llm::rig_compat::to_rig_tool_definition(&native));
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
        builder = builder.tool_choice(crate::llm::rig_compat::to_rig_tool_choice(tool_choice));
    }

    Ok(builder.build())
}
