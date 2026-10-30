use super::*;
use crate::claude_messages_body::ReplayCheckpointError;
use crate::provider_input::ProviderInputProfile;

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

pub(super) fn tagged_from_sourced(
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

pub(in crate::loop_stream) fn provider_order(
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
