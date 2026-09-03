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
#[derive(Debug, thiserror::Error)]
#[error("provider_input_repair_removed_complete_prompt")]
struct ProviderInputRepairError;

pub(crate) fn repair_provider_input(
    history: &mut Vec<Message>,
    new_messages: &mut Vec<Message>,
) -> Result<(), StreamingError> {
    // A restored checkpoint may split one closed tool-call/result pair across
    // rig's history and prompt carriers. Repair and sanitize the canonical
    // joined projection, then split only the final prompt back out.
    let mut provider_messages = std::mem::take(history);
    provider_messages.append(new_messages);
    repair_messages(&mut provider_messages);
    let mut provider_messages = crate::compaction::sanitize_history_for_provider(provider_messages);
    let prompt = provider_messages.pop().ok_or_else(|| {
        StreamingError::Completion(CompletionError::RequestError(Box::new(
            ProviderInputRepairError,
        )))
    })?;
    *history = provider_messages;
    new_messages.push(prompt);
    Ok(())
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

/// Project the complete request through the selected provider wire DTO. Both
/// mid-turn `Repair` rebuild paths recompute this projection before dispatch so
/// the clamp and persisted accounting describe the actual repaired wire shape.
pub(crate) fn completion_request_input_components(
    request: &CompletionRequest,
    counter: &crate::provider_input::ProviderInputCounter,
) -> Result<crate::provider_input::ProviderInputProjection, StreamingError> {
    counter.project_request(request).map_err(|error| {
        StreamingError::Completion(CompletionError::ProviderError(format!(
            "provider_input_projection_failed: {error:#}"
        )))
    })
}

fn completion_request_input_tokens(
    request: &CompletionRequest,
    counter: &crate::provider_input::ProviderInputCounter,
) -> Result<usize, StreamingError> {
    counter.estimate_request(request).map_err(|error| {
        StreamingError::Completion(CompletionError::ProviderError(format!(
            "provider_input_projection_failed: {error:#}"
        )))
    })
}

/// One immutable provider attempt assembled, projected, clamped, and admitted
/// as a unit. Capture and transport receive clones of this same request, so an
/// estimate can never be paired with a different rebuilt request.
#[derive(Clone, Debug)]
pub(super) struct PreparedDispatch {
    request: CompletionRequest,
    projection: crate::provider_input::ProviderInputProjection,
}

impl PreparedDispatch {
    pub(super) fn request(&self) -> &CompletionRequest {
        &self.request
    }

    pub(super) fn projection(&self) -> &crate::provider_input::ProviderInputProjection {
        &self.projection
    }
}

/// Sole preparation path for an actual provider attempt. Every retry starts
/// from the unclamped assembled request and repeats the complete projection and
/// both budget gates before capture or transport.
pub(super) fn prepare_dispatch_attempt(
    assembled_request: &CompletionRequest,
    config: &LoopConfig,
    aggregate_token_budget: Option<&AggregateTokenBudget>,
) -> Result<PreparedDispatch, StreamingError> {
    let mut request = assembled_request.clone();
    let input_tokens =
        completion_request_input_tokens(&request, config.provider_input_counter.as_ref())?;
    clamp_request_output_budget(&mut request, config, input_tokens);
    ensure_context_can_dispatch(&request, config, input_tokens)?;
    super::aggregate_budget::clamp_request_aggregate_token_budget(
        &mut request,
        aggregate_token_budget,
        input_tokens,
    )?;
    // Store the projection of the exact post-clamp snapshot that capture and
    // transport receive. Output-limit fields are excluded from input
    // accounting, so this must retain the scalar used by both guards.
    let projection =
        completion_request_input_components(&request, config.provider_input_counter.as_ref())?;
    debug_assert_eq!(projection.estimated_input_tokens, input_tokens);
    Ok(PreparedDispatch {
        request,
        projection,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TurnContextDecision {
    pub(super) reason: ContextCompactionReason,
    pub(super) pre_compaction_input_tokens: Option<usize>,
}

pub(super) fn context_accounting_for_request(
    dispatch: &PreparedDispatch,
    config: &LoopConfig,
    turn_index: usize,
    attempt: u32,
    compaction_reason: ContextCompactionReason,
    pre_compaction_input_tokens: Option<usize>,
) -> ContextAccounting {
    let request = dispatch.request();
    let projection = dispatch.projection();
    let estimated_input_tokens = projection.estimated_input_tokens;
    ContextAccounting {
        accounting_version: CONTEXT_ACCOUNTING_VERSION,
        turn_index,
        attempt,
        estimator: projection.estimator.to_string(),
        components: projection.components.clone(),
        estimated_input_tokens,
        context_window: config.context_window,
        compaction_threshold_basis_points: crate::provider_input::budget::threshold_basis_points(
            config.compaction_threshold,
        ),
        compaction_threshold_tokens: crate::provider_input::budget::threshold_budget(
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
pub(crate) fn clamp_request_output_budget(
    request: &mut CompletionRequest,
    config: &LoopConfig,
    input_tokens: usize,
) {
    // `None` historically delegated the output limit to the provider. At the
    // owned dispatch boundary that cannot establish a context-fit invariant,
    // so make the remaining context explicit. Production behavior configs
    // normally carry `Some`; this preserves the unset compatibility surface
    // while still reserving positive output locally.
    let configured_max =
        crate::provider_input::budget::configured_output_ceiling(request.max_tokens);
    let effective_max = crate::provider_input::budget::effective_output_budget(
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
    request.max_tokens = Some(
        u64::try_from(effective_max)
            .expect("a usize provider output ceiling is representable as u64"),
    );
}

/// Final context-window legality guard. This belongs after reconstruction and
/// recount but before capture: an input at/above context or a configured zero
/// output ceiling is locally non-dispatchable, never clamped to one.
pub(crate) fn ensure_context_can_dispatch(
    request: &CompletionRequest,
    config: &LoopConfig,
    input_tokens: usize,
) -> Result<(), StreamingError> {
    let configured_max =
        crate::provider_input::budget::configured_output_ceiling(request.max_tokens);
    if crate::provider_input::budget::can_dispatch(
        input_tokens,
        config.context_window,
        configured_max,
    ) {
        return Ok(());
    }
    Err(StreamingError::Completion(CompletionError::RequestError(
        Box::new(
            crate::provider_input::budget::ContextBudgetError::NoOutputCapacity {
                estimated_input_tokens: input_tokens,
                context_window: config.context_window,
                effective_max_output_tokens: configured_max,
            },
        ),
    )))
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
    let request = build_request(model, current_prompt, history, prior, tools, config).await?;
    let projection =
        completion_request_input_components(&request, config.provider_input_counter.as_ref())?;
    let before_tokens = projection.estimated_input_tokens;

    let Some(compactor) = config.turn_compactor.as_ref() else {
        return Ok((
            request,
            TurnContextDecision {
                reason: ContextCompactionReason::CompactorUnavailable,
                pre_compaction_input_tokens: None,
            },
        ));
    };
    let Some(admission) = crate::compaction::ReductionAdmission::for_input(
        before_tokens,
        config.context_window,
        config.compaction_threshold,
    ) else {
        return Ok((
            request,
            TurnContextDecision {
                reason: ContextCompactionReason::BelowThreshold,
                pre_compaction_input_tokens: None,
            },
        ));
    };
    let provider_messages = history
        .iter()
        .chain(new_messages.iter())
        .cloned()
        .collect::<Vec<_>>();
    let outcome = compactor(TurnCompactionRequest {
        messages: provider_messages,
        admission,
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
    let (mut compacted, reduction_key, reason) = match outcome {
        TurnCompactionOutcome::ProviderViewRepaired { messages } => (
            messages,
            None,
            ContextCompactionReason::ProviderViewRepaired,
        ),
        TurnCompactionOutcome::Reduced {
            messages,
            reduction_key,
        } => (
            messages,
            Some(reduction_key),
            ContextCompactionReason::Compacted,
        ),
        TurnCompactionOutcome::CannotFit => {
            return Err(StreamingError::Completion(CompletionError::RequestError(
                Box::new(crate::compaction::ReductionError::CannotFit),
            )))
        }
    };
    let compacted_prompt = compacted.pop().ok_or_else(|| {
        StreamingError::Completion(CompletionError::ProviderError(
            "per-turn provider-input compaction returned no prompt".to_string(),
        ))
    })?;
    *history = compacted;
    *new_messages = vec![compacted_prompt.clone()];
    if let Some(reduction_key) = reduction_key {
        reduction_chain_keys.push(reduction_key.clone());
        active_reduction_keys.clear();
        active_reduction_keys.push(reduction_key);
    }

    let rebuilt = build_request(model, compacted_prompt, history, &[], tools, config).await?;
    let rebuilt_projection =
        completion_request_input_components(&rebuilt, config.provider_input_counter.as_ref())?;
    let after_tokens = rebuilt_projection.estimated_input_tokens;
    let effective_input_budget = crate::provider_input::budget::effective_input_budget(
        config.context_window,
        config.compaction_threshold,
    );
    tracing::info!(
        target: "gents::agent::loop_stream",
        turn = turn_index,
        before_tokens,
        after_tokens,
        effective_input_budget,
        context_window = config.context_window,
        max_output_tokens = config.max_tokens.unwrap_or_default(),
        "compacted provider input before completion dispatch"
    );

    let rebuilt_can_dispatch = crate::provider_input::budget::can_dispatch(
        after_tokens,
        config.context_window,
        crate::provider_input::budget::configured_output_ceiling(rebuilt.max_tokens),
    );
    // Preserve the threshold diagnostic for a fitting-but-over-policy result.
    // A non-dispatchable result continues to the owned loop so its sole final
    // legality choke point returns the typed error before capture or send.
    if rebuilt_can_dispatch
        && crate::compaction::ReductionAdmission::for_input(
            after_tokens,
            config.context_window,
            config.compaction_threshold,
        )
        .is_some()
    {
        tracing::warn!(
            estimated_input_tokens = after_tokens,
            effective_input_budget,
            "provider input remains over threshold after reduction"
        );
        return Err(StreamingError::Completion(CompletionError::RequestError(
            Box::new(crate::compaction::ReductionError::CannotFit),
        )));
    }

    Ok((
        rebuilt,
        TurnContextDecision {
            reason,
            pre_compaction_input_tokens: Some(before_tokens),
        },
    ))
}

/// Apply the one lossy repair and rebuild the complete assembled request. The
/// returned request is deliberately not projected or clamped here; the next
/// provider-attempt iteration must pass through `prepare_dispatch_attempt`.
pub(super) async fn repair_and_rebuild_request<M: CompletionModel>(
    model: &M,
    history: &mut Vec<Message>,
    new_messages: &mut Vec<Message>,
    tools: &[Box<dyn ToolDyn>],
    config: &LoopConfig,
) -> Result<CompletionRequest, StreamingError> {
    repair_provider_input(history, new_messages)?;
    let repaired_prompt = new_messages
        .last()
        .cloned()
        .expect("successful repair restores one prompt");
    let repaired_prior = &new_messages[..new_messages.len() - 1];
    build_request(
        model,
        repaired_prompt,
        history,
        repaired_prior,
        tools,
        config,
    )
    .await
}
