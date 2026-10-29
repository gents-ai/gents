use super::*;
use crate::claude_messages_body::{
    prepare_replay_checkpoint, replay_stage, select_replay, ReplayCheckpointError, ReplayTag,
    ResolvedReplayEvidence, TaggedAssistantRow,
};
use crate::provider_input::ProviderInputProfile;
use gents_protocol::error_chain::ErrorChain;

pub(super) fn message_values(rows: &[TaggedMessage]) -> Vec<Message> {
    rows.iter().map(|row| row.message.clone()).collect()
}

pub fn replay_compaction_prefix_bound(
    rows: &[TaggedMessage],
    required: &[ReplayTag],
) -> Result<Option<usize>, ReplayCheckpointError> {
    let mut assistants = Vec::new();
    for row in rows {
        match &row.message {
            Message::Assistant { id, content } => assistants.push(TaggedAssistantRow {
                source: row.source.clone(),
                physical_header: row.physical_header.clone(),
                block_indices: row.block_indices.clone(),
                id: id.clone(),
                content: content.clone(),
            }),
            _ if row.source.is_some() => return Err(ReplayCheckpointError::InvalidAssociation),
            _ => {}
        }
    }
    prepare_replay_checkpoint(required.to_vec(), assistants, 0)?;
    Ok((!required.is_empty()).then_some(0))
}

fn replay_input_error(message: impl Into<String>) -> StreamingError {
    StreamingError::Completion(CompletionError::RequestError(Box::new(
        std::io::Error::new(std::io::ErrorKind::InvalidInput, message.into()),
    )))
}

pub(super) fn replay_resolution_error(error: ReplayEvidenceResolveError) -> StreamingError {
    match error {
        ReplayEvidenceResolveError::Violation(violation) => {
            StreamingError::Completion(CompletionError::RequestError(Box::new(violation)))
        }
        ReplayEvidenceResolveError::Storage { source } => {
            StreamingError::Completion(CompletionError::ProviderError(format!(
                "loading canonical Claude replay evidence failed: {}",
                ErrorChain(source.as_ref()),
            )))
        }
    }
}

pub async fn resolve_replay_evidence(
    replay: &LoopReplayInput,
    requested: &[ReplayTag],
) -> Result<Vec<ReplayEvidenceRow>, StreamingError> {
    let mut distinct = Vec::new();
    for tag in requested {
        if !distinct.contains(tag) {
            distinct.push(tag.clone());
        }
    }
    if distinct.is_empty() {
        return Ok(Vec::new());
    }
    let mut evidence = replay
        .estimate_seed
        .as_ref()
        .and_then(|seed| seed.take())
        .unwrap_or_default()
        .into_iter()
        .filter(|row| distinct.contains(&row.tag))
        .collect::<Vec<_>>();
    let missing = distinct
        .iter()
        .filter(|tag| !evidence.iter().any(|row| &row.tag == *tag))
        .cloned()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        let resolve = replay.resolve.as_ref().ok_or_else(|| {
            replay_input_error("reasoning replay has no canonical evidence resolver")
        })?;
        evidence.extend(resolve(missing).await.map_err(replay_resolution_error)?);
    }
    if evidence.iter().any(|row| !distinct.contains(&row.tag)) {
        return Err(replay_input_error(
            "canonical resolver returned an unrequested replay source",
        ));
    }
    Ok(evidence)
}

#[cfg(test)]
#[path = "request_assembly_replay_error_tests.rs"]
mod replay_error_tests;

pub fn select_tagged_assistant_blocks(
    source: &TaggedMessage,
    retained_original_indices: &[usize],
) -> Result<TaggedMessage, ReplayCheckpointError> {
    let Message::Assistant { id, content } = &source.message else {
        return Err(ReplayCheckpointError::InvalidAssociation);
    };
    if source.block_indices.len() != content.len()
        || source
            .block_indices
            .windows(2)
            .any(|pair| pair[0] >= pair[1])
    {
        return Err(ReplayCheckpointError::InvalidAssociation);
    }
    let (block_indices, selected): (Vec<_>, Vec<_>) = source
        .block_indices
        .iter()
        .copied()
        .zip(content)
        .filter(|(index, _)| retained_original_indices.contains(index))
        .map(|(index, block)| (index, block.clone()))
        .unzip();
    Ok(TaggedMessage {
        message: Message::Assistant {
            id: id.clone(),
            content: selected,
        },
        source: source.source.clone(),
        physical_header: source.physical_header.clone(),
        block_indices,
    })
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
            if let Message::Assistant { content, .. } = &item.message {
                if item.block_indices.len() != content.len() {
                    return Err(replay_input_error(
                        "provider projection misaligned assistant block indices",
                    ));
                }
                let mut selected = if original.physical_header.is_none()
                    && original.block_indices.is_empty()
                {
                    TaggedMessage {
                        message: original.message.clone(),
                        source: original.source.clone(),
                        physical_header: None,
                        block_indices: Vec::new(),
                    }
                } else {
                    if item.block_indices.windows(2).any(|pair| pair[0] >= pair[1]) {
                        return Err(replay_input_error(
                            "provider projection reordered associated assistant blocks",
                        ));
                    }
                    let original_indices = item.block_indices.iter().map(|position| {
                        original.block_indices.get(*position).copied().ok_or_else(|| {
                            replay_input_error("provider projection emitted an out-of-range assistant block index")
                        })
                    }).collect::<Result<Vec<_>, _>>()?;
                    select_tagged_assistant_blocks(original, &original_indices).map_err(|error| {
                        replay_input_error(format!("provider projection: {error}"))
                    })?
                };
                selected.message = item.message;
                Ok(selected)
            } else {
                Ok(TaggedMessage {
                    message: item.message,
                    source: original.source.clone(),
                    physical_header: original.physical_header.clone(),
                    block_indices: Vec::new(),
                })
            }
        })
        .collect()
}

pub fn sanitize_tagged_history(
    rows: Vec<TaggedMessage>,
) -> Result<Vec<TaggedMessage>, StreamingError> {
    let messages = rows.iter().map(|row| row.message.clone()).collect();
    tagged_from_sourced(
        &rows,
        crate::compaction::sanitize_association_with_sources(messages),
    )
}

pub fn provider_view_tagged(
    rows: Vec<TaggedMessage>,
) -> Result<Vec<TaggedMessage>, StreamingError> {
    let messages = rows.iter().map(|row| row.message.clone()).collect();
    let (sourced, _) = crate::compaction::association_view_with_sources(messages);
    tagged_from_sourced(&rows, sourced)
}

pub fn provider_messages(profile: ProviderInputProfile, rows: &[TaggedMessage]) -> Vec<Message> {
    provider_order(profile, message_values(rows))
}

pub(super) fn provider_order(
    profile: ProviderInputProfile,
    messages: Vec<Message>,
) -> Vec<Message> {
    crate::compaction::history::normalize_assistant_content_order(profile, messages)
}

pub fn association_reduction(
    profile: ProviderInputProfile,
    rows: &[TaggedMessage],
    exact: crate::compaction::ExactReduction<'_>,
) -> Result<crate::compaction::AssociationReduction, crate::compaction::CompactionError> {
    let split = exact.compacted_prefix.len();
    if split > rows.len() {
        return Err(crate::compaction::CompactionError::SplitExceedsRows);
    }
    let (prefix, suffix) = rows.split_at(split);
    exact.over_association_view(profile, message_values(prefix), message_values(suffix))
}

fn assistant_rows(rows: &[TaggedMessage]) -> (Vec<usize>, Vec<TaggedAssistantRow>) {
    rows.iter()
        .enumerate()
        .filter_map(|(index, row)| match &row.message {
            Message::Assistant { id, content } => Some((
                index,
                TaggedAssistantRow {
                    source: row.source.clone(),
                    physical_header: row.physical_header.clone(),
                    block_indices: row.block_indices.clone(),
                    id: id.clone(),
                    content: content.clone(),
                },
            )),
            _ => None,
        })
        .unzip()
}

fn apply_assistant_rows(
    rows: &mut [TaggedMessage],
    indices: &[usize],
    selected: Vec<TaggedAssistantRow>,
) -> Result<(), StreamingError> {
    if selected.len() != indices.len() {
        return Err(replay_input_error(
            "reasoning replay changed assistant row cardinality",
        ));
    }
    for (index, selected) in indices.iter().zip(selected) {
        let row = &mut rows[*index];
        if row.source != selected.source {
            return Err(replay_input_error(
                "reasoning replay changed assistant source association",
            ));
        }
        let Message::Assistant { content, .. } = &mut row.message else {
            unreachable!("indices name assistant rows")
        };
        *content = selected.content;
        row.block_indices = selected.block_indices;
        row.physical_header = selected.physical_header;
    }
    Ok(())
}

pub(super) fn strip_all_reasoning(rows: &mut [TaggedMessage]) {
    for row in rows {
        let Message::Assistant { content, .. } = &mut row.message else {
            continue;
        };
        let mut indices = row.block_indices.iter();
        let mut kept_indices = Vec::new();
        content.retain(|block| {
            let index = indices.next();
            let keep = !matches!(block, AssistantContent::Reasoning(_));
            if keep {
                kept_indices.extend(index.copied());
            }
            keep
        });
        if !row.block_indices.is_empty() {
            row.block_indices = kept_indices;
        }
    }
}

fn split_joined(
    mut joined: Vec<TaggedMessage>,
    history_len: usize,
) -> (Vec<TaggedMessage>, Vec<TaggedMessage>) {
    let new_messages = joined.split_off(history_len);
    (joined, new_messages)
}

async fn build_core<M>(
    model: &M,
    history: &[TaggedMessage],
    new_messages: &[TaggedMessage],
    tools: &[Box<dyn ToolDyn>],
    config: &LoopConfig,
) -> Result<CompletionRequest, StreamingError>
where
    M: CompletionModel,
{
    let prompt = new_messages
        .last()
        .expect("new messages retain a prompt")
        .message
        .clone();
    let prior = message_values(&new_messages[..new_messages.len() - 1]);
    build_request(
        model,
        prompt,
        &message_values(history),
        &prior,
        tools,
        config,
    )
    .await
}

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

#[derive(Debug, thiserror::Error)]
#[error("provider_input_repair_removed_complete_prompt")]
struct ProviderInputRepairError;

pub fn repair_provider_input(
    history: &mut Vec<TaggedMessage>,
    new_messages: &mut Vec<TaggedMessage>,
) -> Result<(), StreamingError> {
    let mut provider_messages = std::mem::take(history);
    provider_messages.append(new_messages);
    repair_messages(&mut provider_messages);
    let mut provider_messages = sanitize_tagged_history(provider_messages)?;
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

pub fn completion_request_input_components(
    request: &CompletionRequest,
    counter: &crate::provider_input::ProviderInputCounter,
) -> Result<crate::provider_input::ProviderInputProjection, StreamingError> {
    counter.project_request(request).map_err(|error| {
        StreamingError::Completion(CompletionError::RequestError(Box::new(
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("provider_input_projection_failed: {}", ErrorChain(&error)),
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
                format!("provider_input_projection_failed: {}", ErrorChain(&error)),
            ),
        )))
    })
}

#[derive(Clone, Debug)]
pub(super) struct PreparedDispatch {
    request: CompletionRequest,
    projection: crate::provider_input::ProviderInputProjection,
    context: crate::provider_input::budget::ContextEstimate,
}

impl PreparedDispatch {
    pub(super) fn request(&self) -> &CompletionRequest {
        &self.request
    }

    pub(super) fn projection(&self) -> &crate::provider_input::ProviderInputProjection {
        &self.projection
    }

    /// The request's size in provider tokens under the calibration and
    /// anchor in effect when this attempt was prepared.
    pub(super) fn context(&self) -> crate::provider_input::budget::ContextEstimate {
        self.context
    }
}

pub(super) fn prepare_dispatch_attempt(
    assembled_request: &CompletionRequest,
    config: &LoopConfig,
    aggregate_token_budget: Option<&AggregateTokenBudget>,
) -> Result<PreparedDispatch, crate::error::LoopStreamError> {
    let mut request = assembled_request.clone();
    let estimated_input_tokens =
        completion_request_input_tokens(&request, config.provider_input_counter.as_ref())?;
    let context = config.context_calibration.context(estimated_input_tokens);
    // The calibrated count triggers compaction and caps output, but only the
    // raw count can refuse dispatch: the provider's own overflow rejection is
    // recovered, an overestimate is not.
    let configured_max_tokens = request.max_tokens;
    clamp_request_output_budget(&mut request, config, context.tokens);
    if request.max_tokens == Some(0) {
        request.max_tokens = configured_max_tokens;
        clamp_request_output_budget(&mut request, config, estimated_input_tokens);
    }
    ensure_context_can_dispatch(&request, config, estimated_input_tokens)?;
    super::aggregate_budget::clamp_request_aggregate_token_budget(
        &mut request,
        aggregate_token_budget,
        &context,
    )?;
    let projection =
        completion_request_input_components(&request, config.provider_input_counter.as_ref())?;
    debug_assert_eq!(projection.estimated_input_tokens, estimated_input_tokens);
    Ok(PreparedDispatch {
        request,
        projection,
        context,
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
    ContextAccounting {
        accounting_version: CONTEXT_ACCOUNTING_VERSION,
        turn_index,
        attempt,
        estimator: projection.estimator.to_string(),
        components: projection.components.clone(),
        estimated_input_tokens: dispatch.context().tokens,
        raw_estimated_input_tokens: Some(projection.estimated_input_tokens),
        input_calibration_basis_points: Some(dispatch.context().ratio_basis_points),
        anchored_input_tokens: dispatch.context().anchored,
        context_window: config.context_window,
        compaction_threshold_basis_points: crate::provider_input::budget::threshold_basis_points(
            config.compaction_threshold,
        ),
        compaction_threshold_tokens: crate::provider_input::budget::compaction_input_budget(
            config.context_window,
            config.compaction_threshold,
            crate::provider_input::budget::configured_output_ceiling(config.max_tokens),
        ),
        configured_max_output_tokens: config.max_tokens,
        effective_max_output_tokens: request.max_tokens,
        compaction_reason,
        pre_compaction_input_tokens,
    }
}

pub fn clamp_request_output_budget(
    request: &mut CompletionRequest,
    config: &LoopConfig,
    input_tokens: usize,
) {
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

pub async fn assemble_provider_request<M>(
    model: &M,
    history: &mut Vec<TaggedMessage>,
    new_messages: &mut Vec<TaggedMessage>,
    tools: &[Box<dyn ToolDyn>],
    config: &LoopConfig,
    replay: &LoopReplayInput,
) -> Result<CompletionRequest, StreamingError>
where
    M: CompletionModel,
{
    let profile_wire = config.provider_input_counter.profile().replay_wire();
    if replay.wire.is_some() && replay.wire != profile_wire {
        return Err(replay_input_error(
            "replay wire differs from built provider profile",
        ));
    }
    let history_len = history.len();
    let mut joined = std::mem::take(history);
    joined.append(new_messages);
    let (indices, mut rows) = assistant_rows(&joined);
    let has_reasoning = rows.iter().any(|row| {
        row.content
            .iter()
            .any(|block| matches!(block, AssistantContent::Reasoning(_)))
    });
    let selection = match (has_reasoning, profile_wire, replay.issuer.as_ref()) {
        (false, ..) => None,
        (true, Some(wire), Some(issuer)) if replay.wire == Some(wire) => Some((wire, issuer)),
        (true, ..) => {
            strip_all_reasoning(&mut joined);
            None
        }
    };
    let Some((wire, issuer)) = selection else {
        (*history, *new_messages) = split_joined(joined, history_len);
        return build_core(model, history, new_messages, tools, config).await;
    };
    let tags = rows
        .iter()
        .filter(|row| {
            row.content
                .iter()
                .any(|block| matches!(block, AssistantContent::Reasoning(_)))
        })
        .filter_map(|row| row.source.clone())
        .collect::<Vec<_>>();
    let evidence = resolve_replay_evidence(replay, &tags).await?;
    let resolve = |tag: &ReplayTag| -> Vec<ResolvedReplayEvidence> {
        evidence
            .iter()
            .filter(|row| row.tag == *tag)
            .map(|row| row.evidence.clone())
            .collect()
    };
    for row in rows
        .iter_mut()
        .filter(|row| row.physical_header.is_none() && row.block_indices.is_empty())
    {
        if let Some(tag) = &row.source {
            if let [evidence] = resolve(tag).as_slice() {
                row.physical_header = Some(evidence.physical_header.clone());
                row.block_indices = (0..row.content.len()).collect();
            }
        }
    }
    apply_assistant_rows(
        &mut joined,
        &indices,
        replay_stage(&rows, issuer, wire, resolve),
    )?;
    let (stage_history, stage_new) = split_joined(joined.clone(), history_len);
    let mut core = build_core(model, &stage_history, &stage_new, tools, config).await?;
    let body = config
        .provider_input_counter
        .project_body(&core)
        .map_err(|error| {
            replay_input_error(format!(
                "provider replay projection: {}",
                ErrorChain(&error)
            ))
        })?;
    let stage_body =
        crate::provider_input::replay_frontier::flatten(&body, wire).map_err(|error| {
            replay_input_error(format!(
                "provider replay projection: {}",
                ErrorChain(&error)
            ))
        })?;
    apply_assistant_rows(
        &mut joined,
        &indices,
        select_replay(&rows, issuer, wire, resolve, &stage_body),
    )?;
    (*history, *new_messages) = split_joined(joined, history_len);
    super::replace_core_chat_history(&mut core, config, history, new_messages)?;
    Ok(core)
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn build_budgeted_request<M>(
    model: &M,
    history: &mut Vec<TaggedMessage>,
    new_messages: &mut Vec<TaggedMessage>,
    tools: &[Box<dyn ToolDyn>],
    config: &LoopConfig,
    replay: &mut LoopReplayInput,
    turn_index: usize,
    reduction_chain_keys: &mut Vec<String>,
    active_reduction_keys: &mut Vec<String>,
) -> Result<(CompletionRequest, TurnContextDecision), crate::error::LoopStreamError>
where
    M: CompletionModel,
{
    let request =
        assemble_provider_request(model, history, new_messages, tools, config, replay).await?;
    let projection =
        completion_request_input_components(&request, config.provider_input_counter.as_ref())?;
    let before_tokens = projection.estimated_input_tokens;

    if config.turn_compactor.is_none() {
        return Ok((
            request,
            TurnContextDecision {
                reason: ContextCompactionReason::CompactorUnavailable,
                pre_compaction_input_tokens: None,
            },
        ));
    }
    let provider_budget = crate::provider_input::budget::compaction_input_budget(
        config.context_window,
        config.compaction_threshold,
        crate::provider_input::budget::configured_output_ceiling(config.max_tokens),
    );
    let context = config.context_calibration.context(before_tokens);
    let Some(admission) =
        crate::compaction::ReductionAdmission::for_context(context, provider_budget)
    else {
        return Ok((
            request,
            TurnContextDecision {
                reason: ContextCompactionReason::BelowThreshold,
                pre_compaction_input_tokens: None,
            },
        ));
    };
    reduce_and_rebuild(
        model,
        history,
        new_messages,
        tools,
        config,
        replay,
        turn_index,
        reduction_chain_keys,
        active_reduction_keys,
        admission,
        provider_budget,
        context.tokens,
    )
    .await
}

/// Rebuild the turn's request after the provider rejected it as over its
/// context window. The rejected request's provider/estimate ratio becomes the
/// session's calibration, and the budget is the provider's limit
/// (`overflow.limit`, else the configured window) less the output reserve, in
/// this estimator's units at that ratio, so the reduced request is measured
/// against what the provider will count.
#[allow(clippy::too_many_arguments)]
pub(super) async fn rebuild_after_provider_overflow<M>(
    model: &M,
    history: &mut Vec<TaggedMessage>,
    new_messages: &mut Vec<TaggedMessage>,
    tools: &[Box<dyn ToolDyn>],
    config: &LoopConfig,
    replay: &mut LoopReplayInput,
    turn_index: usize,
    reduction_chain_keys: &mut Vec<String>,
    active_reduction_keys: &mut Vec<String>,
    overflow: crate::error::ContextOverflow,
    rejected_input_tokens: usize,
) -> Result<(CompletionRequest, TurnContextDecision), crate::error::LoopStreamError>
where
    M: CompletionModel,
{
    use crate::provider_input::budget;
    let provider_limit = overflow
        .limit
        .and_then(|limit| usize::try_from(limit).ok())
        .map_or(config.context_window, |limit| {
            limit.min(config.context_window)
        });
    let observed_input_tokens = overflow.input_tokens.map_or_else(
        || {
            u64::try_from(provider_limit)
                .unwrap_or(u64::MAX)
                .saturating_add(1)
        },
        u64::from,
    );
    config
        .context_calibration
        .observe(observed_input_tokens, rejected_input_tokens);
    let ratio = config.context_calibration.ratio_basis_points();
    let provider_budget = budget::compaction_input_budget(
        provider_limit,
        config.compaction_threshold,
        budget::configured_output_ceiling(config.max_tokens),
    );
    let estimator_budget = config.context_calibration.estimator_budget(provider_budget);
    let request =
        assemble_provider_request(model, history, new_messages, tools, config, replay).await?;
    let before_tokens =
        completion_request_input_components(&request, config.provider_input_counter.as_ref())?
            .estimated_input_tokens;
    tracing::warn!(
        target: "gents::agent::loop_stream",
        turn = turn_index,
        provider_limit,
        observed_input_tokens,
        rejected_input_tokens,
        ratio_basis_points = ratio,
        provider_budget,
        estimator_budget,
        "provider rejected the request as over its context window; compacting once"
    );
    reduce_and_rebuild(
        model,
        history,
        new_messages,
        tools,
        config,
        replay,
        turn_index,
        reduction_chain_keys,
        active_reduction_keys,
        crate::compaction::ReductionAdmission::required(before_tokens, estimator_budget),
        provider_budget,
        usize::try_from(observed_input_tokens).unwrap_or(usize::MAX),
    )
    .await
}

/// Run the turn compactor under `admission` and rebuild the request. The
/// rebuilt request, in provider tokens (calibrated: the anchor describes the
/// unreduced transcript), must fit `provider_budget`, the budget that
/// admitted the reduction. `pre_compaction_tokens` is the unreduced
/// request's size in provider tokens, for the accounting.
///
/// The transcript, replay keys and anchor change only when the rebuilt
/// request fits: a reduction that fails, or cannot fit, leaves the session as
/// it was.
#[allow(clippy::too_many_arguments)]
async fn reduce_and_rebuild<M>(
    model: &M,
    history: &mut Vec<TaggedMessage>,
    new_messages: &mut Vec<TaggedMessage>,
    tools: &[Box<dyn ToolDyn>],
    config: &LoopConfig,
    replay: &mut LoopReplayInput,
    turn_index: usize,
    reduction_chain_keys: &mut Vec<String>,
    active_reduction_keys: &mut Vec<String>,
    admission: crate::compaction::ReductionAdmission,
    provider_budget: usize,
    pre_compaction_tokens: usize,
) -> Result<(CompletionRequest, TurnContextDecision), crate::error::LoopStreamError>
where
    M: CompletionModel,
{
    let Some(compactor) = config.turn_compactor.as_ref() else {
        return Err(StreamingError::Completion(CompletionError::RequestError(Box::new(
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "provider input must be reduced but this completion loop has no per-turn compactor",
            ),
        )))
        .into());
    };
    let before_tokens = admission.input_tokens();
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
            error,
            super::TurnCompactionError::Compaction(
                crate::compaction::CompactionError::Reduction(
                    crate::compaction::ReductionError::CannotFit
                ) | crate::compaction::CompactionError::ReplayCheckpoint(_)
                    | crate::compaction::CompactionError::ReplayEvidence(_)
            )
        ) {
            return StreamingError::Completion(CompletionError::RequestError(Box::new(error)))
                .into();
        }
        error.aggregate_token_budget_exhaustion().map_or_else(
            || {
                StreamingError::Completion(CompletionError::ProviderError(format!(
                    "per-turn provider-input compaction failed: {}",
                    ErrorChain(&error)
                )))
                .into()
            },
            |reason| {
                crate::rig_compat::loop_verdict_error(
                    crate::error::LoopVerdict::AggregateTokenBudgetExhausted,
                    reason,
                )
            },
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
            return Err(
                StreamingError::Completion(CompletionError::RequestError(Box::new(
                    crate::compaction::ReductionError::CannotFit,
                )))
                .into(),
            );
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
    let mut rebuilt_history = compacted;
    let mut rebuilt_new_messages = vec![compacted_prompt];
    let rebuilt = assemble_provider_request(
        model,
        &mut rebuilt_history,
        &mut rebuilt_new_messages,
        tools,
        config,
        replay,
    )
    .await?;
    let rebuilt_projection =
        completion_request_input_components(&rebuilt, config.provider_input_counter.as_ref())?;
    let after_tokens = rebuilt_projection.estimated_input_tokens;
    // Provider tokens at the session ratio. Not anchored: the anchor counted
    // the unreduced transcript, which this request no longer extends.
    let after_context_tokens = config.context_calibration.calibrated(after_tokens);
    tracing::info!(
        target: "gents::agent::loop_stream",
        turn = turn_index,
        before_tokens,
        after_tokens,
        after_context_tokens,
        provider_budget,
        context_window = config.context_window,
        max_output_tokens = config.max_tokens.unwrap_or_default(),
        "compacted provider input before completion dispatch"
    );

    let rebuilt_can_dispatch = crate::provider_input::budget::can_dispatch(
        after_context_tokens,
        config.context_window,
        crate::provider_input::budget::configured_output_ceiling(rebuilt.max_tokens),
    );
    if rebuilt_can_dispatch
        && crate::provider_input::budget::threshold_decision(after_context_tokens, provider_budget)
            == crate::provider_input::budget::ThresholdDecision::ReduceEligible
    {
        tracing::warn!(
            estimated_input_tokens = after_tokens,
            after_context_tokens,
            provider_budget,
            "provider input remains over threshold after reduction"
        );
        return Err(
            StreamingError::Completion(CompletionError::RequestError(Box::new(
                crate::compaction::ReductionError::CannotFit,
            )))
            .into(),
        );
    }

    *history = rebuilt_history;
    *new_messages = rebuilt_new_messages;
    config.context_calibration.invalidate_anchor();
    if let Some(reduction_key) = reduction_key {
        reduction_chain_keys.push(reduction_key.clone());
        active_reduction_keys.clear();
        active_reduction_keys.push(reduction_key);
    }

    Ok((
        rebuilt,
        TurnContextDecision {
            reason,
            pre_compaction_input_tokens: Some(pre_compaction_tokens),
        },
    ))
}

pub(super) async fn repair_and_rebuild_request<M>(
    model: &M,
    history: &mut Vec<TaggedMessage>,
    new_messages: &mut Vec<TaggedMessage>,
    tools: &[Box<dyn ToolDyn>],
    config: &LoopConfig,
) -> Result<CompletionRequest, StreamingError>
where
    M: CompletionModel,
{
    repair_provider_input(history, new_messages)?;
    strip_all_reasoning(history);
    strip_all_reasoning(new_messages);
    config.context_calibration.invalidate_anchor();
    build_core(model, history, new_messages, tools, config).await
}

#[cfg(test)]
#[path = "request_assembly_dispatch_tests.rs"]
mod dispatch_tests;
