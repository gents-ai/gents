//! Owned multi-turn completion and tool-execution loop.
//!
//! This module owns provider-request assembly, retry and retract decisions,
//! streamed-turn accumulation, tool dispatch, and message threading. Durable
//! lifecycle effects remain hook-owned; rig remains the provider client and
//! streaming decoder at the boundary.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;

use crate::agent::completion_retry::{
    CompletionRetryPolicy, CompletionRetryState, MidStreamDirective, PreStreamDirective,
};
use crate::error::InferenceError;
use crate::llm::message::{
    AssistantContent, Message, ToolCall, ToolResult, ToolResultContent, UserContent,
};
use crate::llm::rig_compat;
use crate::llm::{HookAction, ToolCallHookAction};
use crate::rendered_request::{
    AssemblyBuildPath, AssemblyTrace, ContextAccounting, ContextCompactionReason,
    CONTEXT_ACCOUNTING_VERSION,
};
use async_stream::try_stream;
use futures::{Stream, StreamExt};
use rig::agent::{MultiTurnStreamItem, StreamingError};
use rig::completion::{
    CompletionError, CompletionModel, CompletionRequest, GetTokenUsage, PromptError, Usage,
};

use crate::llm::tool::ToolDyn;
use crate::llm::ToolChoice;
use rig::streaming::{StreamedAssistantContent, StreamedUserContent};

use super::stream_processor::AssistantTurnAccumulator;
use crate::agent::output_obligation::OutputObligationGate;
use crate::hook::DefraSessionHook;
use crate::tool_call_lifecycle::runtime::{
    current_tool_runtime_context, deadline_remaining, scope_request_tool_execution_with_session,
    ToolOutcome,
};
use crate::truncation::{tool_result_truncation_mode, truncate_text, TruncationLimits};

mod aggregate_budget;
mod contract;
mod one_shot;
mod provider_input;
mod tool_dispatch;
mod turn_threading;

#[allow(unused_imports)]
pub(crate) use contract::TurnCompactor;
pub(crate) use contract::{
    LoopConfig, LoopStreamItem, RenderedRequestSink, StructuredOutputConfig, TurnCompactionOutcome,
    TurnCompactionRequest,
};
pub(crate) use one_shot::{run_loop_to_text, run_loop_to_typed};
pub(crate) use provider_input::{
    assemble_new_messages, is_request_context_message, repair_provider_input,
};
pub(crate) use tool_dispatch::dispatch_tool;

use provider_input::{
    build_budgeted_request, clamp_request_output_budget, completion_request_input_components,
    context_accounting_for_request, ensure_context_can_dispatch,
};
use tool_dispatch::value_to_json_string;
use turn_threading::{add_usage_saturating, close_streaming_turn};
#[cfg(test)]
mod tests;

#[cfg(test)]
use aggregate_budget::AggregateTokenLedger;
use aggregate_budget::{
    aggregate_post_charge_action, clamp_request_aggregate_token_budget, AggregatePostChargeAction,
    AggregateTokenCharge,
};
pub(crate) use aggregate_budget::{
    aggregate_token_budget_exhaustion_message, AggregateTokenBudget,
    AGGREGATE_TOKEN_BUDGET_EXHAUSTED_PREFIX,
};

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
        // A recovered durable checkpoint is one exact provider projection even
        // though rig's loop API carries its final message separately as the
        // prompt. Sanitize the joined projection before splitting it again so
        // a tool call in history remains paired with a tool result prompt.
        let mut entry_projection = history;
        entry_projection.push(prompt);
        let mut entry_projection =
            crate::compaction::sanitize_history_for_provider(entry_projection);
        let prompt = entry_projection.pop().ok_or_else(|| {
            StreamingError::Completion(CompletionError::ProviderError(
                "provider-bound loop entry has no prompt after sanitization".to_string(),
            ))
        })?;
        let history = entry_projection;
        // Prior requests' per-request context rows must not re-enter provider
        // history. The current context is assembled into `new_messages`.
        // Repair may rewrite both vectors in place after provider rejection.
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
        let aggregate_token_budget = config.aggregate_token_budget.clone();
        let mut current_turn: usize = config.initial_turn_index;
        let mut retry = CompletionRetryState::new(config.retry_policy.clone());
        // Retain the effective native message list whenever request-local
        // context, reduction, or repair can make transcript reconstruction
        // differ from the provider input.
        let mut retain_effective_messages_oracle =
            config.context_message.is_some() || !config.active_reduction_keys.is_empty();
        let mut active_reduction_keys = config.active_reduction_keys.clone();
        let mut reduction_chain_keys = config.reduction_chain_keys.clone();

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

            let turn_index = current_turn - 1;
            let (mut request, turn_context_decision) = build_budgeted_request(
                &model,
                &mut history,
                &mut new_messages,
                tools.as_slice(),
                &config,
                turn_index,
                &mut reduction_chain_keys,
                &mut active_reduction_keys,
            )
            .await?;
            let compaction_reason = turn_context_decision.reason;
            let pre_compaction_input_tokens =
                turn_context_decision.pre_compaction_input_tokens;
            let mut provider_input_projection = turn_context_decision.projection;
            retain_effective_messages_oracle |= matches!(
                compaction_reason,
                ContextCompactionReason::Compacted
            );

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

            let mut attempt = 0_u32;
            // Repair bypasses budgeted construction, although both dispatch
            // clamps are reapplied below. The build path is not recoverable
            // from the transcript, so it rides in the trace.
            let mut build_path = AssemblyBuildPath::Budgeted;
            'attempts: loop {
                let mut stream = loop {
                    // Repair and retry paths can rebuild or reuse the request.
                    // Re-apply both clamps at the one provider-dispatch
                    // chokepoint so no attempt escapes either budget.
                    clamp_request_output_budget(
                        &mut request,
                        &config,
                        provider_input_projection.estimated_input_tokens,
                    );
                    ensure_context_can_dispatch(
                        &request,
                        &config,
                        provider_input_projection.estimated_input_tokens,
                    )?;
                    clamp_request_aggregate_token_budget(
                        &mut request,
                        aggregate_token_budget.as_ref(),
                        provider_input_projection.estimated_input_tokens,
                    )?;
                    if let Some(on_rendered_request) = config.on_rendered_request.as_ref() {
                        // `history ++ new_messages` is the effective provider
                        // message list: post sanitization, post request-context
                        // filtering, and post any per-turn compaction (which
                        // rewrote both vectors in place).
                        let effective_messages =
                            history.iter().chain(new_messages.iter()).cloned().collect();
                        let assembly_trace = if retain_effective_messages_oracle {
                            AssemblyTrace::from_effective_messages(build_path, effective_messages)
                        } else {
                            AssemblyTrace::from_reconstructible_messages(
                                build_path,
                                effective_messages,
                            )
                        }
                        .with_reduction_keys(active_reduction_keys.clone())
                        .with_context_accounting(context_accounting_for_request(
                            &request,
                            &config,
                            &provider_input_projection,
                            turn_index,
                            attempt,
                            compaction_reason,
                            pre_compaction_input_tokens,
                        ));
                        on_rendered_request(turn_index, attempt, request.clone(), assembly_trace)
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
                                    // Repair bypasses compaction; dispatch
                                    // reapplies both budget clamps.
                                    request = build_request(
                                        &model,
                                        repaired_prompt,
                                        &history,
                                        repaired_prior,
                                        tools.as_slice(),
                                        &config,
                                    )
                                    .await?;
                                    provider_input_projection =
                                        completion_request_input_components(
                                            &request,
                                            config.provider_input_counter.as_ref(),
                                        )?;
                                    build_path = AssemblyBuildPath::Repair;
                                    // Repair rewrites `history` and `new_messages` in place, and
                                    // both are declared outside `'turns`. The durable transcript is
                                    // never rewritten to match, so every later turn is assembled
                                    // from messages no `AgentMessage` row reproduces. Mark the
                                    // effective list ephemeral for the rest of the request, not just
                                    // this turn — `build_path` resets per turn and would otherwise
                                    // report `Budgeted` for a turn whose input repair had altered.
                                    retain_effective_messages_oracle = true;
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
            let mut saw_final_usage_event = false;
            let mut aggregate_budget_exhausted = false;
            let mut aggregate_usage_failure = None::<String>;

            while let Some(item) = stream.next().await {
                let item = match item {
                    Ok(item) => {
                        if !saw_stream_item {
                            // A provider response arrived while this attempt's
                            // capture was still waiting to be claimed, which
                            // means the send did not travel through the
                            // capturing transport. That is a mis-wired client
                            // stack, and the only honest response is to stop:
                            // silently continuing would produce a turn whose
                            // provider input is not durable anywhere.
                            ensure_rendered_request_was_captured(turn_index, attempt)?;
                        }
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
                                    // Repair bypasses compaction; dispatch
                                    // reapplies both budget clamps.
                                    request = build_request(
                                        &model,
                                        repaired_prompt,
                                        &history,
                                        repaired_prior,
                                        tools.as_slice(),
                                        &config,
                                    )
                                    .await?;
                                    provider_input_projection =
                                        completion_request_input_components(
                                            &request,
                                            config.provider_input_counter.as_ref(),
                                        )?;
                                    build_path = AssemblyBuildPath::Repair;
                                    // Repair mutates the request-scoped vectors,
                                    // so retain the effective list for later turns.
                                    retain_effective_messages_oracle = true;
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
                        if let Some(budget) = aggregate_token_budget.as_ref() {
                            let ledger = budget.snapshot()?;
                            Err(StreamingError::Completion(
                                CompletionError::ProviderError(format!(
                                    "aggregate_token_usage_missing: limit={}, used={}; \
                                     provider stream failed after emitting content without a \
                                     final usage event",
                                    ledger.limit, ledger.used,
                                )),
                            ))?;
                            unreachable!("Err(..)? above ends the stream");
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
                        if let Some(budget) = aggregate_token_budget.as_ref() {
                            for item in close_streaming_turn(
                                &mut new_messages,
                                &mut accumulator,
                                stream.message_id.clone(),
                                pending_results,
                            ) {
                                yield item;
                            }
                            let ledger = budget.snapshot()?;
                            Err(StreamingError::Completion(
                                CompletionError::ProviderError(format!(
                                    "aggregate_token_usage_missing: limit={}, used={}; \
                                     provider stream failed after tool effects without a final \
                                     usage event",
                                    ledger.limit, ledger.used,
                                )),
                            ))?;
                            unreachable!("Err(..)? above ends the stream");
                        }
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
                                let live_output = match hook.as_ref() {
                                    Some(hook) => Some(
                                        hook.foreground_live_output_writer(&internal_call_id)
                                            .await,
                                    ),
                                    None => None,
                                };
                                let session_id = match hook.as_ref() {
                                    Some(hook) => hook.session_id().await,
                                    None => None,
                                };
                                let outcome = dispatch_tool(
                                    tools.as_slice(),
                                    &tool_name,
                                    tool_args.clone(),
                                    live_output,
                                    session_id,
                                )
                                .await;

                                if let Some(hook) = hook.as_ref() {
                                    let result_action = hook
                                        .on_tool_result(
                                            &tool_name,
                                            tool_call.call_id.clone(),
                                            &internal_call_id,
                                            &tool_args,
                                            &outcome,
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
                                // The typed outcome's model-facing accessor is
                                // the only text that may thread to the model.
                                let (bounded, _, _) = truncate_text(
                                    outcome.model_facing_text(),
                                    tool_result_truncation_mode(&tool_name),
                                    &TruncationLimits::default(),
                                );
                                bounded
                            }
                        };

                        pending_results.push((rig_compat::from_rig_tool_call(&tool_call), internal_call_id, bounded_result));
                    }
                    StreamedAssistantContent::ToolCallDelta { .. } => {
                    }
                    StreamedAssistantContent::Final(raw) => {
                        saw_final_usage_event = true;
                        let usage = raw.token_usage();
                        if let Some(usage) = usage {
                            add_usage_saturating(&mut aggregated_usage, usage);
                        }
                        if let Some(budget) = aggregate_token_budget.as_ref() {
                            let (charge, ledger) = budget.charge_reported(usage)?;
                            match charge {
                                AggregateTokenCharge::Missing => {
                                    aggregate_usage_failure = Some(format!(
                                        "aggregate_token_usage_missing: limit={}, used={}; \
                                         provider completed without a non-zero usage report",
                                        ledger.limit, ledger.used,
                                    ));
                                }
                                AggregateTokenCharge::Within => {}
                                AggregateTokenCharge::Exhausted => {
                                    aggregate_budget_exhausted = true;
                                }
                                AggregateTokenCharge::Overrun => {
                                    aggregate_usage_failure = Some(format!(
                                        "aggregate_token_budget_overrun: limit={}, observed_used={}",
                                        ledger.limit, ledger.used,
                                    ));
                                }
                            }
                        }
                    }
                }
            }

            // An empty stream never enters the item branch above. It is still
            // proof that `model.stream` dispatched, so it must not reach the
            // ordinary no-output retry path while this attempt remains armed.
            if !saw_stream_item {
                ensure_rendered_request_was_captured(turn_index, attempt)?;
            }

            if aggregate_token_budget.is_some() && !saw_final_usage_event {
                let ledger = aggregate_token_budget
                    .as_ref()
                    .expect("configured aggregate token budget remains present")
                    .snapshot()?;
                aggregate_usage_failure = Some(format!(
                    "aggregate_token_usage_missing: limit={}, used={}; \
                     provider stream ended without a final usage event",
                    ledger.limit, ledger.used,
                ));
            }

            if let Some(reason) = aggregate_usage_failure {
                for item in close_streaming_turn(
                    &mut new_messages,
                    &mut accumulator,
                    stream.message_id.clone(),
                    pending_results,
                ) {
                    yield item;
                }
                Err(StreamingError::Completion(CompletionError::ProviderError(reason)))?;
                unreachable!("Err(..)? above ends the stream");
            }

            let structured_output_error = if pending_results.is_empty() {
                config
                    .structured_output
                    .as_ref()
                    .and_then(|output| (output.validate)(&turn_text).err())
            } else {
                None
            };
            let terminal_valid = pending_results.is_empty()
                && !turn_text.trim().is_empty()
                && structured_output_error.is_none();
            if aggregate_budget_exhausted
                && aggregate_post_charge_action(
                    AggregateTokenCharge::Exhausted,
                    terminal_valid,
                )
                    == AggregatePostChargeAction::Fail
            {
                for item in close_streaming_turn(
                    &mut new_messages,
                    &mut accumulator,
                    stream.message_id.clone(),
                    pending_results,
                ) {
                    yield item;
                }
                let ledger = aggregate_token_budget
                    .as_ref()
                    .expect("exhaustion requires a configured aggregate token budget")
                    .snapshot()?;
                let contract_detail = structured_output_error
                    .as_deref()
                    .map(|error| format!("; terminal output did not satisfy the structured contract: {error}"))
                    .unwrap_or_default();
                Err(StreamingError::Completion(CompletionError::ProviderError(format!(
                    "{AGGREGATE_TOKEN_BUDGET_EXHAUSTED_PREFIX}limit={}, used={} after provider call{}",
                    ledger.limit, ledger.used, contract_detail,
                ))))?;
                unreachable!("Err(..)? above ends the stream");
            }

            if pending_results.is_empty() && turn_text.trim().is_empty() {
                // A reasoning-only or empty terminal response is unusable.
                // With no tool effect to replay, use the proven no-effect
                // retract transition and resample the same request.
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
                            "retracting completion turn with no visible output"
                        );
                        tokio::time::sleep(delay).await;
                        attempt += 1;
                        continue 'attempts;
                    }
                    MidStreamDirective::CloseAndContinue { .. } => {
                        unreachable!(
                            "no-output completion without tool effects cannot close and continue"
                        );
                    }
                    MidStreamDirective::Fail { reason } => {
                        Err(StreamingError::Completion(
                            CompletionError::ProviderError(format!(
                                "completion produced no visible output: {reason}; \
                                 raw_output_preview=\"\"; \
                                 finish_metadata=unavailable_at_rig_streaming_boundary"
                            )),
                        ))?;
                        unreachable!("Err(..)? above ends the stream");
                    }
                }
            }

            if let Some(error) = structured_output_error {
                // The provider completed normally, but the result does not
                // satisfy the typed contract Rig sent. No tool effect has run,
                // so this is the same proven CompletionRetry.retract transition
                // as an interrupted or empty no-effect turn: discard all
                // streamed content and resample the identical request.
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
                            error = %error,
                            "retracting completion turn after structured-output validation failure"
                        );
                        tokio::time::sleep(delay).await;
                        attempt += 1;
                        continue 'attempts;
                    }
                    MidStreamDirective::CloseAndContinue { .. } => {
                        unreachable!(
                            "invalid structured output without tool effects cannot close and continue"
                        );
                    }
                    MidStreamDirective::Fail { reason } => {
                        Err(StreamingError::Completion(
                            CompletionError::ProviderError(format!(
                                "structured-output validation failed: {error}; {reason}"
                            )),
                        ))?;
                        unreachable!("Err(..)? above ends the stream");
                    }
                }
            }

            if pending_results.is_empty() {
                if let Some(gate) = config.output_obligation_gate.as_ref() {
                    let unmet = gate.unmet().await.map_err(|error| {
                        StreamingError::Completion(CompletionError::ProviderError(format!(
                            "checking output obligations failed: {error:#}"
                        )))
                    })?;
                    if !unmet.is_empty() {
                        if let Some(mut assistant_message) = accumulator.take_message() {
                            if let Message::Assistant { id, .. } = &mut assistant_message {
                                *id = stream.message_id.clone();
                            }
                            new_messages.push(assistant_message);
                        }
                        let reminder = Message::user(
                            crate::agent::output_obligation::continuation_message(&unmet),
                        );
                        new_messages.push(reminder.clone());
                        yield LoopStreamItem::OutputObligationPending { reminder };
                        continue 'turns;
                    }
                }
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

fn ensure_rendered_request_was_captured(
    turn_index: usize,
    attempt: u32,
) -> Result<(), StreamingError> {
    if crate::rendered_request::scope::pending_is_armed() {
        return Err(StreamingError::Completion(CompletionError::ProviderError(
            format!(
                "provider response for turn {turn_index} attempt {attempt} \
                 arrived without a durable rendered-request capture; the \
                 completion client is missing its capturing transport"
            ),
        )));
    }
    Ok(())
}

pub(crate) async fn build_request<M: CompletionModel>(
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
        .output_schema_opt(
            config
                .structured_output
                .as_ref()
                .map(|output| output.schema.clone()),
        )
        .tools(tool_defs);

    if let Some(tool_choice) = &config.tool_choice {
        builder = builder.tool_choice(crate::llm::rig_compat::to_rig_tool_choice(tool_choice));
    }

    Ok(builder.build())
}
