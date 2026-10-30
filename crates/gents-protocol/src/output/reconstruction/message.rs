use super::extents::Extents;
use super::invalid_extent;
use super::payload::{
    expect_payload, is_arguments, is_reasoning, is_reasoning_encrypted, is_reasoning_redacted,
    is_reasoning_summary, is_text, is_tool_output, reconstruct_media, resolve_full,
    resolve_presented,
};
use super::validation::{
    validate_native_shape, validate_reasoning_signature, validate_reference_source,
};
use crate::message::{
    AssistantContent, Audio, Document, Image, Message, Reasoning, ReasoningContent, Text, ToolCall,
    ToolFunction, ToolResult, ToolResultContent, UserContent, Video,
};
use crate::output::{
    MediaData, MediaKind, MediaType, MessageBlock, MessagePublication, MessageRole, OutputSource,
    PayloadRef, ReasoningPart, ReconstructionError, StreamPayload, ToolResultPart,
    TranscriptMessage,
};

pub(super) fn reconstruct_message_with(
    extents: &Extents<'_, '_>,
    message: &TranscriptMessage,
) -> Result<Message, ReconstructionError> {
    validate_native_shape(message)?;

    let validate_reference =
        |reference: &PayloadRef| validate_reference_source(extents, message, reference);
    // Closures/source provenance are validated before native decoding.
    let mut provenance_failure = None;
    for reference in message.payload_references() {
        if provenance_failure.is_none() {
            provenance_failure = validate_reference(reference).err();
        }
    }
    if let Some(error) = provenance_failure {
        return Err(error);
    }

    let mut provider_positions: Vec<(OutputSource, u32, u32)> = Vec::new();
    let mut check_position = |reference: &PayloadRef,
                              block: u32,
                              part: u32,
                              exact: bool|
     -> Result<(), ReconstructionError> {
        let closing = extents.closing(reference)?;
        if let OutputSource::ProviderTurn { .. } = &closing.segment.source {
            let extent = extents.extent(reference)?;
            let stream = &extent.streams[reference.stream as usize];
            if exact
                && (stream.declaration.block_index != block
                    || stream.declaration.part_index != part)
            {
                return Err(invalid_extent(reference, stream.text.len() as u64));
            }
            if matches!(
                message.publication,
                MessagePublication::RequestRecovery { .. }
            ) && (!matches!(stream.declaration.payload, StreamPayload::Text)
                || stream.declaration.part_index != 0)
            {
                return Err(invalid_extent(reference, stream.text.len() as u64));
            }
            provider_positions.push((
                closing.segment.source.clone(),
                stream.declaration.block_index,
                stream.declaration.part_index,
            ));
        }
        Ok(())
    };
    for (block_index, block) in message.blocks.iter().enumerate() {
        let block_index =
            u32::try_from(block_index).map_err(|_| ReconstructionError::InvalidStructure {
                detail: "too many message blocks".to_owned(),
            })?;
        match block {
            MessageBlock::Text { text } => check_position(
                &text.output,
                block_index,
                0,
                matches!(
                    message.publication,
                    MessagePublication::RequestExecution { .. }
                ),
            )?,
            MessageBlock::Reasoning { parts, .. } => {
                for (part_index, part) in parts.iter().enumerate() {
                    let reference = match part {
                        ReasoningPart::Text { text, .. } | ReasoningPart::Summary { text } => text,
                        ReasoningPart::Encrypted { data } | ReasoningPart::Redacted { data } => {
                            data
                        }
                    };
                    check_position(
                        reference,
                        block_index,
                        u32::try_from(part_index).map_err(|_| {
                            ReconstructionError::InvalidStructure {
                                detail: "too many message parts".to_owned(),
                            }
                        })?,
                        matches!(
                            message.publication,
                            MessagePublication::RequestExecution { .. }
                        ),
                    )?;
                }
            }
            MessageBlock::ToolCall { arguments, .. } => check_position(
                arguments,
                block_index,
                0,
                matches!(
                    message.publication,
                    MessagePublication::RequestExecution { .. }
                ),
            )?,
            MessageBlock::ToolResult { parts, .. } => {
                for (part_index, part) in parts.iter().enumerate() {
                    let part_index = u32::try_from(part_index).map_err(|_| {
                        ReconstructionError::InvalidStructure {
                            detail: "too many message parts".to_owned(),
                        }
                    })?;
                    match part {
                        ToolResultPart::Text { text } => check_position(
                            &text.output,
                            block_index,
                            part_index,
                            matches!(
                                message.publication,
                                MessagePublication::RequestExecution { .. }
                            ),
                        )?,
                        ToolResultPart::Media(media) => {
                            if let MediaData::Base64 { data }
                            | MediaData::Raw { data }
                            | MediaData::String { data } = &media.data
                            {
                                check_position(
                                    data,
                                    block_index,
                                    part_index,
                                    matches!(
                                        message.publication,
                                        MessagePublication::RequestExecution { .. }
                                    ),
                                )?;
                            }
                        }
                    }
                }
            }
            MessageBlock::Media(media) => {
                if let MediaData::Base64 { data }
                | MediaData::Raw { data }
                | MediaData::String { data } = &media.data
                {
                    check_position(
                        data,
                        block_index,
                        0,
                        matches!(
                            message.publication,
                            MessagePublication::RequestExecution { .. }
                        ),
                    )?;
                }
            }
        }
    }
    for pair in provider_positions.windows(2) {
        if pair[0].0 == pair[1].0 && (pair[0].1, pair[0].2) >= (pair[1].1, pair[1].2) {
            return Err(ReconstructionError::InvalidStructure {
                detail: "provider payload declarations are out of native order".to_owned(),
            });
        }
    }

    let assistant_content =
        |block: &MessageBlock| -> Result<AssistantContent, ReconstructionError> {
            match block {
                MessageBlock::Text { text } => Ok(AssistantContent::Text(Text {
                    text: resolve_presented(extents, text, &[is_text, is_tool_output])?,
                })),
                MessageBlock::Reasoning { id, parts } => {
                    Ok(AssistantContent::Reasoning(Reasoning {
                        id: id.clone(),
                        content: parts
                            .iter()
                            .map(|part| match part {
                                ReasoningPart::Text { text, signature } => {
                                    validate_reasoning_signature(
                                        extents,
                                        text,
                                        signature.as_deref(),
                                    )?;
                                    Ok(ReasoningContent::Text {
                                        text: resolve_full(extents, text, &[is_reasoning], false)?,
                                        signature: signature.clone(),
                                    })
                                }
                                ReasoningPart::Encrypted { data } => {
                                    Ok(ReasoningContent::Encrypted(resolve_full(
                                        extents,
                                        data,
                                        &[is_reasoning_encrypted],
                                        false,
                                    )?))
                                }
                                ReasoningPart::Redacted { data } => {
                                    Ok(ReasoningContent::Redacted {
                                        data: resolve_full(
                                            extents,
                                            data,
                                            &[is_reasoning_redacted],
                                            false,
                                        )?,
                                    })
                                }
                                ReasoningPart::Summary { text } => Ok(ReasoningContent::Summary(
                                    resolve_full(extents, text, &[is_reasoning_summary], false)?,
                                )),
                            })
                            .collect::<Result<Vec<_>, _>>()?,
                    }))
                }
                MessageBlock::ToolCall {
                    id,
                    call_id,
                    name,
                    arguments,
                    signature,
                    additional_params,
                    ..
                } => {
                    let stream = extents.stream(arguments)?;
                    if !matches!(&stream.declaration.payload, StreamPayload::ToolArguments { id: declared_id, call_id: declared_call_id, name: declared_name } if declared_id == id && declared_call_id == call_id && declared_name == name)
                    {
                        return Err(invalid_extent(arguments, stream.text.len() as u64));
                    }
                    let argument_reference = arguments.clone();
                    expect_payload(&stream, arguments, &[is_arguments], true, true)?;
                    let arguments = stream.text;
                    let arguments = serde_json::from_str(&arguments).map_err(|_| {
                        ReconstructionError::InvalidPayload {
                            reference: argument_reference,
                        }
                    })?;
                    Ok(AssistantContent::ToolCall(ToolCall {
                        id: id.clone(),
                        call_id: call_id.clone(),
                        function: ToolFunction {
                            name: name.clone(),
                            arguments,
                        },
                        signature: signature.clone(),
                        additional_params: additional_params.clone(),
                    }))
                }
                MessageBlock::Media(media) if media.kind == MediaKind::Image => {
                    let (_, data) = reconstruct_media(extents, media)?;
                    Ok(AssistantContent::Image(Image {
                        data,
                        media_type: match &media.media_type {
                            Some(MediaType::Image(value)) => Some(value.clone()),
                            _ => None,
                        },
                        detail: media.detail.clone(),
                        additional_params: media.additional_params.clone(),
                    }))
                }
                _ => Err(ReconstructionError::InvalidStructure {
                    detail: "non-assistant block in assistant message".to_owned(),
                }),
            }
        };
    match message.role {
        MessageRole::System => match &message.blocks[0] {
            MessageBlock::Text { text } => Ok(Message::System {
                content: resolve_presented(extents, text, &[is_text, is_tool_output])?,
            }),
            _ => unreachable!(),
        },
        MessageRole::Assistant => Ok(Message::Assistant {
            id: message.native_id.clone(),
            content: message
                .blocks
                .iter()
                .map(assistant_content)
                .collect::<Result<Vec<_>, _>>()?,
        }),
        MessageRole::User => {
            let mut content = Vec::new();
            for block in &message.blocks {
                match block {
                    MessageBlock::Text { text } => content.push(UserContent::Text(Text {
                        text: resolve_presented(extents, text, &[is_text, is_tool_output])?,
                    })),
                    MessageBlock::ToolResult {
                        id, call_id, parts, ..
                    } => content.push(UserContent::ToolResult(ToolResult {
                        id: id.clone(),
                        call_id: call_id.clone(),
                        content: parts
                            .iter()
                            .map(|part| match part {
                                ToolResultPart::Text { text } => {
                                    Ok(ToolResultContent::Text(Text {
                                        text: resolve_presented(extents, text, &[is_tool_output])?,
                                    }))
                                }
                                ToolResultPart::Media(media) => {
                                    let (_, data) = reconstruct_media(extents, media)?;
                                    Ok(ToolResultContent::Image(Image {
                                        data,
                                        media_type: match &media.media_type {
                                            Some(MediaType::Image(value)) => Some(value.clone()),
                                            _ => None,
                                        },
                                        detail: media.detail.clone(),
                                        additional_params: media.additional_params.clone(),
                                    }))
                                }
                            })
                            .collect::<Result<Vec<_>, _>>()?,
                    })),
                    MessageBlock::Media(media) => {
                        let (kind, data) = reconstruct_media(extents, media)?;
                        match (kind, &media.media_type) {
                            (MediaKind::Image, Some(MediaType::Image(media_type))) => {
                                content.push(UserContent::Image(Image {
                                    data,
                                    media_type: Some(media_type.clone()),
                                    detail: media.detail.clone(),
                                    additional_params: media.additional_params.clone(),
                                }))
                            }
                            (MediaKind::Image, None) => content.push(UserContent::Image(Image {
                                data,
                                media_type: None,
                                detail: media.detail.clone(),
                                additional_params: media.additional_params.clone(),
                            })),
                            (MediaKind::Audio, Some(MediaType::Audio(media_type))) => {
                                content.push(UserContent::Audio(Audio {
                                    data,
                                    media_type: Some(media_type.clone()),
                                    additional_params: media.additional_params.clone(),
                                }))
                            }
                            (MediaKind::Audio, None) => content.push(UserContent::Audio(Audio {
                                data,
                                media_type: None,
                                additional_params: media.additional_params.clone(),
                            })),
                            (MediaKind::Video, Some(MediaType::Video(media_type))) => {
                                content.push(UserContent::Video(Video {
                                    data,
                                    media_type: Some(media_type.clone()),
                                    additional_params: media.additional_params.clone(),
                                }))
                            }
                            (MediaKind::Video, None) => content.push(UserContent::Video(Video {
                                data,
                                media_type: None,
                                additional_params: media.additional_params.clone(),
                            })),
                            (MediaKind::Document, Some(MediaType::Document(media_type))) => content
                                .push(UserContent::Document(Document {
                                    data,
                                    media_type: Some(media_type.clone()),
                                    additional_params: media.additional_params.clone(),
                                })),
                            (MediaKind::Document, None) => {
                                content.push(UserContent::Document(Document {
                                    data,
                                    media_type: None,
                                    additional_params: media.additional_params.clone(),
                                }))
                            }
                            _ => {
                                return Err(ReconstructionError::InvalidStructure {
                                    detail: "media type does not match native content".to_owned(),
                                })
                            }
                        }
                    }
                    _ => {
                        return Err(ReconstructionError::InvalidStructure {
                            detail: "non-user block in user message".to_owned(),
                        })
                    }
                }
            }
            Ok(Message::User { content })
        }
    }
}
