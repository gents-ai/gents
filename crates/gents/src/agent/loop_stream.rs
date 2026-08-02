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
//! The generator yields a native `LoopStreamItem` envelope around rig's
//! `MultiTurnStreamItem`, keeping provider payloads at the rig boundary while
//! giving the runtime a place to carry retry-control events.
//!
//! Tool side-effects (lifecycle tracking, truncation/spill, persistence) are
//! NOT reimplemented here: the generator calls the existing
//! `DefraSessionHook::on_tool_call` / `on_tool_result` methods directly (the
//! former `PromptHook` callbacks). The generator owns only the orchestration:
//! request construction, turn iteration, deadline/cancellation-aware dispatch,
//! native result bounding, and message threading. Because the bounded result is
//! threaded into the conversation by construction, the in-loop truncation gap
//! (#401) is closed natively without the recorder shim.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use chrono::{DateTime, Utc};

use crate::agent::completion_retry::{
    CompletionRetryPolicy, CompletionRetryState, MidStreamDirective, PreStreamDirective,
};
use crate::error::InferenceError;
use crate::llm::message::{
    AssistantContent, Message, ToolCall, ToolResult, ToolResultContent, UserContent,
};
use crate::llm::rig_compat;
use crate::llm::{HookAction, ToolCallHookAction};
use async_stream::try_stream;
use futures::{Stream, StreamExt};
use rig::agent::{MultiTurnStreamItem, StreamingError};
use rig::completion::{
    CompletionError, CompletionModel, CompletionRequest, GetTokenUsage, PromptError, Usage,
};

use crate::llm::tool::{
    ToolDyn, ToolError, UnparseableArgsKind, JSON_ERROR_PREFIX, TOOL_CALL_ERROR_PREFIX,
};
use crate::llm::ToolChoice;
use rig::streaming::{StreamedAssistantContent, StreamedUserContent};

use super::stream_processor::AssistantTurnAccumulator;
use crate::hook::DefraSessionHook;
use crate::tool_call_lifecycle::runtime::{
    cancelled_result, current_tool_runtime_context, deadline_remaining,
    scope_request_tool_execution_with_workspace_and_live_output, timeout_result,
    unparseable_args_notice, unparseable_args_result,
};
use crate::truncation::{tool_result_truncation_mode, truncate_text, TruncationLimits};

#[cfg(test)]
mod tests;

pub(crate) type RenderedRequestSink = Arc<
    dyn Fn(
            usize,
            u32,
            CompletionRequest,
        ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>>
        + Send
        + Sync,
>;

#[derive(Debug)]
#[allow(dead_code)]
pub(crate) enum LoopStreamItem<R> {
    Item(MultiTurnStreamItem<R>),
    TurnRetracted {
        turn: usize,
        attempt: u32,
        /// Backoff the generator sleeps before the resample. Carried so the
        /// daemon can extend the next poll's liveness budget by it, exactly as
        /// for `AttemptFailed` — otherwise a retract backoff longer than the
        /// liveness timeout is misread as a dead stream (#648).
        backoff: std::time::Duration,
    },
    AttemptFailed {
        turn: usize,
        attempt: u32,
        error: InferenceError,
        will_retry: bool,
        backoff: std::time::Duration,
    },
}

#[derive(Clone)]
pub(crate) struct LoopConfig {
    pub(crate) preamble: Option<String>,
    pub(crate) context_message: Option<Message>,
    pub(crate) temperature: Option<f64>,
    pub(crate) max_tokens: Option<u64>,
    pub(crate) additional_params: Option<serde_json::Value>,
    pub(crate) tool_choice: Option<ToolChoice>,
    pub(crate) on_rendered_request: Option<RenderedRequestSink>,
    pub(crate) retry_policy: CompletionRetryPolicy,
    pub(crate) deadline: Option<DateTime<Utc>>,
    pub(crate) max_turns: usize,
}

/// Assemble the per-request message tail: the optional `<context>` message rides
/// immediately before the prompt, which is always last (rig prompt semantics).
///
/// This mirrors Lean `PromptAssembly.Template.assembleWithContext`, whose
/// `assembleWithContext_tail` theorem fixes the order as `... contextPreamble,
/// prompt`. Fenced by `tests` (`assembles_context_immediately_before_prompt`);
/// reordering here breaks that test and contradicts the proof.
pub(crate) fn assemble_new_messages(
    context_message: Option<Message>,
    prompt: Message,
) -> Vec<Message> {
    let mut new_messages: Vec<Message> = Vec::with_capacity(2);
    if let Some(context_message) = context_message {
        new_messages.push(context_message);
    }
    new_messages.push(prompt);
    new_messages
}

pub(crate) fn is_request_context_message(message: &Message) -> bool {
    let Message::User { content } = message else {
        return false;
    };
    let [UserContent::Text(text)] = content.as_slice() else {
        return false;
    };
    let trimmed = text.text.trim();
    trimmed.starts_with("<context>") && trimmed.ends_with("</context>")
}

pub(crate) fn run_loop_stream<M>(
    model: M,
    hook: Option<DefraSessionHook>,
    prompt: Message,
    history: Vec<Message>,
    tools: Arc<Vec<Box<dyn ToolDyn>>>,
    config: LoopConfig,
) -> impl Stream<Item = Result<LoopStreamItem<M::StreamingResponse>, StreamingError>>
where
    M: CompletionModel + 'static,
    M::StreamingResponse: 'static,
{
    try_stream! {
        let history = crate::compaction::sanitize_history_for_provider(history);
        // #497: prior requests' per-request `<context>` messages are durably
        // persisted (training capture), but must NOT be replayed to the provider:
        // they carry stale `ctx.now` / collection summaries and would accumulate
        // unboundedly across a multi-request session, inflating tokens and
        // presenting stale context as current. Strip them from the provider-bound
        // history; the CURRENT request's context rides in `new_messages` below.
        // Persistence is untouched (it already happened upstream).
        // `mut` because the completion-retry Repair directive rewrites the
        // assembled input in place — loaded history included (#652).
        let mut history: Vec<Message> = history
            .into_iter()
            .filter(|message| !is_request_context_message(message))
            .collect();
        // The running set of messages produced this request. The last element
        // is always the "prompt" for the next turn (rig semantics): initially
        // the user message, later the trailing tool-result user message. The
        // optional per-request context message rides immediately before the
        // prompt (mirrors Lean `PromptAssembly.Template.assembleWithContext`).
        let mut new_messages: Vec<Message> =
            assemble_new_messages(config.context_message.clone(), prompt);
        let mut aggregated_usage = Usage::new();
        let mut current_turn: usize = 0;
        let mut retry = CompletionRetryState::new(config.retry_policy.clone());

        'turns: loop {
            if current_turn > config.max_turns + 1 {
                let prompt = new_messages
                    .last()
                    .cloned()
                    .expect("new_messages always retains at least the initial prompt");
                let chat_history = rig_compat::to_rig_messages(&error_chat_history(
                    &history,
                    &new_messages[..new_messages.len() - 1],
                ));
                Err(StreamingError::Prompt(Box::new(PromptError::MaxTurnsError {
                    max_turns: config.max_turns,
                    chat_history: Box::new(chat_history),
                    prompt: Box::new(rig_compat::to_rig_message(&prompt)),
                })))?;
            }
            current_turn += 1;

            let current_prompt = new_messages
                .last()
                .cloned()
                .expect("new_messages always retains at least the initial prompt");
            let prior = &new_messages[..new_messages.len() - 1];

            if let Some(hook) = hook.as_ref() {
                let history_snapshot: Vec<Message> =
                    history.iter().chain(prior.iter()).cloned().collect();
                if let HookAction::Terminate { reason } =
                    hook.on_completion_call_with_context(
                        &current_prompt,
                        &history_snapshot,
                        (current_turn == 1)
                            .then_some(config.context_message.as_ref())
                            .flatten(),
                    ).await
                {
                    Err(StreamingError::Prompt(Box::new(PromptError::PromptCancelled {
                        chat_history: rig_compat::to_rig_messages(&error_chat_history(
                            &history,
                            &new_messages,
                        )),
                        reason,
                    })))?;
                }
            }

            let turn_index = current_turn - 1;
            let mut attempt = 0_u32;
            let mut request = build_request(&model, current_prompt, &history, prior, tools.as_slice(), &config).await?;
            'attempts: loop {
                let mut stream = loop {
                    if let Some(on_rendered_request) = config.on_rendered_request.as_ref() {
                        on_rendered_request(turn_index, attempt, request.clone())
                            .await
                            .map_err(|error| {
                                StreamingError::Completion(CompletionError::ProviderError(format!(
                                    "capturing rendered completion request failed: {error:#}"
                                )))
                            })?;
                    }

                    match model.stream(request.clone()).await {
                        Ok(stream) => break stream,
                        Err(completion_error) => {
                            let streaming_error = StreamingError::Completion(completion_error);
                            let classified = crate::error::classify_completion_error(&streaming_error);
                            let error_text = streaming_error.to_string();
                            match retry.on_pre_stream_failure(
                                &classified,
                                &error_text,
                                Utc::now(),
                                config.deadline,
                            ) {
                                PreStreamDirective::RetryAfter { delay, kind } => {
                                    yield LoopStreamItem::AttemptFailed {
                                        turn: turn_index,
                                        attempt,
                                        error: classified,
                                        will_retry: true,
                                        backoff: delay,
                                    };
                                    tracing::warn!(
                                        turn = turn_index,
                                        attempt,
                                        kind = ?kind,
                                        delay_ms = delay.as_millis() as u64,
                                        error = %error_text,
                                        "retrying completion after transient failure"
                                    );
                                    tokio::time::sleep(delay).await;
                                    attempt += 1;
                                }
                                PreStreamDirective::Repair => {
                                    retry.mark_repair_used();
                                    yield LoopStreamItem::AttemptFailed {
                                        turn: turn_index,
                                        attempt,
                                        error: classified,
                                        will_retry: true,
                                        backoff: std::time::Duration::ZERO,
                                    };
                                    repair_provider_input(&mut history, &mut new_messages);
                                    let repaired_prompt = new_messages
                                        .last()
                                        .cloned()
                                        .expect("new_messages remains non-empty after repair");
                                    let repaired_prior = &new_messages[..new_messages.len() - 1];
                                    request = build_request(
                                        &model,
                                        repaired_prompt,
                                        &history,
                                        repaired_prior,
                                        tools.as_slice(),
                                        &config,
                                    )
                                    .await?;
                                    attempt += 1;
                                }
                                PreStreamDirective::Fail { reason } => {
                                    let terminal_reason =
                                        terminal_pre_stream_retry_reason(&classified, attempt, reason);
                                    yield LoopStreamItem::AttemptFailed {
                                        turn: turn_index,
                                        attempt,
                                        error: classified,
                                        will_retry: false,
                                        backoff: std::time::Duration::ZERO,
                                    };
                                    Err(StreamingError::Completion(
                                        CompletionError::ProviderError(terminal_reason),
                                    ))?;
                                    unreachable!("Err(..)? above ends the stream");
                                }
                            }
                        }
                    }
                };

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
            let mut saw_stream_item = false;

            while let Some(item) = stream.next().await {
                let item = match item {
                    Ok(item) => {
                        saw_stream_item = true;
                        item
                    }
                    Err(completion_error) if pending_results.is_empty() => {
                        let streaming_error = StreamingError::Completion(completion_error);
                        let classified = crate::error::classify_completion_error(&streaming_error);
                        let error_text = streaming_error.to_string();
                        if !saw_stream_item {
                            match retry.on_pre_stream_failure(
                                &classified,
                                &error_text,
                                Utc::now(),
                                config.deadline,
                            ) {
                                PreStreamDirective::RetryAfter { delay, kind } => {
                                    yield LoopStreamItem::AttemptFailed {
                                        turn: turn_index,
                                        attempt,
                                        error: classified,
                                        will_retry: true,
                                        backoff: delay,
                                    };
                                    tracing::warn!(
                                        turn = turn_index,
                                        attempt,
                                        kind = ?kind,
                                        delay_ms = delay.as_millis() as u64,
                                        error = %error_text,
                                        "retrying completion after first stream item failed"
                                    );
                                    tokio::time::sleep(delay).await;
                                    attempt += 1;
                                    continue 'attempts;
                                }
                                PreStreamDirective::Repair => {
                                    retry.mark_repair_used();
                                    yield LoopStreamItem::AttemptFailed {
                                        turn: turn_index,
                                        attempt,
                                        error: classified,
                                        will_retry: true,
                                        backoff: std::time::Duration::ZERO,
                                    };
                                    repair_provider_input(&mut history, &mut new_messages);
                                    let repaired_prompt = new_messages.last().cloned().expect(
                                        "new_messages remains non-empty after repair",
                                    );
                                    let repaired_prior = &new_messages[..new_messages.len() - 1];
                                    request = build_request(
                                        &model,
                                        repaired_prompt,
                                        &history,
                                        repaired_prior,
                                        tools.as_slice(),
                                        &config,
                                    )
                                    .await?;
                                    attempt += 1;
                                    continue 'attempts;
                                }
                                PreStreamDirective::Fail { reason } => {
                                    let terminal_reason = terminal_pre_stream_retry_reason(
                                        &classified,
                                        attempt,
                                        reason,
                                    );
                                    yield LoopStreamItem::AttemptFailed {
                                        turn: turn_index,
                                        attempt,
                                        error: classified,
                                        will_retry: false,
                                        backoff: std::time::Duration::ZERO,
                                    };
                                    Err(StreamingError::Completion(
                                        CompletionError::ProviderError(terminal_reason),
                                    ))?;
                                    unreachable!("Err(..)? above ends the stream");
                                }
                            }
                        }
                        match retry.on_mid_stream_failure(false, Utc::now(), config.deadline) {
                            MidStreamDirective::RetractAndResample { delay } => {
                                yield LoopStreamItem::TurnRetracted {
                                    turn: turn_index,
                                    attempt,
                                    backoff: delay,
                                };
                                tracing::warn!(
                                    turn = turn_index,
                                    attempt,
                                    delay_ms = delay.as_millis() as u64,
                                    error = %error_text,
                                    "retracting partial completion turn after mid-stream failure"
                                );
                                tokio::time::sleep(delay).await;
                                attempt += 1;
                                continue 'attempts;
                            }
                            MidStreamDirective::CloseAndContinue { .. } => {
                                unreachable!(
                                    "no-effect mid-stream failure cannot close and continue"
                                );
                            }
                            MidStreamDirective::Fail { reason } => {
                                let terminal_reason =
                                    terminal_pre_stream_retry_reason(&classified, attempt, reason);
                                Err(StreamingError::Completion(
                                    CompletionError::ProviderError(terminal_reason),
                                ))?;
                                unreachable!("Err(..)? above ends the stream");
                            }
                        }
                    }
                    Err(completion_error) => {
                        let streaming_error = StreamingError::Completion(completion_error);
                        let classified = crate::error::classify_completion_error(&streaming_error);
                        let error_text = streaming_error.to_string();
                        match retry.on_mid_stream_failure(true, Utc::now(), config.deadline) {
                            MidStreamDirective::CloseAndContinue { delay } => {
                                for item in close_streaming_turn(
                                    &mut new_messages,
                                    &mut accumulator,
                                    stream.message_id.clone(),
                                    pending_results,
                                ) {
                                    yield item;
                                }
                                yield LoopStreamItem::AttemptFailed {
                                    turn: turn_index,
                                    attempt,
                                    error: classified,
                                    will_retry: true,
                                    backoff: delay,
                                };
                                tracing::warn!(
                                    turn = turn_index,
                                    attempt,
                                    delay_ms = delay.as_millis() as u64,
                                    error = %error_text,
                                    "closing completion turn after mid-stream failure with tool effects"
                                );
                                tokio::time::sleep(delay).await;
                                continue 'turns;
                            }
                            MidStreamDirective::RetractAndResample { .. } => {
                                unreachable!(
                                    "effectful mid-stream failure cannot retract and resample"
                                );
                            }
                            MidStreamDirective::Fail { reason } => {
                                let terminal_reason =
                                    terminal_pre_stream_retry_reason(&classified, attempt, reason);
                                Err(StreamingError::Completion(
                                    CompletionError::ProviderError(terminal_reason),
                                ))?;
                                unreachable!("Err(..)? above ends the stream");
                            }
                        }
                    }
                };

                match item {
                    StreamedAssistantContent::Text(text) => {
                        turn_text.push_str(&text.text);
                        accumulator.push_text(&text.text);
                        yield LoopStreamItem::Item(MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::Text(text)));
                    }
                    StreamedAssistantContent::Reasoning(reasoning) => {
                        accumulator.push_reasoning(rig_compat::from_rig_reasoning(&reasoning));
                        yield LoopStreamItem::Item(MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::Reasoning(reasoning)));
                    }
                    StreamedAssistantContent::ReasoningDelta { id, reasoning } => {
                        accumulator.push_reasoning_delta(id.clone(), &reasoning);
                        yield LoopStreamItem::Item(MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::ReasoningDelta { id, reasoning }));
                    }
                    StreamedAssistantContent::ToolCall { tool_call, internal_call_id } => {
                        accumulator.push_tool_call(rig_compat::from_rig_tool_call(&tool_call));
                        yield LoopStreamItem::Item(MultiTurnStreamItem::StreamAssistantItem(
                            StreamedAssistantContent::ToolCall {
                                tool_call: tool_call.clone(),
                                internal_call_id: internal_call_id.clone(),
                            },
                        ));

                        let tool_name = tool_call.function.name.clone();
                        let tool_args = value_to_json_string(&tool_call.function.arguments);

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
                                        chat_history: rig_compat::to_rig_messages(&error_chat_history(
                                            &history,
                                            &new_messages,
                                        )),
                                        reason,
                                    },
                                )))?;
                                unreachable!("Err(..)? above ends the stream");
                            }
                            ToolCallHookAction::Skip { reason } => {
                                reason
                            }
                            _ => {
                                let live_output = hook.as_ref().map(|hook| {
                                    hook.foreground_live_output_writer(&internal_call_id)
                                });
                                let full_result = dispatch_tool(
                                    tools.as_slice(),
                                    &tool_name,
                                    tool_args.clone(),
                                    live_output,
                                )
                                .await;
                                let (bounded, _, _) = truncate_text(
                                    &full_result,
                                    tool_result_truncation_mode(&tool_name),
                                    &TruncationLimits::default(),
                                );

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
                                                chat_history: rig_compat::to_rig_messages(&error_chat_history(
                                                    &history,
                                                    &new_messages,
                                                )),
                                                reason,
                                            },
                                        )))?;
                                    }
                                }
                                // The internal marker must never reach the model.
                                match unparseable_args_notice(&bounded) {
                                    Some(notice) => notice.to_string(),
                                    None => bounded,
                                }
                            }
                        };

                        pending_results.push((rig_compat::from_rig_tool_call(&tool_call), internal_call_id, bounded_result));
                    }
                    StreamedAssistantContent::ToolCallDelta { .. } => {
                    }
                    StreamedAssistantContent::Final(raw) => {
                        if let Some(usage) = raw.token_usage() {
                            aggregated_usage += usage;
                        }
                    }
                }
            }

            if pending_results.is_empty() {
                yield LoopStreamItem::Item(MultiTurnStreamItem::final_response(&turn_text, aggregated_usage));
                break 'turns;
            }

            for item in close_streaming_turn(
                &mut new_messages,
                &mut accumulator,
                stream.message_id.clone(),
                pending_results,
            ) {
                yield item;
            }
            break 'attempts;
        }
        }
    }
}

fn close_streaming_turn<R>(
    new_messages: &mut Vec<Message>,
    accumulator: &mut AssistantTurnAccumulator,
    message_id: Option<String>,
    pending_results: Vec<(ToolCall, String, String)>,
) -> Vec<LoopStreamItem<R>> {
    // Thread the assistant turn (text + reasoning + tool calls) ahead of its
    // tool results, matching rig's history ordering. Carry the provider
    // message id (captured into `stream.message_id` from the stream's
    // `MessageId` event) onto the threaded message — rig threads this same id,
    // and OpenAI Responses / ChatGPT Codex follow-up requests reference prior
    // `msg_` ids, so dropping it breaks them.
    if let Some(mut assistant_message) = accumulator.take_message() {
        if let Message::Assistant { id, .. } = &mut assistant_message {
            *id = message_id;
        }
        new_messages.push(assistant_message);
    }

    pending_results
        .into_iter()
        .map(|(tool_call, internal_call_id, bounded_result)| {
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
                content: vec![user_content],
            });

            let tool_result = ToolResult {
                id: tool_call.id.clone(),
                call_id: tool_call.call_id.clone(),
                content,
            };
            LoopStreamItem::Item(MultiTurnStreamItem::StreamUserItem(
                StreamedUserContent::ToolResult {
                    tool_result: rig_compat::to_rig_tool_result(&tool_result),
                    internal_call_id,
                },
            ))
        })
        .collect()
}

fn terminal_pre_stream_retry_reason(
    classified: &InferenceError,
    attempt: u32,
    reason: String,
) -> String {
    if !classified.is_retryable() {
        reason
    } else {
        format!(
            "completion retry budget exhausted after {} attempts: {reason}; last error: {classified}",
            attempt + 1
        )
    }
}

/// Repair the ASSEMBLED provider input — loaded history and run-threaded
/// messages alike (#652).
///
/// This runs only after the provider has already REJECTED the request (the
/// completion-retry `Repair` directive). It is deliberately more aggressive
/// than the egress normalizer: on top of the shape coercion it runs a LOSSY
/// leaf sanitizer over every JSON string in a tool call's arguments. That
/// lossiness is exactly why it cannot live at egress — it would corrupt
/// legitimate multi-line tool arguments on every request.
///
/// It used to rewrite only `new_messages`. But the motivating failure (the vLLM
/// parse-signature 400) originates from tool-call arguments in the INPUT
/// TRANSCRIPT — i.e. the loaded history it skipped — so repair re-issued the
/// same poisoned input and failed identically. The fence described a transform
/// that did not exist.
///
/// Widening it to history is licensed by `PromptAssembly.repair_is_payload_only`
/// (repair rewrites argument payloads only — never rows, roles, call ids, or
/// ordering, so the row-granular assembly theorems T1–T5 hold verbatim) and by
/// `PromptAssembly.repair_idempotent` (a second pass is a no-op, so re-entering
/// the path cannot keep re-escaping its own escapes).
pub(crate) fn repair_provider_input(history: &mut Vec<Message>, new_messages: &mut Vec<Message>) {
    repair_messages(history);
    repair_messages(new_messages);
    *history = crate::compaction::sanitize_history_for_provider(std::mem::take(history));
    *new_messages = crate::compaction::sanitize_history_for_provider(std::mem::take(new_messages));
}

fn repair_messages(messages: &mut [Message]) {
    for message in messages.iter_mut() {
        let Message::Assistant { content, .. } = message else {
            continue;
        };
        for item in content {
            let AssistantContent::ToolCall(tool_call) = item else {
                continue;
            };
            let mut repaired = crate::llm::tool::normalize_tool_call_arguments(
                "repair",
                &tool_call.function.name,
                &tool_call.function.arguments,
            );
            sanitize_json_string_leaves(&mut repaired);
            tool_call.function.arguments = repaired;
        }
    }
}

fn sanitize_json_string_leaves(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Array(values) => {
            for value in values {
                sanitize_json_string_leaves(value);
            }
        }
        serde_json::Value::Object(map) => {
            for value in map.values_mut() {
                sanitize_json_string_leaves(value);
            }
        }
        serde_json::Value::String(text) => {
            *text = sanitize_provider_arg_string(text);
        }
        _ => {}
    }
}

fn sanitize_provider_arg_string(text: &str) -> String {
    let mut sanitized = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\n' => sanitized.push_str("\\n"),
            '\t' => sanitized.push_str("\\t"),
            ch if ch.is_control() => {}
            ch => sanitized.push(ch),
        }
    }
    sanitized
}

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
    let mut last_attempt_error: Option<InferenceError> = None;

    while let Some(item) = stream.next().await {
        let item = item.map_err(|error| match last_attempt_error.as_ref() {
            Some(last_error) => {
                anyhow::anyhow!(
                    "one-shot loop stream error after retry failure ({last_error}): {error}"
                )
            }
            None => anyhow::anyhow!("one-shot loop stream error: {error}"),
        })?;
        match item {
            LoopStreamItem::TurnRetracted { .. } => {
                accumulator = AssistantTurnAccumulator::default();
                continue;
            }
            LoopStreamItem::AttemptFailed { error, .. } => {
                last_attempt_error = Some(error);
                continue;
            }
            LoopStreamItem::Item(item) => match item {
                MultiTurnStreamItem::StreamAssistantItem(content) => match content {
                    StreamedAssistantContent::Text(text) => accumulator.push_text(&text.text),
                    StreamedAssistantContent::Reasoning(reasoning) => {
                        accumulator.push_reasoning(rig_compat::from_rig_reasoning(&reasoning))
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
                        accumulator.push_tool_call(rig_compat::from_rig_tool_call(&tool_call));
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
                            hook.persist_stream_tool_result_message(
                                &rig_compat::from_rig_tool_result(&tool_result),
                                &internal_call_id,
                            )
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
            },
        }
    }
    Ok(final_text)
}

fn error_chat_history(history: &[Message], new_messages: &[Message]) -> Vec<Message> {
    history.iter().chain(new_messages.iter()).cloned().collect()
}

fn current_rag_text(prompt: &Message, history: &[Message], prior: &[Message]) -> String {
    if let Some(text) = prompt.rag_text() {
        return text;
    }
    history
        .iter()
        .chain(prior.iter())
        .rev()
        .find_map(Message::rag_text)
        .unwrap_or_default()
}

fn value_to_json_string(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(string) => string.clone(),
        other => other.to_string(),
    }
}

async fn dispatch_tool(
    tools: &[Box<dyn ToolDyn>],
    name: &str,
    args: String,
    live_output: Option<crate::background_tools::LiveToolOutputWriter>,
) -> String {
    let Some(tool) = tools.iter().find(|tool| tool.name() == name) else {
        return format!("error: unknown tool '{name}'");
    };

    let Some(scope) = current_tool_runtime_context() else {
        return tool_outcome_to_result(name, tool.call(args).await);
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

    let call = scope_request_tool_execution_with_workspace_and_live_output(
        scope.deadline_at,
        scope.cancellation_token.clone(),
        scope.workspace_cwd.clone(),
        live_output,
        tool.call(args),
    );
    tokio::select! {
        biased;
        _ = scope.cancellation_token.cancelled() => cancelled_result(),
        _ = &mut deadline => timeout_result(scope.deadline_at),
        result = call => tool_outcome_to_result(name, result),
    }
}

pub(crate) fn tool_outcome_to_result(name: &str, outcome: Result<String, ToolError>) -> String {
    match outcome {
        Ok(result) => result,
        Err(ToolError::UnparseableArgs { kind, reason }) => {
            tracing::warn!(
                tool = name,
                %kind,
                %reason,
                "tool-call arguments unparseable after repair; notifying model"
            );
            let guidance = match kind {
                UnparseableArgsKind::Truncated => {
                    "the arguments were cut off — your response hit the token limit before the \
                     JSON was complete; re-call the tool with a shorter, complete arguments object"
                }
                UnparseableArgsKind::Malformed => {
                    "the arguments were not valid JSON; re-call the tool with valid JSON \
                     (escape any backslash as \\\\)"
                }
            };
            unparseable_args_result(&format!(
                "tool '{name}' arguments could not be parsed: {guidance}."
            ))
        }
        // Stamp the prefixes `classify_runtime_failure` matches on. Without
        // them the persistence path cannot tell a failed call from a successful
        // one and terminalizes it `completed` (#400/D6 regression).
        //
        // The INNER error is rendered, not the `ToolError` wrapper: a command
        // policy denial arrives as a `ToolCallError` whose payload is the denial
        // JSON, and `parse_command_policy_denial` strips this prefix and parses
        // the remainder. Rendering the wrapper's own "tool call error: " text
        // here would leave non-JSON in front of the payload and silently
        // downgrade every denial to an unstructured failure.
        Err(ToolError::JsonError(error)) => format!("{JSON_ERROR_PREFIX} {error}"),
        Err(ToolError::ToolCallError(error)) => format!("{TOOL_CALL_ERROR_PREFIX} {error}"),
    }
}

async fn build_request<M: CompletionModel>(
    model: &M,
    prompt: Message,
    history: &[Message],
    prior: &[Message],
    tools: &[Box<dyn ToolDyn>],
    config: &LoopConfig,
) -> Result<CompletionRequest, StreamingError> {
    let rag_text = current_rag_text(&prompt, history, prior);
    let mut tool_defs = Vec::with_capacity(tools.len());
    for tool in tools {
        let native = tool.definition(rag_text.clone()).await;
        tool_defs.push(crate::llm::rig_compat::to_rig_tool_definition(&native));
    }

    let chat_history: Vec<rig::completion::Message> = config
        .preamble
        .as_ref()
        .map(|preamble| rig::completion::Message::system(preamble.clone()))
        .into_iter()
        .chain(history.iter().map(rig_compat::to_rig_message))
        .chain(prior.iter().map(rig_compat::to_rig_message))
        .collect();

    let mut builder = model
        .completion_request(rig_compat::to_rig_message(&prompt))
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
