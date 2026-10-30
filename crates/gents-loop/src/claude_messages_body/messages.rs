//! The Gents history as typed Messages-API messages: user text, tool results
//! with their image limits, and the assistant codec that replays thinking.

use gents_claude::wire::{
    ContentBlock, Message as WireMessage, MessageContent, RawObject, Role,
    ToolResultContent as WireToolResult,
};
use gents_protocol::message::{
    AssistantContent, DocumentSourceKind, Image, Message, ReasoningContent, ToolResultContent,
    UserContent,
};
use serde_json::value::to_raw_value;

use super::images::{image_omissions, media_type};
use super::MessagesBodyError;
use crate::loop_stream::TOOL_RESULT_IMAGE_OMITTED;

/// The non-blank system rows of the history, in order.
pub(super) fn system_rows(history: &[Message]) -> Vec<&str> {
    history
        .iter()
        .filter_map(|message| match message {
            Message::System { content } if !content.trim().is_empty() => Some(content.as_str()),
            _ => None,
        })
        .collect()
}

fn text_block(text: &str) -> ContentBlock {
    ContentBlock::Text {
        text: text.to_owned(),
        citations: None,
        cache_control: None,
    }
}

/// The user and assistant messages of the history; system rows are the system prompt's.
pub(super) fn wire_messages(history: &[Message]) -> Result<Vec<WireMessage>, MessagesBodyError> {
    let mut omissions = image_omissions(history).into_iter();
    let mut out = Vec::with_capacity(history.len());
    for message in history {
        let (role, blocks) = match message {
            Message::User { content } => {
                let mut blocks = Vec::with_capacity(content.len());
                for block in content {
                    match block {
                        UserContent::Text(text) if !text.text.is_empty() => {
                            blocks.push(text_block(&text.text));
                        }
                        UserContent::ToolResult(result) => blocks.push(ContentBlock::ToolResult {
                            tool_use_id: result.id.clone(),
                            content: tool_result_content(&result.content, &mut omissions),
                            is_error: None,
                            cache_control: None,
                        }),
                        _ => {}
                    }
                }
                (Role::User, blocks)
            }
            Message::Assistant { content, .. } => (Role::Assistant, assistant_blocks(content)?),
            Message::System { .. } => continue,
        };
        if !blocks.is_empty() {
            out.push(WireMessage {
                role,
                content: MessageContent::Blocks(blocks),
                output_config: None,
            });
        }
    }
    Ok(out)
}

fn tool_result_content(
    content: &[ToolResultContent],
    omissions: &mut impl Iterator<Item = Option<String>>,
) -> WireToolResult {
    if !content
        .iter()
        .any(|item| matches!(item, ToolResultContent::Image(_)))
    {
        return WireToolResult::Text(
            content
                .iter()
                .filter_map(|item| match item {
                    ToolResultContent::Text(text) => Some(text.text.as_str()),
                    _ => None,
                })
                .collect(),
        );
    }
    WireToolResult::Blocks(
        content
            .iter()
            .filter_map(|item| match item {
                ToolResultContent::Text(item) if item.text.is_empty() => None,
                ToolResultContent::Text(item) => Some(text_block(&item.text)),
                ToolResultContent::Image(image) => Some(match image_block(image) {
                    Some(block) => match omissions.next().flatten() {
                        Some(note) => text_block(&note),
                        None => block,
                    },
                    None => text_block(TOOL_RESULT_IMAGE_OMITTED),
                }),
            })
            .collect(),
    )
}

fn image_block(image: &Image) -> Option<ContentBlock> {
    match (&image.data, media_type(image)) {
        (DocumentSourceKind::Base64(data), Some(media_type)) => Some(ContentBlock::Image {
            media_type: media_type.to_owned(),
            data: data.clone(),
            cache_control: None,
        }),
        (DocumentSourceKind::Url(url), _) => {
            let url = serde_json::to_string(url).ok()?;
            RawObject::from_json(&format!(
                r#"{{"type":"image","source":{{"type":"url","url":{url}}}}}"#
            ))
            .ok()
            .map(ContentBlock::Raw)
        }
        _ => None,
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AssistantCodecError {
    #[error("unsupported assistant image in Claude replay")]
    UnsupportedImage,
    #[error("Claude thinking replay requires a nonempty signature")]
    MissingSignature,
    #[error("Claude redacted thinking replay has no data")]
    EmptyRedacted,
    #[error("unsupported native reasoning kind in Claude replay")]
    UnsupportedReasoning,
    #[error("a tool call input cannot be encoded as JSON")]
    UnencodableToolInput,
}

/// The typed Messages blocks of one assistant row.
pub fn assistant_blocks(
    content: &[AssistantContent],
) -> Result<Vec<ContentBlock>, AssistantCodecError> {
    let mut blocks = Vec::with_capacity(content.len());
    for block in content {
        match block {
            AssistantContent::Text(text) if !text.text.is_empty() => {
                blocks.push(text_block(&text.text));
            }
            AssistantContent::Text(_) => {}
            AssistantContent::Image(_) => return Err(AssistantCodecError::UnsupportedImage),
            AssistantContent::ToolCall(call) => blocks.push(ContentBlock::ToolUse {
                id: call.id.clone(),
                name: call.function.name.clone(),
                input: to_raw_value(&call.function.arguments)
                    .map_err(|_| AssistantCodecError::UnencodableToolInput)?,
                cache_control: None,
            }),
            AssistantContent::Reasoning(reasoning) => {
                for part in &reasoning.content {
                    match part {
                        ReasoningContent::Text { text, signature } => {
                            let signature = signature
                                .as_deref()
                                .filter(|signature| !signature.is_empty())
                                .ok_or(AssistantCodecError::MissingSignature)?;
                            blocks.push(ContentBlock::Thinking {
                                thinking: text.clone(),
                                signature: signature.to_owned(),
                            });
                        }
                        ReasoningContent::Redacted { data } if !data.is_empty() => {
                            blocks.push(ContentBlock::RedactedThinking { data: data.clone() });
                        }
                        ReasoningContent::Redacted { .. } => {
                            return Err(AssistantCodecError::EmptyRedacted);
                        }
                        ReasoningContent::Encrypted(_) | ReasoningContent::Summary(_) => {
                            return Err(AssistantCodecError::UnsupportedReasoning);
                        }
                    }
                }
            }
        }
    }
    Ok(blocks)
}

/// The JSON blocks of one assistant row, as the replay checkpoint compares them.
pub fn encode_assistant_content(
    content: &[AssistantContent],
) -> Result<Vec<serde_json::Value>, AssistantCodecError> {
    assistant_blocks(content)?
        .iter()
        .map(|block| {
            serde_json::to_value(block).map_err(|_| AssistantCodecError::UnencodableToolInput)
        })
        .collect()
}
