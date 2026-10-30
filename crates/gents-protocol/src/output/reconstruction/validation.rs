use super::extents::Extents;
use super::ObservedSegment;
use crate::output::{
    MediaKind, MessageBlock, MessagePublication, MessageRole, OutputSource, OutputWriter,
    PayloadPresentation, PayloadRef, ReconstructionError, ToolResultPart, TranscriptMessage,
};

fn reference_allowed_by_publication(
    message: &TranscriptMessage,
    closing: ObservedSegment<'_>,
) -> bool {
    if closing.segment.source.is_auxiliary_audit() {
        return false;
    }
    match &message.publication {
        MessagePublication::RequestExecution {
            execution_generation,
        } => {
            message.request_doc_id.as_deref() == Some(&closing.segment.request_doc_id)
                && matches!(
                    (&closing.segment.source, &closing.segment.writer),
                    (OutputSource::ProviderTurn { .. }, OutputWriter::RequestExecution { execution_generation: writer })
                        | (OutputSource::Authored { .. }, OutputWriter::RequestExecution { execution_generation: writer })
                        if writer == execution_generation
                )
        }
        MessagePublication::RequestRecovery { .. } => {
            message.request_doc_id.as_deref() == Some(&closing.segment.request_doc_id)
                && matches!(
                    (&closing.segment.source, &closing.segment.writer),
                    (
                        OutputSource::ProviderTurn { .. },
                        OutputWriter::RequestExecution { .. }
                    )
                )
        }
        MessagePublication::ToolDelivery { tool_call_doc_id } => {
            match (&closing.segment.source, &closing.segment.writer) {
                (
                    OutputSource::ToolCall {
                        tool_call_doc_id: source,
                    },
                    OutputWriter::ToolExecution {
                        tool_call_doc_id: writer,
                    },
                ) => source == tool_call_doc_id && writer == tool_call_doc_id,
                (
                    OutputSource::Authored { .. },
                    OutputWriter::ToolExecution {
                        tool_call_doc_id: writer,
                    },
                ) => {
                    writer == tool_call_doc_id
                        && message.request_doc_id.as_deref()
                            == Some(&closing.segment.request_doc_id)
                }
                _ => false,
            }
        }
        MessagePublication::Fork { .. } => true,
    }
}

pub(super) fn validate_reference_source(
    extents: &Extents<'_, '_>,
    message: &TranscriptMessage,
    reference: &PayloadRef,
) -> Result<(), ReconstructionError> {
    let closing = extents.closing(reference)?;
    if reference_allowed_by_publication(message, closing) {
        Ok(())
    } else {
        Err(ReconstructionError::InvalidStructure {
            detail: "payload source is not admitted by message publication".to_owned(),
        })
    }
}

pub(super) fn validate_native_shape(
    message: &TranscriptMessage,
) -> Result<(), ReconstructionError> {
    let allowed = |block: &MessageBlock| match block {
        MessageBlock::Text { .. } => true,
        MessageBlock::Reasoning { .. } | MessageBlock::ToolCall { .. } => {
            message.role == MessageRole::Assistant
        }
        MessageBlock::ToolResult { parts, .. } => {
            message.role == MessageRole::User
                && parts.iter().all(|part| match part {
                    ToolResultPart::Text { .. } => true,
                    ToolResultPart::Media(media) => media.kind == MediaKind::Image,
                })
        }
        MessageBlock::Media(media) => {
            message.role == MessageRole::User
                || (message.role == MessageRole::Assistant && media.kind == MediaKind::Image)
        }
    };
    if !message.blocks.iter().all(allowed)
        || matches!(message.role, MessageRole::System)
            && (message.native_id.is_some()
                || !matches!(message.blocks.as_slice(), [MessageBlock::Text { .. }]))
        || matches!(message.role, MessageRole::User) && message.native_id.is_some()
    {
        return Err(ReconstructionError::InvalidStructure {
            detail: "illegal native message role or block shape".to_owned(),
        });
    }
    match &message.publication {
        MessagePublication::RequestExecution { .. } => {
            if message.request_doc_id.is_none() {
                return Err(ReconstructionError::InvalidStructure { detail: "request publication lacks request membership".to_owned() });
            }
        }
        MessagePublication::RequestRecovery { .. } => {
            if message.request_doc_id.is_none()
                || message.role != MessageRole::Assistant
                || message.outcome != crate::output::OutputOutcome::Partial
                || message.native_id.is_some()
                || !message.blocks.iter().all(|block| matches!(block, MessageBlock::Text { text } if matches!(text.presentation, PayloadPresentation::Full)))
            {
                return Err(ReconstructionError::InvalidStructure { detail: "invalid recovery header".to_owned() });
            }
        }
        MessagePublication::ToolDelivery { tool_call_doc_id } => {
            if message.request_doc_id.is_none()
                || message.role != MessageRole::User
                || message.blocks.iter().any(|block| match block {
                    MessageBlock::Text { .. } => false,
                    MessageBlock::ToolResult { tool_call_doc_id: id, .. } => id != tool_call_doc_id,
                    _ => true,
                })
            {
                return Err(ReconstructionError::InvalidStructure { detail: "invalid tool delivery header".to_owned() });
            }
        }
        MessagePublication::Fork { origin_message_doc_id } => {
            if message.request_doc_id.is_some() || origin_message_doc_id.is_empty() {
                return Err(ReconstructionError::InvalidStructure { detail: "invalid fork provenance".to_owned() });
            }
        }
    }
    Ok(())
}

pub(super) fn validate_reasoning_signature(
    extents: &Extents<'_, '_>,
    body_reference: &PayloadRef,
    signature: Option<&str>,
) -> Result<(), ReconstructionError> {
    let extent = extents.extent(body_reference)?;
    let body = &extent.streams[body_reference.stream as usize];
    let mut signatures = extent
        .signatures
        .get(&(body.declaration.block_index, body.declaration.part_index))
        .into_iter()
        .flatten()
        .map(|index| &extent.streams[*index]);
    let Some(stream) = signatures.next() else {
        if signature.is_none()
            || matches!(
                extents.closing(body_reference)?.segment.source,
                OutputSource::Authored { .. }
            )
        {
            return Ok(());
        }
        return Err(ReconstructionError::InvalidStructure {
            detail: "reasoning signature has no retained stream".to_owned(),
        });
    };
    if signatures.next().is_some() || signature != Some(stream.text.as_str()) {
        return Err(ReconstructionError::InvalidStructure {
            detail: "reasoning signature disagrees with its retained stream".to_owned(),
        });
    }
    Ok(())
}
