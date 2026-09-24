use super::*;
use crate::claude_messages_body::{
    prepare_replay_checkpoint, restore_and_narrow_replay, ReplayCheckpointError, ReplayTag,
    TaggedAssistantRow,
};
use crate::provider_input::ProviderInputProfile;

pub(super) fn message_values(rows: &[TaggedMessage]) -> Vec<Message> {
    rows.iter().map(|row| row.message.clone()).collect()
}

/// Translate the checkpoint owner's independently required assistant sources
/// into the reducer's exact provider-view row coordinates. No native payload
/// equality or provider-assigned message ID participates in this association.
pub fn replay_compaction_prefix_bound(
    rows: &[TaggedMessage],
    required: &[ReplayTag],
) -> Result<Option<usize>, ReplayCheckpointError> {
    let mut assistants = Vec::new();
    for row in rows {
        match &row.message {
            Message::Assistant { id, content } => assistants.push(TaggedAssistantRow {
                source: row.source.clone(),
                id: id.clone(),
                content: content.clone(),
            }),
            _ if row.source.is_some() => return Err(ReplayCheckpointError::InvalidAssociation),
            _ => {}
        }
    }
    prepare_replay_checkpoint(required.to_vec(), assistants, 0)?;
    Ok(rows.iter().position(|row| {
        row.source
            .as_ref()
            .is_some_and(|tag| required.contains(tag))
    }))
}

fn replay_input_error(message: impl Into<String>) -> StreamingError {
    StreamingError::Completion(CompletionError::RequestError(Box::new(
        std::io::Error::new(std::io::ErrorKind::InvalidInput, message.into()),
    )))
}

/// Resolution happens outside provider attempt retry handling. Invalid
/// provenance remains typed permanent input failure; infrastructure errors
/// terminate this invocation without being presented as malformed input.
pub(super) fn replay_resolution_error(error: anyhow::Error) -> StreamingError {
    match error.downcast::<ReplayEvidenceViolation>() {
        Ok(violation) => {
            StreamingError::Completion(CompletionError::RequestError(Box::new(violation)))
        }
        Err(error) => StreamingError::Completion(CompletionError::ProviderError(format!(
            "loading canonical Claude replay evidence failed: {error:#}",
        ))),
    }
}

pub(super) async fn resolve_replay_evidence(
    replay: &mut LoopReplayInput,
    requested: &[ReplayTag],
) -> Result<(), StreamingError> {
    let mut missing = Vec::new();
    for tag in requested {
        if replay.resolved.contains(tag) {
            continue;
        }
        if replay.evidence.iter().any(|row| row.tag == *tag) {
            replay.resolved.push(tag.clone());
        } else if !missing.contains(tag) {
            missing.push(tag.clone());
        }
    }
    if missing.is_empty() {
        return Ok(());
    }
    let resolve = replay.resolve.as_ref().ok_or_else(|| {
        replay_input_error("required Claude replay has no canonical evidence resolver")
    })?;
    let evidence = resolve(missing.clone())
        .await
        .map_err(replay_resolution_error)?;
    if evidence.iter().any(|row| !missing.contains(&row.tag)) {
        return Err(replay_input_error(
            "canonical resolver returned an unrequested replay source",
        ));
    }
    replay.evidence.extend(evidence);
    replay.resolved.extend(missing);
    Ok(())
}

#[cfg(test)]
mod replay_error_tests {
    use super::*;

    fn provider_tag(turn_index: u32) -> ReplayTag {
        ReplayTag {
            request_doc_id: "request".into(),
            source: OutputSource::ProviderTurn {
                scope: "inference.1".parse().unwrap(),
                turn_index,
                attempt: 0,
            },
        }
    }

    fn associated_assistant(tag: &ReplayTag) -> TaggedMessage {
        TaggedMessage {
            message: Message::assistant("native assistant row"),
            source: Some(tag.clone()),
        }
    }

    #[test]
    fn replay_prefix_bound_uses_full_provider_view_row_index() {
        let earlier = provider_tag(0);
        let required = provider_tag(1);
        let rows = vec![
            TaggedMessage::unassociated(Message::system("preamble")),
            TaggedMessage::unassociated(Message::user("first")),
            associated_assistant(&earlier),
            TaggedMessage::unassociated(Message::user("second")),
            TaggedMessage::unassociated(Message::assistant("authored history")),
            associated_assistant(&required),
        ];
        assert_eq!(
            replay_compaction_prefix_bound(&rows, &[required]).unwrap(),
            Some(5),
            "the bound is the whole provider-view index, not assistant ordinal 2"
        );
    }

    #[test]
    fn replay_prefix_bound_without_required_sources_is_unbounded() {
        let rows = vec![
            TaggedMessage::unassociated(Message::user("input")),
            associated_assistant(&provider_tag(0)),
        ];
        assert_eq!(replay_compaction_prefix_bound(&rows, &[]), Ok(None));
    }

    #[test]
    fn replay_prefix_bound_reuses_checkpoint_missing_and_duplicate_errors() {
        let required = provider_tag(1);
        let other = provider_tag(0);
        let missing = vec![associated_assistant(&other)];
        assert_eq!(
            replay_compaction_prefix_bound(&missing, &[required.clone()]),
            Err(ReplayCheckpointError::MissingRequired)
        );

        let duplicated = vec![
            associated_assistant(&required),
            associated_assistant(&required),
        ];
        assert_eq!(
            replay_compaction_prefix_bound(&duplicated, &[required]),
            Err(ReplayCheckpointError::DuplicateAssociation)
        );
    }

    #[test]
    fn replay_prefix_bound_rejects_tagged_non_assistant() {
        let required = provider_tag(0);
        let rows = vec![TaggedMessage {
            message: Message::user("input"),
            source: Some(required.clone()),
        }];
        assert_eq!(
            replay_compaction_prefix_bound(&rows, &[required]),
            Err(ReplayCheckpointError::InvalidAssociation)
        );
    }

    #[tokio::test]
    async fn unresolved_sources_are_batched_and_missing_results_stay_missing() {
        let tags = (0..2)
            .map(|turn_index| ReplayTag {
                request_doc_id: "request".into(),
                source: OutputSource::ProviderTurn {
                    scope: "inference.1".parse().unwrap(),
                    turn_index,
                    attempt: 0,
                },
            })
            .collect::<Vec<_>>();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = calls.clone();
        let expected = tags.clone();
        let mut replay = LoopReplayInput {
            resolve: Some(Arc::new(move |requested| {
                assert_eq!(requested, expected);
                observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Box::pin(async { Ok(Vec::new()) })
            })),
            ..LoopReplayInput::default()
        };
        resolve_replay_evidence(&mut replay, &tags).await.unwrap();
        resolve_replay_evidence(&mut replay, &tags).await.unwrap();
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(replay.resolved, tags);
        assert!(replay.evidence.is_empty());
    }

    #[test]
    fn canonical_violation_retains_its_type_through_context() {
        let error = anyhow::Error::new(ReplayEvidenceViolation("invalid canonical join".into()))
            .context("resolving required replay");
        let StreamingError::Completion(CompletionError::RequestError(error)) =
            replay_resolution_error(error)
        else {
            panic!("canonical violation must remain a typed local input failure");
        };
        assert!(error.downcast_ref::<ReplayEvidenceViolation>().is_some());
    }

    #[test]
    fn storage_error_is_not_reclassified_by_message_text() {
        let error = anyhow::anyhow!("canonical replay tag belongs to another physical request");
        assert!(matches!(
            replay_resolution_error(error),
            StreamingError::Completion(CompletionError::ProviderError(_))
        ));
    }
}

fn tagged_from_sourced(
    originals: &[TaggedMessage],
    sourced: Vec<crate::compaction::history::SourcedMessage>,
) -> Result<Vec<TaggedMessage>, StreamingError> {
    if originals
        .iter()
        .any(|row| row.source.is_some() && !matches!(row.message, Message::Assistant { .. }))
    {
        return Err(replay_input_error(
            "canonical provider source was attached to a non-assistant row",
        ));
    }
    let mut previous_source = None;
    sourced
        .into_iter()
        .map(|item| {
            if previous_source.is_some_and(|previous| item.source_index <= previous) {
                return Err(replay_input_error(
                    "provider projection reordered or duplicated a source row",
                ));
            }
            previous_source = Some(item.source_index);
            let original = originals.get(item.source_index).ok_or_else(|| {
                replay_input_error("provider projection emitted an out-of-range source index")
            })?;
            if original.source.is_some() && !matches!(item.message, Message::Assistant { .. }) {
                return Err(replay_input_error(
                    "canonical provider source was attached to a non-assistant row",
                ));
            }
            Ok(TaggedMessage {
                message: item.message,
                source: original.source.clone(),
            })
        })
        .collect()
}

pub fn sanitize_tagged_history(
    profile: ProviderInputProfile,
    rows: Vec<TaggedMessage>,
) -> Result<Vec<TaggedMessage>, StreamingError> {
    let messages = rows.iter().map(|row| row.message.clone()).collect();
    tagged_from_sourced(
        &rows,
        crate::compaction::sanitize_history_with_sources(profile, messages),
    )
}

pub fn provider_view_tagged(
    profile: ProviderInputProfile,
    rows: Vec<TaggedMessage>,
) -> Result<Vec<TaggedMessage>, StreamingError> {
    let messages = rows.iter().map(|row| row.message.clone()).collect();
    let (sourced, _) = crate::compaction::provider_view_with_sources(profile, messages);
    tagged_from_sourced(&rows, sourced)
}

/// Narrow the one actual loop-owned native row list before request assembly.
/// The selected assistant occurrence retains its independently carried tag;
/// expected reasoning is loaded and cached from the canonical owner, never
/// rebuilt from this mutable provider-input list.
pub async fn narrow_tagged_history(
    profile: ProviderInputProfile,
    rows: &mut [TaggedMessage],
    replay: &mut LoopReplayInput,
) -> Result<(), StreamingError> {
    if profile != ProviderInputProfile::ClaudeMessages {
        if !replay.required.is_empty() {
            return Err(replay_input_error(
                "required Claude replay coordinates on a non-Claude provider input",
            ));
        }
        return Ok(());
    }
    resolve_replay_evidence(replay, &replay.required.clone()).await?;

    let assistant_indices = rows
        .iter()
        .enumerate()
        .filter_map(|(index, row)| {
            matches!(row.message, Message::Assistant { .. }).then_some(index)
        })
        .collect::<Vec<_>>();
    let assistant_rows = assistant_indices
        .iter()
        .map(|index| {
            let row = &rows[*index];
            let Message::Assistant { id, content } = &row.message else {
                unreachable!("assistant_indices contains only assistant rows")
            };
            TaggedAssistantRow {
                source: row.source.clone(),
                id: id.clone(),
                content: content.clone(),
            }
        })
        .collect();
    let checkpoint = prepare_replay_checkpoint(replay.required.clone(), assistant_rows, 0)
        .map_err(|error| replay_input_error(format!("Claude replay checkpoint: {error}")))?;
    let narrowed = restore_and_narrow_replay(&checkpoint, |tag| {
        replay
            .evidence
            .iter()
            .filter(|row| row.tag == *tag)
            .map(|row| row.evidence.clone())
            .collect()
    })
    .map_err(|error| replay_input_error(format!("Claude replay narrowing: {error}")))?;
    if narrowed.len() != assistant_indices.len() {
        return Err(replay_input_error(
            "Claude replay narrowing changed assistant row cardinality",
        ));
    }
    for (index, selected) in assistant_indices.into_iter().zip(narrowed) {
        if rows[index].source != selected.source {
            return Err(replay_input_error(
                "Claude replay narrowing changed assistant source association",
            ));
        }
        let Message::Assistant { content, .. } = &mut rows[index].message else {
            unreachable!("assistant_indices contains only assistant rows")
        };
        *content = selected.content;
    }
    Ok(())
}

/// Assemble the per-request message tail: an optional runtime context message
/// rides immediately before the prompt, which is always last for rig.
///
/// The Lean prompt-assembly model deliberately excludes this runtime-only
/// workspace context. Its local ordering is fenced by
/// `assembles_context_immediately_before_prompt`; the generated Lean layer
/// cases exercise the canonical prompt tail without it.
pub fn assemble_new_messages(
    context_message: Option<Message>,
    prompt: TaggedMessage,
) -> Vec<TaggedMessage> {
    let mut new_messages: Vec<TaggedMessage> = Vec::with_capacity(2);
    if let Some(context_message) = context_message {
        new_messages.push(TaggedMessage::unassociated(context_message));
    }
    new_messages.push(prompt);
    new_messages
}

pub fn is_request_context_message(message: &Message) -> bool {
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

pub fn repair_provider_input(
    profile: crate::provider_input::ProviderInputProfile,
    history: &mut Vec<TaggedMessage>,
    new_messages: &mut Vec<TaggedMessage>,
) -> Result<(), StreamingError> {
    // A restored checkpoint may split one closed tool-call/result pair across
    // rig's history and prompt carriers. Repair and sanitize the canonical
    // joined projection, then split only the final prompt back out.
    let mut provider_messages = std::mem::take(history);
    provider_messages.append(new_messages);
    repair_messages(&mut provider_messages);
    let mut provider_messages = sanitize_tagged_history(profile, provider_messages)?;
    let prompt = provider_messages.pop().ok_or_else(|| {
        StreamingError::Completion(CompletionError::RequestError(Box::new(
            ProviderInputRepairError,
        )))
    })?;
    *history = provider_messages;
    new_messages.push(prompt);
    Ok(())
}

fn repair_messages(messages: &mut [TaggedMessage]) {
    for row in messages.iter_mut() {
        let Message::Assistant { content, .. } = &mut row.message else {
            continue;
        };
        for item in content {
            let AssistantContent::ToolCall(tool_call) = item else {
                continue;
            };
            let mut repaired = crate::tool::normalize_tool_call_arguments(
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
pub fn completion_request_input_components(
    request: &CompletionRequest,
    counter: &crate::provider_input::ProviderInputCounter,
) -> Result<crate::provider_input::ProviderInputProjection, StreamingError> {
    counter.project_request(request).map_err(|error| {
        StreamingError::Completion(CompletionError::RequestError(Box::new(
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("provider_input_projection_failed: {error:#}"),
            ),
        )))
    })
}

fn completion_request_input_tokens(
    request: &CompletionRequest,
    counter: &crate::provider_input::ProviderInputCounter,
) -> Result<usize, StreamingError> {
    counter.estimate_request(request).map_err(|error| {
        StreamingError::Completion(CompletionError::RequestError(Box::new(
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("provider_input_projection_failed: {error:#}"),
            ),
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
pub fn clamp_request_output_budget(
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
pub fn ensure_context_can_dispatch(
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

#[allow(clippy::too_many_arguments)]
pub(super) async fn build_budgeted_request<M: CompletionModel>(
    model: &M,
    history: &mut Vec<TaggedMessage>,
    new_messages: &mut Vec<TaggedMessage>,
    tools: &[Box<dyn ToolDyn>],
    config: &LoopConfig,
    replay: &mut LoopReplayInput,
    turn_index: usize,
    reduction_chain_keys: &mut Vec<String>,
    active_reduction_keys: &mut Vec<String>,
) -> Result<(CompletionRequest, TurnContextDecision), StreamingError> {
    narrow_joined_input(
        config.provider_input_counter.profile(),
        history,
        new_messages,
        replay,
    )
    .await?;
    let current_prompt = new_messages
        .last()
        .map(|row| row.message.clone())
        .expect("new_messages always retains at least the initial prompt");
    let prior = message_values(&new_messages[..new_messages.len() - 1]);
    let history_messages = message_values(history);
    let request = build_request(
        model,
        current_prompt,
        &history_messages,
        &prior,
        tools,
        config,
    )
    .await?;
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
        required: replay.required.clone(),
        admission,
        turn_index,
        prior_reduction_keys: reduction_chain_keys.clone(),
    })
    .await
    .map_err(|error| {
        if matches!(
            error.downcast_ref::<crate::compaction::ReductionError>(),
            Some(crate::compaction::ReductionError::CannotFit)
        ) || error.is::<ReplayCheckpointError>()
            || error.is::<ReplayEvidenceViolation>()
        {
            return StreamingError::Completion(CompletionError::RequestError(
                error.into_boxed_dyn_error(),
            ));
        }
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
            )));
        }
    };
    let compacted_prompt = compacted.pop().ok_or_else(|| {
        StreamingError::Completion(CompletionError::RequestError(Box::new(
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "per-turn provider-input compaction returned no prompt",
            ),
        )))
    })?;
    *history = compacted;
    *new_messages = vec![compacted_prompt.clone()];
    narrow_joined_input(
        config.provider_input_counter.profile(),
        history,
        new_messages,
        replay,
    )
    .await?;
    if let Some(reduction_key) = reduction_key {
        reduction_chain_keys.push(reduction_key.clone());
        active_reduction_keys.clear();
        active_reduction_keys.push(reduction_key);
    }

    let history_messages = message_values(history);
    let rebuilt = build_request(
        model,
        compacted_prompt.message,
        &history_messages,
        &[],
        tools,
        config,
    )
    .await?;
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
    history: &mut Vec<TaggedMessage>,
    new_messages: &mut Vec<TaggedMessage>,
    tools: &[Box<dyn ToolDyn>],
    config: &LoopConfig,
    replay: &mut LoopReplayInput,
) -> Result<CompletionRequest, StreamingError> {
    repair_provider_input(
        config.provider_input_counter.profile(),
        history,
        new_messages,
    )?;
    narrow_joined_input(
        config.provider_input_counter.profile(),
        history,
        new_messages,
        replay,
    )
    .await?;
    let repaired_prompt = new_messages
        .last()
        .map(|row| row.message.clone())
        .expect("successful repair restores one prompt");
    let repaired_prior = message_values(&new_messages[..new_messages.len() - 1]);
    let repaired_history = message_values(history);
    build_request(
        model,
        repaired_prompt,
        &repaired_history,
        &repaired_prior,
        tools,
        config,
    )
    .await
}

pub(super) async fn narrow_joined_input(
    profile: ProviderInputProfile,
    history: &mut Vec<TaggedMessage>,
    new_messages: &mut Vec<TaggedMessage>,
    replay: &mut LoopReplayInput,
) -> Result<(), StreamingError> {
    let history_len = history.len();
    let mut joined = std::mem::take(history);
    joined.append(new_messages);
    narrow_tagged_history(profile, &mut joined, replay).await?;
    *new_messages = joined.split_off(history_len);
    *history = joined;
    Ok(())
}
