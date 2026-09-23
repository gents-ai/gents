//! Representation bridge for model-derived payload presentation cases.
//! Reconstruction and admission policy remain in `gents_protocol::output`.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use gents_protocol::message::{
    AssistantContent, DocumentSourceKind, Message, ReasoningContent, ToolResultContent, UserContent,
};
use gents_protocol::output::reconstruction::{
    reconstruct_message, reconstruct_stream, ObservedSegment,
};
use gents_protocol::output::{
    MediaBlock, MediaData, MediaKind, MessageBlock, MessagePublication, MessageRole, OutputOutcome,
    OutputSegment, OutputSource, OutputWriter, PayloadPresentation, PayloadRef, PresentationPart,
    PresentedPayload, ReasoningPart, SegmentRun, SourceClose, StreamDeclaration, StreamPayload,
    ToolResultPart, TranscriptMessage,
};

use super::super::{
    ExecutionFuture, LeanCanonicalClosure, LeanCanonicalMessage, LeanCanonicalSegment,
    LeanCanonicalSource, LeanCanonicalWriter, LeanMedia, LeanMediaData, LeanMediaKind,
    LeanMessageBlock, LeanMessagePublication, LeanMessageRole, LeanOutcome, LeanPayloadKind,
    LeanPayloadSpec, LeanPresentation, LeanPresentationPart, LeanReasoningPart, LeanResultPart,
};
use super::{assert_native_payload_presentation_case, PayloadPresentationAdapter};

const FIXTURE_EPOCH_SECONDS: i64 = 1_700_000_000;

fn fixture_time(value: u64) -> Result<String> {
    let seconds = FIXTURE_EPOCH_SECONDS
        .checked_add(i64::try_from(value)?)
        .context("modeled timestamp exceeds native range")?;
    Ok(DateTime::<Utc>::from_timestamp(seconds, 0)
        .context("modeled timestamp overflows native date")?
        .to_rfc3339())
}

fn request_id(value: u64) -> String {
    format!("lean-request-{value}")
}
fn session_id(value: u64) -> String {
    format!("lean-session-{value}")
}
fn segment_id(value: u64) -> String {
    format!("lean-segment-{value}")
}
fn tool_id(value: u64) -> String {
    format!("lean-tool-{value}")
}
fn generation(value: u64) -> String {
    format!("lean-generation-{value}")
}

fn reference(value: &super::super::LeanPayloadRef) -> Result<PayloadRef> {
    Ok(PayloadRef {
        close_doc_id: segment_id(value.close_id),
        stream: u32::try_from(value.stream)?,
    })
}

fn full_reference(value: &LeanPayloadSpec) -> Result<PayloadRef> {
    anyhow::ensure!(
        matches!(&value.presentation, LeanPresentation::Full),
        "native payload field has no composed presentation"
    );
    reference(&value.reference)
}

fn presentation(value: &LeanPayloadSpec) -> Result<PresentedPayload> {
    Ok(PresentedPayload {
        output: reference(&value.reference)?,
        presentation: match &value.presentation {
            LeanPresentation::Full => PayloadPresentation::Full,
            LeanPresentation::Composed { parts } => PayloadPresentation::Composed {
                parts: parts
                    .iter()
                    .map(|part| -> Result<PresentationPart> {
                        Ok(match part {
                            LeanPresentationPart::Range { start, end } => {
                                PresentationPart::OutputRange {
                                    start_byte: *start,
                                    end_byte: *end,
                                }
                            }
                            LeanPresentationPart::Literal { bytes } => PresentationPart::Literal {
                                text: String::from_utf8(bytes.clone())?,
                            },
                        })
                    })
                    .collect::<Result<Vec<_>>>()?,
            },
        },
    })
}

fn source(value: &LeanCanonicalSource) -> Result<OutputSource> {
    Ok(match value {
        LeanCanonicalSource::Provider {
            scope,
            turn,
            attempt,
        } => OutputSource::ProviderTurn {
            scope: format!("inference.{scope}").parse()?,
            turn_index: u32::try_from(*turn)?,
            attempt: u32::try_from(*attempt)?,
        },
        LeanCanonicalSource::Tool { call } => OutputSource::ToolCall {
            tool_call_doc_id: tool_id(*call),
        },
        LeanCanonicalSource::Authored { key } => OutputSource::Authored {
            key: format!("lean-authored-{key}"),
        },
    })
}

fn media_kind(value: &LeanMediaKind) -> MediaKind {
    match value {
        LeanMediaKind::Image => MediaKind::Image,
        LeanMediaKind::Audio => MediaKind::Audio,
        LeanMediaKind::Video => MediaKind::Video,
        LeanMediaKind::Document => MediaKind::Document,
    }
}

fn segment(value: &LeanCanonicalSegment) -> Result<OutputSegment> {
    let (ordinal, runs, payload) = match &value.flush {
        None => (None, Vec::new(), String::new()),
        Some(flush) => (
            Some(u32::try_from(flush.ordinal)?),
            flush
                .runs
                .iter()
                .map(|run| -> Result<SegmentRun> {
                    let declaration = run
                        .declaration
                        .as_ref()
                        .map(|decl| -> Result<StreamDeclaration> {
                            let payload = match decl.kind {
                                LeanPayloadKind::Text => StreamPayload::Text,
                                LeanPayloadKind::Reasoning => StreamPayload::Reasoning,
                                LeanPayloadKind::Summary => StreamPayload::ReasoningSummary,
                                LeanPayloadKind::Opaque => StreamPayload::ReasoningOpaque,
                                LeanPayloadKind::Arguments => {
                                    let tool = decl
                                        .tool
                                        .as_ref()
                                        .context("argument declaration lacks tool identity")?;
                                    StreamPayload::ToolArguments {
                                        id: tool.id.clone(),
                                        call_id: tool.call_id.clone(),
                                        name: tool.name.clone(),
                                    }
                                }
                                LeanPayloadKind::ToolOutput => StreamPayload::ToolOutput,
                                LeanPayloadKind::Media => StreamPayload::Media {
                                    media_kind: media_kind(
                                        decl.media_kind
                                            .as_ref()
                                            .context("media declaration lacks kind")?,
                                    ),
                                },
                            };
                            Ok(StreamDeclaration {
                                block_index: u32::try_from(decl.block)?,
                                part_index: u32::try_from(decl.part)?,
                                payload,
                            })
                        })
                        .transpose()?;
                    Ok(SegmentRun {
                        stream: u32::try_from(run.stream)?,
                        bytes: u32::try_from(run.bytes)?,
                        declaration,
                    })
                })
                .collect::<Result<Vec<_>>>()?,
            String::from_utf8(flush.payload.clone())?,
        ),
    };
    Ok(OutputSegment {
        agent_did: "did:example:lean".into(),
        requester_did: None,
        session_id: "lean-session-1".into(),
        request_doc_id: request_id(value.coordinate.request),
        source: source(&value.coordinate.source)?,
        writer: match &value.writer {
            LeanCanonicalWriter::Request { generation: owner } => OutputWriter::RequestExecution {
                execution_generation: generation(*owner),
            },
            LeanCanonicalWriter::Tool { call } => OutputWriter::ToolExecution {
                tool_call_doc_id: tool_id(*call),
            },
        },
        ordinal,
        runs,
        payload,
        close: value
            .close
            .as_ref()
            .map(|close| -> Result<SourceClose> {
                Ok(match close {
                    LeanCanonicalClosure::Retracted => SourceClose::Retracted,
                    LeanCanonicalClosure::Closed {
                        outcome,
                        segments,
                        stream_bytes,
                    } => SourceClose::Closed {
                        outcome: outcome_value(outcome),
                        segments: u32::try_from(*segments)?,
                        stream_bytes: stream_bytes.clone(),
                    },
                })
            })
            .transpose()?,
        created_at: fixture_time(value.created_at)?,
    })
}

fn outcome_value(value: &LeanOutcome) -> OutputOutcome {
    match value {
        LeanOutcome::Complete => OutputOutcome::Complete,
        LeanOutcome::Partial => OutputOutcome::Partial,
    }
}

fn media(value: &LeanMedia<LeanPayloadSpec>) -> Result<MediaBlock> {
    let kind = media_kind(&value.kind);
    let data = match &value.data {
        LeanMediaData::Url { url } => MediaData::Url { url: url.clone() },
        LeanMediaData::Base64 { payload } => MediaData::Base64 {
            data: full_reference(payload)?,
        },
        LeanMediaData::Raw { payload } => MediaData::Raw {
            data: full_reference(payload)?,
        },
        LeanMediaData::String { payload } => MediaData::String {
            data: full_reference(payload)?,
        },
        LeanMediaData::Unknown => MediaData::Unknown,
    };
    // Native media types are typed by media kind; decoding a modeled MIME
    // string is representation translation, never a validation fallback.
    let media_type = value
        .media_type
        .as_deref()
        .map(|value| -> Result<_> {
            let suffix = value.split_once('/').map_or(value, |(_, suffix)| suffix);
            let literal = serde_json::Value::String(suffix.to_ascii_lowercase());
            Ok(match kind {
                MediaKind::Image => {
                    gents_protocol::output::MediaType::Image(serde_json::from_value(literal)?)
                }
                MediaKind::Audio => {
                    gents_protocol::output::MediaType::Audio(serde_json::from_value(literal)?)
                }
                MediaKind::Video => {
                    gents_protocol::output::MediaType::Video(serde_json::from_value(literal)?)
                }
                MediaKind::Document => {
                    gents_protocol::output::MediaType::Document(serde_json::from_value(literal)?)
                }
            })
        })
        .transpose()?;
    let detail = value
        .detail
        .as_ref()
        .map(|text| serde_json::from_value(serde_json::Value::String(text.clone())))
        .transpose()?;
    Ok(MediaBlock {
        kind,
        data,
        media_type,
        detail,
        additional_params: value
            .additional_params
            .as_deref()
            .map(serde_json::from_str)
            .transpose()?,
    })
}

fn block(value: &LeanMessageBlock<LeanPayloadSpec>) -> Result<MessageBlock> {
    Ok(match value {
        LeanMessageBlock::Text { payload } => MessageBlock::Text {
            text: presentation(payload)?,
        },
        LeanMessageBlock::Reasoning { id, parts } => MessageBlock::Reasoning {
            id: id.clone(),
            parts: parts
                .iter()
                .map(|part| -> Result<ReasoningPart> {
                    Ok(match part {
                        LeanReasoningPart::Text { payload, signature } => ReasoningPart::Text {
                            text: full_reference(payload)?,
                            signature: signature.clone(),
                        },
                        LeanReasoningPart::Encrypted { payload } => ReasoningPart::Encrypted {
                            data: full_reference(payload)?,
                        },
                        LeanReasoningPart::Redacted { payload } => ReasoningPart::Redacted {
                            data: full_reference(payload)?,
                        },
                        LeanReasoningPart::Summary { payload } => ReasoningPart::Summary {
                            text: full_reference(payload)?,
                        },
                    })
                })
                .collect::<Result<Vec<_>>>()?,
        },
        LeanMessageBlock::ToolCall {
            doc_id,
            id,
            call_id,
            name,
            arguments,
            signature,
            additional_params,
        } => MessageBlock::ToolCall {
            tool_call_doc_id: tool_id(*doc_id),
            id: id.clone(),
            call_id: call_id.clone(),
            name: name.clone(),
            arguments: full_reference(arguments)?,
            signature: signature.clone(),
            additional_params: additional_params
                .as_deref()
                .map(serde_json::from_str)
                .transpose()?,
        },
        LeanMessageBlock::ToolResult {
            doc_id,
            id,
            call_id,
            parts,
        } => MessageBlock::ToolResult {
            tool_call_doc_id: tool_id(*doc_id),
            id: id.clone(),
            call_id: call_id.clone(),
            parts: parts
                .iter()
                .map(|part| -> Result<ToolResultPart> {
                    Ok(match part {
                        LeanResultPart::Text { payload } => ToolResultPart::Text {
                            text: presentation(payload)?,
                        },
                        LeanResultPart::Media { value } => ToolResultPart::Media(media(value)?),
                    })
                })
                .collect::<Result<Vec<_>>>()?,
        },
        LeanMessageBlock::Media { value } => MessageBlock::Media(media(value)?),
    })
}

fn message(value: &LeanCanonicalMessage<LeanPayloadSpec>) -> Result<TranscriptMessage> {
    match (&value.header.publication, value.header.origin) {
        (LeanMessagePublication::Fork { origin }, Some(header_origin))
            if *origin == header_origin => {}
        (LeanMessagePublication::Fork { .. }, _) => {
            anyhow::bail!("fork origin differs from header origin")
        }
        (_, None) => {}
        (_, Some(_)) => anyhow::bail!("non-fork header carries origin"),
    }
    let publication = match &value.header.publication {
        LeanMessagePublication::RequestExecution { generation: owner } => {
            MessagePublication::RequestExecution {
                execution_generation: generation(*owner),
            }
        }
        LeanMessagePublication::RequestRecovery { generation: owner } => {
            MessagePublication::RequestRecovery {
                execution_generation: generation(*owner),
            }
        }
        LeanMessagePublication::ToolDelivery { call } => MessagePublication::ToolDelivery {
            tool_call_doc_id: tool_id(*call),
        },
        LeanMessagePublication::Fork { origin } => MessagePublication::Fork {
            origin_message_doc_id: format!("lean-message-{origin}"),
        },
    };
    let blocks = value.blocks.iter().map(block).collect::<Result<Vec<_>>>()?;
    let converted = TranscriptMessage {
        message_key: value.key.clone(),
        session_id: session_id(value.header.session),
        agent_did: "did:example:lean".into(),
        requester_did: None,
        request_doc_id: value.header.request.map(request_id),
        publication,
        outcome: outcome_value(&value.header.outcome),
        sequence: u32::try_from(
            value
                .sequence
                .checked_add(1)
                .context("modeled sequence overflow")?,
        )?,
        role: match &value.header.role {
            LeanMessageRole::System => MessageRole::System,
            LeanMessageRole::User => MessageRole::User,
            LeanMessageRole::Assistant => MessageRole::Assistant,
        },
        native_id: value.native_id.clone(),
        blocks,
        created_at: fixture_time(value.created_at)?,
    };
    let modeled_refs = value
        .header
        .refs
        .iter()
        .map(reference)
        .collect::<Result<Vec<_>>>()?;
    let native_refs = converted
        .payload_references()
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();
    anyhow::ensure!(
        modeled_refs == native_refs,
        "modeled header refs differ from native block refs"
    );
    Ok(converted)
}

fn data_bytes(value: &DocumentSourceKind) -> usize {
    match value {
        DocumentSourceKind::Url(_) | DocumentSourceKind::Unknown => 0,
        DocumentSourceKind::Base64(text) | DocumentSourceKind::String(text) => text.len(),
        DocumentSourceKind::Raw(bytes) => bytes.len(),
    }
}

fn presented_bytes(value: &Message) -> Result<u64> {
    let bytes = match value {
        Message::System { content } => content.len(),
        Message::Assistant { content, .. } => content
            .iter()
            .map(|part| -> Result<usize> {
                Ok(match part {
                    AssistantContent::Text(text) => text.text.len(),
                    AssistantContent::Reasoning(reasoning) => reasoning
                        .content
                        .iter()
                        .map(|part| match part {
                            ReasoningContent::Text { text, .. }
                            | ReasoningContent::Summary(text)
                            | ReasoningContent::Encrypted(text) => text.len(),
                            ReasoningContent::Redacted { data } => data.len(),
                        })
                        .sum(),
                    // `reconstruct_message` has validated this native field,
                    // but parses its JSON into a Value. Count the sealed source
                    // bytes below, so whitespace and key order are not lost.
                    AssistantContent::ToolCall(_) => 0,
                    AssistantContent::Image(image) => data_bytes(&image.data),
                })
            })
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .sum(),
        Message::User { content } => content
            .iter()
            .map(|part| match part {
                UserContent::Text(text) => text.text.len(),
                UserContent::ToolResult(result) => result
                    .content
                    .iter()
                    .map(|part| match part {
                        ToolResultContent::Text(text) => text.text.len(),
                        ToolResultContent::Image(image) => data_bytes(&image.data),
                    })
                    .sum(),
                UserContent::Image(image) => data_bytes(&image.data),
                UserContent::Audio(audio) => data_bytes(&audio.data),
                UserContent::Video(video) => data_bytes(&video.data),
                UserContent::Document(document) => data_bytes(&document.data),
            })
            .sum(),
    };
    Ok(u64::try_from(bytes)?)
}

fn tool_argument_bytes(
    observations: &[ObservedSegment<'_>],
    envelope: &TranscriptMessage,
) -> Result<u64> {
    envelope
        .blocks
        .iter()
        .try_fold(0_u64, |total, block| -> Result<u64> {
            let MessageBlock::ToolCall { arguments, .. } = block else {
                return Ok(total);
            };
            let stream = reconstruct_stream(observations, &[], &[], arguments)?;
            total
                .checked_add(u64::try_from(stream.text.len())?)
                .context("presented argument length overflow")
        })
}

#[derive(Default)]
pub(crate) struct NativePayloadPresentationAdapter;

impl PayloadPresentationAdapter for NativePayloadPresentationAdapter {
    type Error = anyhow::Error;

    fn presented_payload_bytes<'a>(
        &'a mut self,
        segments: &'a [LeanCanonicalSegment],
        envelope: &'a LeanCanonicalMessage<LeanPayloadSpec>,
    ) -> ExecutionFuture<'a, Result<Option<u64>, Self::Error>> {
        Box::pin(async move {
            let segments = segments
                .iter()
                .map(|record| Ok((segment_id(record.id), segment(record)?)))
                .collect::<Result<Vec<_>>>()?;
            let observations = segments
                .iter()
                .map(|(doc_id, segment)| ObservedSegment { doc_id, segment })
                .collect::<Vec<_>>();
            let envelope = message(envelope)?;
            match reconstruct_message(&observations, &[], &[], &envelope) {
                Ok(native) => {
                    let bytes = presented_bytes(&native)?
                        .checked_add(tool_argument_bytes(&observations, &envelope)?)
                        .context("presented payload length overflow")?;
                    Ok(Some(bytes))
                }
                Err(_) => Ok(None),
            }
        })
    }
}

#[tokio::test]
async fn generated_payload_presentation_cases_use_native_reconstruction() {
    let cases = super::super::lean_canonical_payload_presentation_cases();
    assert!(
        !cases.is_empty(),
        "generated payload presentation inventory is empty"
    );
    let mut adapter = NativePayloadPresentationAdapter;
    for case in cases {
        assert_native_payload_presentation_case(case, &mut adapter)
            .await
            .unwrap_or_else(|error| panic!("{error}"));
    }
}
