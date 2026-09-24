//! Lossless conversion from native messages to payload streams and header plans.

use anyhow::{Context, Result};
use base64::Engine;
use gents_protocol::message::{
    AssistantContent, DocumentSourceKind, Message, ReasoningContent, ToolCall,
};
use gents_protocol::output::{
    MediaBlock, MediaData, MediaKind, MediaType, MessageBlock, MessageRole, PayloadPresentation,
    PayloadRef, PresentedPayload, ReasoningPart, StreamDeclaration, StreamPayload,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EncodedStream {
    pub(crate) declaration: StreamDeclaration,
    pub(crate) payload: String,
}

#[derive(Clone, Debug)]
pub(crate) struct EncodedToolCall {
    pub(crate) native_index: usize,
    pub(crate) id: String,
    pub(crate) call_id: Option<String>,
    pub(crate) name: String,
    pub(crate) arguments_stream: u32,
}

#[derive(Clone, Debug)]
enum BlockPlan {
    Text {
        stream: u32,
    },
    Reasoning {
        id: Option<String>,
        parts: Vec<ReasoningPlan>,
    },
    ToolCall {
        native_index: usize,
        call: ToolCall,
        stream: u32,
    },
    Media(MediaPlan),
}

#[derive(Clone, Debug)]
enum ReasoningPlan {
    Text {
        stream: u32,
        signature: Option<String>,
    },
    Encrypted {
        stream: u32,
    },
    Redacted {
        stream: u32,
    },
    Summary {
        stream: u32,
    },
}

#[derive(Clone, Debug)]
struct MediaPlan {
    kind: MediaKind,
    data: MediaDataPlan,
    media_type: Option<MediaType>,
    detail: Option<gents_protocol::message::ImageDetail>,
    additional_params: Option<serde_json::Value>,
}

#[derive(Clone, Debug)]
enum MediaDataPlan {
    Url(String),
    Base64(u32),
    Raw(u32),
    String(u32),
    Unknown,
}

#[derive(Clone, Debug)]
pub(crate) struct EncodedNativeMessage {
    pub(crate) role: MessageRole,
    pub(crate) native_id: Option<String>,
    pub(crate) streams: Vec<EncodedStream>,
    plans: Vec<BlockPlan>,
    pub(crate) tool_calls: Vec<EncodedToolCall>,
}

impl EncodedNativeMessage {
    pub(crate) fn build_blocks(
        &self,
        close_doc_id: &str,
        tool_doc_ids: &[String],
    ) -> Result<Vec<MessageBlock>> {
        anyhow::ensure!(!close_doc_id.trim().is_empty(), "blank closing identity");
        anyhow::ensure!(
            tool_doc_ids.len() == self.tool_calls.len(),
            "tool identity count changed"
        );
        let reference = |stream| PayloadRef {
            close_doc_id: close_doc_id.to_owned(),
            stream,
        };
        self.plans
            .iter()
            .map(|plan| {
                Ok(match plan {
                    BlockPlan::Text { stream } => MessageBlock::Text {
                        text: PresentedPayload {
                            output: reference(*stream),
                            presentation: PayloadPresentation::Full,
                        },
                    },
                    BlockPlan::Reasoning { id, parts } => MessageBlock::Reasoning {
                        id: id.clone(),
                        parts: parts
                            .iter()
                            .map(|part| match part {
                                ReasoningPlan::Text { stream, signature } => ReasoningPart::Text {
                                    text: reference(*stream),
                                    signature: signature.clone(),
                                },
                                ReasoningPlan::Encrypted { stream } => ReasoningPart::Encrypted {
                                    data: reference(*stream),
                                },
                                ReasoningPlan::Redacted { stream } => ReasoningPart::Redacted {
                                    data: reference(*stream),
                                },
                                ReasoningPlan::Summary { stream } => ReasoningPart::Summary {
                                    text: reference(*stream),
                                },
                            })
                            .collect(),
                    },
                    BlockPlan::ToolCall {
                        native_index,
                        call,
                        stream,
                    } => MessageBlock::ToolCall {
                        tool_call_doc_id: tool_doc_ids
                            .get(*native_index)
                            .context("tool identity missing")?
                            .clone(),
                        id: call.id.clone(),
                        call_id: call.call_id.clone(),
                        name: call.function.name.clone(),
                        arguments: reference(*stream),
                        signature: call.signature.clone(),
                        additional_params: call.additional_params.clone(),
                    },
                    BlockPlan::Media(media) => MessageBlock::Media(media.materialize(close_doc_id)),
                })
            })
            .collect()
    }
}

impl MediaPlan {
    fn materialize(&self, close_doc_id: &str) -> MediaBlock {
        let reference = |stream| PayloadRef {
            close_doc_id: close_doc_id.to_owned(),
            stream,
        };
        let data = match &self.data {
            MediaDataPlan::Url(url) => MediaData::Url { url: url.clone() },
            MediaDataPlan::Base64(stream) => MediaData::Base64 {
                data: reference(*stream),
            },
            MediaDataPlan::Raw(stream) => MediaData::Raw {
                data: reference(*stream),
            },
            MediaDataPlan::String(stream) => MediaData::String {
                data: reference(*stream),
            },
            MediaDataPlan::Unknown => MediaData::Unknown,
        };
        MediaBlock {
            kind: self.kind,
            data,
            media_type: self.media_type.clone(),
            detail: self.detail.clone(),
            additional_params: self.additional_params.clone(),
        }
    }
}

fn push_stream(
    streams: &mut Vec<EncodedStream>,
    block_index: u32,
    part_index: u32,
    payload_kind: StreamPayload,
    payload: String,
) -> u32 {
    let stream = streams.len() as u32;
    streams.push(EncodedStream {
        declaration: StreamDeclaration {
            block_index,
            part_index,
            payload: payload_kind,
        },
        payload,
    });
    stream
}

fn media_plan(
    streams: &mut Vec<EncodedStream>,
    block_index: u32,
    kind: MediaKind,
    data: &DocumentSourceKind,
    media_type: Option<MediaType>,
    detail: Option<gents_protocol::message::ImageDetail>,
    additional_params: Option<serde_json::Value>,
) -> MediaPlan {
    let (data, payload) = match data {
        DocumentSourceKind::Url(url) => (MediaDataPlan::Url(url.clone()), None),
        DocumentSourceKind::Base64(value) => {
            (MediaDataPlan::Base64(0), Some((value.clone(), false)))
        }
        DocumentSourceKind::Raw(value) => (
            MediaDataPlan::Raw(0),
            Some((
                base64::engine::general_purpose::STANDARD.encode(value),
                true,
            )),
        ),
        DocumentSourceKind::String(value) => {
            (MediaDataPlan::String(0), Some((value.clone(), false)))
        }
        DocumentSourceKind::Unknown => (MediaDataPlan::Unknown, None),
    };
    let data = if let Some((value, _raw)) = payload {
        let stream = push_stream(
            streams,
            block_index,
            0,
            StreamPayload::Media { media_kind: kind },
            value,
        );
        match data {
            MediaDataPlan::Base64(_) => MediaDataPlan::Base64(stream),
            MediaDataPlan::Raw(_) => MediaDataPlan::Raw(stream),
            MediaDataPlan::String(_) => MediaDataPlan::String(stream),
            _ => unreachable!(),
        }
    } else {
        data
    };
    MediaPlan {
        kind,
        data,
        media_type,
        detail,
        additional_params,
    }
}

pub(crate) fn encode_native_message(message: &Message) -> Result<EncodedNativeMessage> {
    if let Message::User { content } = message {
        let mut streams = Vec::new();
        let mut plans = Vec::new();
        for (block, item) in content.iter().enumerate() {
            let gents_protocol::message::UserContent::Text(text) = item else {
                anyhow::bail!("authored publication currently requires native user text blocks")
            };
            let stream = push_stream(
                &mut streams,
                u32::try_from(block)?,
                0,
                StreamPayload::Text,
                text.text.clone(),
            );
            plans.push(BlockPlan::Text { stream });
        }
        anyhow::ensure!(!plans.is_empty(), "native user message has no blocks");
        return Ok(EncodedNativeMessage {
            role: MessageRole::User,
            native_id: None,
            streams,
            plans,
            tool_calls: Vec::new(),
        });
    }
    let Message::Assistant { id, content } = message else {
        anyhow::bail!("unsupported native authored role")
    };
    let mut streams = Vec::new();
    let mut plans = Vec::new();
    let mut tool_calls = Vec::new();
    for (block, content) in content.iter().enumerate() {
        let block = u32::try_from(block).context("native block index exceeds u32")?;
        match content {
            AssistantContent::Text(text) => {
                let stream = push_stream(
                    &mut streams,
                    block,
                    0,
                    StreamPayload::Text,
                    text.text.clone(),
                );
                plans.push(BlockPlan::Text { stream });
            }
            AssistantContent::Reasoning(reasoning) => {
                let mut parts = Vec::new();
                for (part, value) in reasoning.content.iter().enumerate() {
                    let part = u32::try_from(part).context("reasoning part index exceeds u32")?;
                    parts.push(match value {
                        ReasoningContent::Text { text, signature } => {
                            let stream = push_stream(
                                &mut streams,
                                block,
                                part,
                                StreamPayload::Reasoning,
                                text.clone(),
                            );
                            if let Some(signature) = signature {
                                push_stream(
                                    &mut streams,
                                    block,
                                    part,
                                    StreamPayload::ReasoningSignature,
                                    signature.clone(),
                                );
                            }
                            ReasoningPlan::Text {
                                stream,
                                signature: signature.clone(),
                            }
                        }
                        ReasoningContent::Encrypted(data) => ReasoningPlan::Encrypted {
                            stream: push_stream(
                                &mut streams,
                                block,
                                part,
                                StreamPayload::ReasoningEncrypted,
                                data.clone(),
                            ),
                        },
                        ReasoningContent::Redacted { data } => ReasoningPlan::Redacted {
                            stream: push_stream(
                                &mut streams,
                                block,
                                part,
                                StreamPayload::ReasoningRedacted,
                                data.clone(),
                            ),
                        },
                        ReasoningContent::Summary(text) => ReasoningPlan::Summary {
                            stream: push_stream(
                                &mut streams,
                                block,
                                part,
                                StreamPayload::ReasoningSummary,
                                text.clone(),
                            ),
                        },
                    });
                }
                plans.push(BlockPlan::Reasoning {
                    id: reasoning.id.clone(),
                    parts,
                });
            }
            AssistantContent::ToolCall(call) => {
                let arguments = serde_json::to_string(&call.function.arguments)
                    .context("encoding exact native tool arguments")?;
                let stream = push_stream(
                    &mut streams,
                    block,
                    0,
                    StreamPayload::ToolArguments {
                        id: call.id.clone(),
                        call_id: call.call_id.clone(),
                        name: call.function.name.clone(),
                    },
                    arguments,
                );
                let native_index = tool_calls.len();
                tool_calls.push(EncodedToolCall {
                    native_index,
                    id: call.id.clone(),
                    call_id: call.call_id.clone(),
                    name: call.function.name.clone(),
                    arguments_stream: stream,
                });
                plans.push(BlockPlan::ToolCall {
                    native_index,
                    call: call.clone(),
                    stream,
                });
            }
            AssistantContent::Image(image) => plans.push(BlockPlan::Media(media_plan(
                &mut streams,
                block,
                MediaKind::Image,
                &image.data,
                image.media_type.clone().map(MediaType::Image),
                image.detail.clone(),
                image.additional_params.clone(),
            ))),
        }
    }
    anyhow::ensure!(!plans.is_empty(), "native assistant message has no blocks");
    Ok(EncodedNativeMessage {
        role: MessageRole::Assistant,
        native_id: id.clone(),
        streams,
        plans,
        tool_calls,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents_protocol::message::{Reasoning, ToolFunction};
    use gents_protocol::output::reconstruction::{reconstruct_message, ObservedSegment};
    use gents_protocol::output::{
        MessagePublication, OutputOutcome, OutputSegment, OutputSource, OutputWriter, SegmentRun,
        SourceClose, TranscriptMessage,
    };

    #[test]
    fn assistant_encoding_round_trips_exact_native_structure() {
        let native = Message::Assistant {
            id: Some("provider-α".into()),
            content: vec![
                AssistantContent::Reasoning(Reasoning {
                    id: Some("reasoning-1".into()),
                    content: vec![
                        ReasoningContent::Text {
                            text: "考える".into(),
                            signature: Some("signed".into()),
                        },
                        ReasoningContent::Encrypted("opaque-1".into()),
                        ReasoningContent::Redacted {
                            data: "opaque-2".into(),
                        },
                        ReasoningContent::Summary("".into()),
                    ],
                }),
                AssistantContent::Text(gents_protocol::message::Text { text: "".into() }),
                AssistantContent::ToolCall(ToolCall {
                    id: "tool-一".into(),
                    call_id: Some("call-a".into()),
                    function: ToolFunction::new("read".into(), serde_json::json!({"path":"é"})),
                    signature: Some("tool-sig".into()),
                    additional_params: Some(serde_json::json!({"vendor":true})),
                }),
                AssistantContent::Text(gents_protocol::message::Text {
                    text: "done ✓".into(),
                }),
                AssistantContent::ToolCall(ToolCall::new(
                    "tool-2".into(),
                    ToolFunction::new("empty".into(), serde_json::json!({})),
                )),
            ],
        };
        let encoded = encode_native_message(&native).unwrap();
        let tool_ids = vec!["tool-doc-1".into(), "tool-doc-2".into()];
        let blocks = encoded.build_blocks("close-1", &tool_ids).unwrap();
        let mut payload = String::new();
        let mut runs = Vec::new();
        let mut stream_bytes = Vec::new();
        for (stream, encoded_stream) in encoded.streams.iter().enumerate() {
            payload.push_str(&encoded_stream.payload);
            stream_bytes.push(encoded_stream.payload.len() as u64);
            runs.push(SegmentRun {
                stream: stream as u32,
                bytes: encoded_stream.payload.len() as u32,
                declaration: Some(encoded_stream.declaration.clone()),
            });
        }
        let source = OutputSource::ProviderTurn {
            scope: "inference.1".parse().unwrap(),
            turn_index: 0,
            attempt: 0,
        };
        let segment = OutputSegment {
            agent_did: "did:key:agent".into(),
            requester_did: Some("did:key:user".into()),
            session_id: "session".into(),
            request_doc_id: "request".into(),
            source,
            writer: OutputWriter::RequestExecution {
                execution_generation: "generation".into(),
            },
            ordinal: Some(0),
            runs,
            payload,
            close: Some(SourceClose::Closed {
                outcome: OutputOutcome::Complete,
                segments: 1,
                stream_bytes,
            }),
            created_at: "2026-01-01T00:00:00Z".into(),
        };
        let header = TranscriptMessage {
            message_key: "provider-turn".into(),
            session_id: "session".into(),
            agent_did: "did:key:agent".into(),
            requester_did: Some("did:key:user".into()),
            request_doc_id: Some("request".into()),
            publication: MessagePublication::RequestExecution {
                execution_generation: "generation".into(),
            },
            outcome: OutputOutcome::Complete,
            sequence: 1,
            role: encoded.role,
            native_id: encoded.native_id.clone(),
            blocks,
            created_at: segment.created_at.clone(),
        };
        let observed = [ObservedSegment {
            doc_id: "close-1",
            segment: &segment,
        }];
        assert_eq!(
            reconstruct_message(&observed, &[], &[], &header).unwrap(),
            native
        );
    }
}
