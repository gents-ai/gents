use super::*;
use crate::claude_messages_body::{
    prepare_replay_checkpoint, replay_stage, select_replay, ReplayCheckpointError, ReplayTag,
    ResolvedReplayEvidence, TaggedAssistantRow,
};
use gents_protocol::error_chain::ErrorChain;

pub(super) mod dispatch;
pub(super) mod repair;
pub(super) mod tagged_history;
pub(super) mod turn_budget;

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

#[cfg(test)]
#[path = "request_assembly_dispatch_tests.rs"]
mod dispatch_tests;
