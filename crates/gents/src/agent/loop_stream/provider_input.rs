use super::*;

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

/// Repair the assembled provider input, including loaded history and
/// run-threaded messages.
///
/// This runs only after the provider has already REJECTED the request (the
/// completion-retry `Repair` directive). It is deliberately more aggressive
/// than the egress normalizer: on top of the shape coercion it runs a LOSSY
/// leaf sanitizer over every JSON string in a tool call's arguments. That
/// lossiness is exactly why it cannot live at egress — it would corrupt
/// legitimate multi-line tool arguments on every request.
///
/// Repairing history is licensed by `PromptAssembly.repair_is_payload_only`
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

fn serialized_token_estimate<T: serde::Serialize + ?Sized>(value: &T) -> usize {
    serde_json::to_string(value)
        .map(|json| crate::compaction::estimate_tokens(&json))
        .unwrap_or_default()
}

/// Estimate the complete provider input represented by Rig's rendered request,
/// including static tool schemas. The production profile deliberately leaves
/// tokenizer headroom because this estimator is approximate; its job here is to
/// apply that conservative profile to every turn's assembled request, in
/// `build_budgeted_request`. Both mid-turn `Repair` rebuild paths recompute these
/// components before dispatch so the clamp and persisted accounting always
/// describe the repaired request rather than the rejected one.
pub(super) fn completion_request_input_components(
    request: &CompletionRequest,
) -> ContextInputComponents {
    ContextInputComponents {
        messages: serialized_token_estimate(&request.chat_history),
        documents: serialized_token_estimate(&request.documents),
        tool_schemas: serialized_token_estimate(&request.tools),
        additional_parameters: serialized_token_estimate(&request.additional_params),
        output_schema: serialized_token_estimate(&request.output_schema),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TurnContextDecision {
    pub(super) reason: ContextCompactionReason,
    pub(super) pre_compaction_input_tokens: Option<usize>,
    pub(super) components: ContextInputComponents,
}

pub(super) fn context_accounting_for_request(
    request: &CompletionRequest,
    config: &LoopConfig,
    components: &ContextInputComponents,
    turn_index: usize,
    attempt: u32,
    compaction_reason: ContextCompactionReason,
    pre_compaction_input_tokens: Option<usize>,
) -> ContextAccounting {
    let estimated_input_tokens = components.estimated_input_tokens();
    ContextAccounting {
        accounting_version: CONTEXT_ACCOUNTING_VERSION,
        turn_index,
        attempt,
        estimator: "serialized_json_bytes_div_4_v1".to_string(),
        components: components.clone(),
        estimated_input_tokens,
        context_window: config.context_window,
        compaction_threshold_basis_points: crate::compaction::threshold_basis_points(
            config.compaction_threshold,
        ),
        compaction_threshold_tokens: crate::compaction::threshold_budget(
            config.context_window,
            config.compaction_threshold,
        ),
        configured_max_output_tokens: config.max_tokens,
        effective_max_output_tokens: request.max_tokens,
        compaction_reason,
        pre_compaction_input_tokens,
    }
}

/// Treat the configured output value as a ceiling and fit each completion to
/// the context remaining after its fully assembled provider input. Compaction
/// protects the configured input threshold; this clamp independently preserves
/// `input + output <= context` on every dispatch.
pub(super) fn clamp_request_output_budget(
    request: &mut CompletionRequest,
    config: &LoopConfig,
    input_tokens: usize,
) {
    let Some(configured_max) = request.max_tokens else {
        return;
    };
    let configured_max = usize::try_from(configured_max).unwrap_or(usize::MAX);
    let effective_max = crate::compaction::effective_output_budget(
        input_tokens,
        config.context_window,
        configured_max,
    );
    if effective_max < configured_max {
        tracing::debug!(
            target: "gents::agent::loop_stream",
            input_tokens,
            context_window = config.context_window,
            configured_max_output_tokens = configured_max,
            effective_max_output_tokens = effective_max,
            "clamped completion output to remaining provider context"
        );
    }
    request.max_tokens = u64::try_from(effective_max).ok();
}

fn compactable_message_estimate(messages: &[Message]) -> usize {
    let rig_messages = messages
        .iter()
        .map(rig_compat::to_rig_message)
        .collect::<Vec<_>>();
    serialized_token_estimate(&rig_messages)
}

/// Keep enough room for both the non-compactable request layers (preamble,
/// tool schemas, provider parameters) and the summary inserted by the
/// compactor. The post-compaction dispatch guard remains authoritative if a
/// pathological summary or a single oversized current prompt still does not
/// fit.
fn turn_keep_recent_target(
    total_input: usize,
    provider_messages: &[Message],
    config: &LoopConfig,
) -> usize {
    let effective_budget = crate::compaction::effective_input_budget(
        config.context_window,
        config
            .max_tokens
            .and_then(|tokens| usize::try_from(tokens).ok())
            .unwrap_or_default(),
        config.compaction_threshold,
    );
    let compactable_input = compactable_message_estimate(provider_messages);
    let static_input = total_input.saturating_sub(compactable_input);
    let message_budget = effective_budget.saturating_sub(static_input);

    // Summaries vary with the model and history. Reserve one quarter of the
    // compactable-message budget for the summary and serialization drift.
    message_budget.saturating_mul(3) / 4
}

fn completion_request_exceeds_budget(input_tokens: usize, config: &LoopConfig) -> bool {
    let max_output_tokens = config
        .max_tokens
        .and_then(|tokens| usize::try_from(tokens).ok())
        .unwrap_or_default();
    crate::compaction::input_exceeds_budget(
        input_tokens,
        config.context_window,
        max_output_tokens,
        config.compaction_threshold,
    )
}

pub(super) async fn build_budgeted_request<M: CompletionModel>(
    model: &M,
    history: &mut Vec<Message>,
    new_messages: &mut Vec<Message>,
    tools: &[Box<dyn ToolDyn>],
    config: &LoopConfig,
    turn_index: usize,
    reduction_chain_keys: &mut Vec<String>,
    active_reduction_keys: &mut Vec<String>,
) -> Result<(CompletionRequest, TurnContextDecision), StreamingError> {
    let current_prompt = new_messages
        .last()
        .cloned()
        .expect("new_messages always retains at least the initial prompt");
    let prior = &new_messages[..new_messages.len() - 1];
    let mut request = build_request(model, current_prompt, history, prior, tools, config).await?;
    let components = completion_request_input_components(&request);
    let before_tokens = components.estimated_input_tokens();
    clamp_request_output_budget(&mut request, config, before_tokens);

    let Some(compactor) = config.turn_compactor.as_ref() else {
        return Ok((
            request,
            TurnContextDecision {
                reason: ContextCompactionReason::CompactorUnavailable,
                pre_compaction_input_tokens: None,
                components,
            },
        ));
    };
    if !completion_request_exceeds_budget(before_tokens, config) {
        return Ok((
            request,
            TurnContextDecision {
                reason: ContextCompactionReason::BelowThreshold,
                pre_compaction_input_tokens: None,
                components,
            },
        ));
    }

    let provider_messages = history
        .iter()
        .chain(new_messages.iter())
        .cloned()
        .collect::<Vec<_>>();
    let keep_recent_target = turn_keep_recent_target(before_tokens, &provider_messages, config);
    let outcome = compactor(TurnCompactionRequest {
        messages: provider_messages,
        keep_recent_target,
        turn_index,
        prior_reduction_keys: reduction_chain_keys.clone(),
    })
    .await
    .map_err(|error| {
        aggregate_token_budget_exhaustion_message(&error).map_or_else(
            || {
                StreamingError::Completion(CompletionError::ProviderError(format!(
                    "per-turn provider-input compaction failed: {error:#}"
                )))
            },
            |reason| StreamingError::Completion(CompletionError::ProviderError(reason)),
        )
    })?;
    let mut compacted = outcome.messages;
    let compacted_prompt = compacted.pop().ok_or_else(|| {
        StreamingError::Completion(CompletionError::ProviderError(
            "per-turn provider-input compaction returned no prompt".to_string(),
        ))
    })?;
    *history = compacted;
    *new_messages = vec![compacted_prompt.clone()];
    reduction_chain_keys.push(outcome.reduction_key.clone());
    active_reduction_keys.clear();
    active_reduction_keys.push(outcome.reduction_key);

    let mut rebuilt = build_request(model, compacted_prompt, history, &[], tools, config).await?;
    let rebuilt_components = completion_request_input_components(&rebuilt);
    let after_tokens = rebuilt_components.estimated_input_tokens();
    clamp_request_output_budget(&mut rebuilt, config, after_tokens);
    tracing::info!(
        target: "gents::agent::loop_stream",
        turn = turn_index,
        before_tokens,
        after_tokens,
        keep_recent_target,
        context_window = config.context_window,
        max_output_tokens = config.max_tokens.unwrap_or_default(),
        "compacted provider input before completion dispatch"
    );

    if completion_request_exceeds_budget(after_tokens, config) {
        let effective_budget = crate::compaction::effective_input_budget(
            config.context_window,
            config
                .max_tokens
                .and_then(|tokens| usize::try_from(tokens).ok())
                .unwrap_or_default(),
            config.compaction_threshold,
        );
        return Err(StreamingError::Completion(CompletionError::ProviderError(
            format!(
                "per-turn provider input remains over budget after compaction: \
                 estimated_input_tokens={after_tokens}, effective_input_budget={effective_budget}"
            ),
        )));
    }

    Ok((
        rebuilt,
        TurnContextDecision {
            reason: ContextCompactionReason::Compacted,
            pre_compaction_input_tokens: Some(before_tokens),
            components: rebuilt_components,
        },
    ))
}
