use gents_lean_vocab::{
    LeanAuxiliaryKind, LeanAuxiliaryOutputCase, LeanCanonicalClosure, LeanCanonicalMessage,
    LeanCanonicalOutputView as View, LeanCanonicalSegment, LeanCanonicalSource,
    LeanCanonicalWriter, LeanMedia, LeanMediaData, LeanMediaKind, LeanMessageBlock,
    LeanMessagePublication, LeanMessageRole, LeanOutcome, LeanPayloadKind, LeanPayloadSpec,
    LeanPresentation, LeanPresentationPart, LeanReasoningAuditResult, LeanReasoningPart,
    LeanResultPart,
};
use gents_protocol::output::live::{
    project_live, reconstruct_dense_prefix, LiveObservation, LiveTarget, LiveView, OwnerLiveness,
};
use gents_protocol::output::reconstruction::{DependencyDenial, ObservedSegment};
use gents_protocol::output::recovery::plan_recovery_prefix;
use gents_protocol::output::{
    MediaBlock, MediaData, MediaKind, MessageBlock, MessagePublication, MessageRole, OutputOutcome,
    OutputSegment, OutputSource, OutputWriter, PayloadPresentation, PayloadRef, PresentationPart,
    PresentedPayload, ReasoningPart, SegmentRun, SourceClose, StreamDeclaration, StreamPayload,
    TerminalOutput, ToolResultPart, TranscriptMessage,
};
use gents_protocol::rendered_request::{CaptureScope, CaptureScopeKind};

const NODE_DID: &str = "did:key:z6MkLeanConformanceAgent";

fn nat(id: u64) -> String {
    format!("nat-{id}")
}

fn rfc3339(seconds: u64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(
        i64::try_from(seconds).expect("generated timestamp fits i64"),
        0,
    )
    .expect("generated timestamp is representable")
    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn lean_outcome(outcome: &LeanOutcome) -> OutputOutcome {
    match outcome {
        LeanOutcome::Complete => OutputOutcome::Complete,
        LeanOutcome::Partial => OutputOutcome::Partial,
    }
}

fn lean_source(source: &LeanCanonicalSource) -> OutputSource {
    match source {
        LeanCanonicalSource::Provider {
            scope,
            turn,
            attempt,
        } => OutputSource::ProviderTurn {
            scope: CaptureScope {
                kind: CaptureScopeKind::Inference,
                seq: scope + 1,
            },
            turn_index: u32::try_from(*turn).expect("generated turn fits u32"),
            attempt: u32::try_from(*attempt).expect("generated attempt fits u32"),
        },
        LeanCanonicalSource::Auxiliary {
            auxiliary_kind,
            scope,
            turn,
            attempt,
        } => OutputSource::ProviderTurn {
            scope: CaptureScope {
                kind: match auxiliary_kind {
                    LeanAuxiliaryKind::Compaction => CaptureScopeKind::Compaction,
                    LeanAuxiliaryKind::CompactionFallback => CaptureScopeKind::CompactionFallback,
                    LeanAuxiliaryKind::Title => CaptureScopeKind::Title,
                    LeanAuxiliaryKind::AccessReview => CaptureScopeKind::AccessReview,
                },
                seq: *scope,
            },
            turn_index: u32::try_from(*turn).expect("generated turn fits u32"),
            attempt: u32::try_from(*attempt).expect("generated attempt fits u32"),
        },
        LeanCanonicalSource::Tool { call } => OutputSource::ToolCall {
            tool_call_doc_id: nat(*call),
        },
        LeanCanonicalSource::Authored { key } => OutputSource::Authored { key: nat(*key) },
    }
}

fn lean_writer(writer: &LeanCanonicalWriter) -> OutputWriter {
    match writer {
        LeanCanonicalWriter::Request { generation } => OutputWriter::RequestExecution {
            execution_generation: nat(*generation),
        },
        LeanCanonicalWriter::Tool { call } => OutputWriter::ToolExecution {
            tool_call_doc_id: nat(*call),
        },
    }
}

fn lean_publication(publication: &LeanMessagePublication) -> MessagePublication {
    match publication {
        LeanMessagePublication::RequestExecution { generation } => {
            MessagePublication::RequestExecution {
                execution_generation: nat(*generation),
            }
        }
        LeanMessagePublication::RequestRecovery { generation } => {
            MessagePublication::RequestRecovery {
                execution_generation: nat(*generation),
            }
        }
        LeanMessagePublication::ToolDelivery { call } => MessagePublication::ToolDelivery {
            tool_call_doc_id: nat(*call),
        },
        LeanMessagePublication::Fork { origin } => MessagePublication::Fork {
            origin_message_doc_id: nat(*origin),
        },
    }
}

fn lean_role(role: &LeanMessageRole) -> MessageRole {
    match role {
        LeanMessageRole::System => MessageRole::System,
        LeanMessageRole::User => MessageRole::User,
        LeanMessageRole::Assistant => MessageRole::Assistant,
    }
}

fn lean_payload_kind(
    kind: &LeanPayloadKind,
    tool: Option<&gents_lean_vocab::LeanToolIdentity>,
    media_kind: Option<&LeanMediaKind>,
) -> StreamPayload {
    match kind {
        LeanPayloadKind::Text => StreamPayload::Text,
        LeanPayloadKind::Reasoning => StreamPayload::Reasoning,
        LeanPayloadKind::Signature => StreamPayload::ReasoningSignature,
        LeanPayloadKind::ReasoningStop => StreamPayload::ReasoningStop,
        LeanPayloadKind::Summary => StreamPayload::ReasoningSummary,
        LeanPayloadKind::Encrypted => StreamPayload::ReasoningEncrypted,
        LeanPayloadKind::Redacted => StreamPayload::ReasoningRedacted,
        LeanPayloadKind::Arguments => StreamPayload::ToolArguments {
            id: tool
                .expect("generated arguments run carries tool identity")
                .id
                .clone(),
            call_id: tool.and_then(|tool| tool.call_id.clone()),
            name: tool
                .expect("generated arguments run carries tool identity")
                .name
                .clone(),
        },
        LeanPayloadKind::ToolOutput => StreamPayload::ToolOutput,
        LeanPayloadKind::Media => StreamPayload::Media {
            media_kind: lean_media_kind(media_kind.expect("generated media run carries a kind")),
        },
    }
}

fn lean_media_kind(kind: &LeanMediaKind) -> MediaKind {
    match kind {
        LeanMediaKind::Image => MediaKind::Image,
        LeanMediaKind::Audio => MediaKind::Audio,
        LeanMediaKind::Video => MediaKind::Video,
        LeanMediaKind::Document => MediaKind::Document,
    }
}

fn lean_payload_ref(close_id: u64, stream: u64) -> PayloadRef {
    PayloadRef {
        close_doc_id: nat(close_id),
        stream: u32::try_from(stream).expect("generated stream index fits u32"),
    }
}

fn lean_presentation(presentation: &LeanPresentation) -> PayloadPresentation {
    match presentation {
        LeanPresentation::Full => PayloadPresentation::Full,
        LeanPresentation::Composed { parts } => PayloadPresentation::Composed {
            parts: parts
                .iter()
                .map(|part| match part {
                    LeanPresentationPart::Range { start, end } => PresentationPart::OutputRange {
                        start_byte: *start,
                        end_byte: *end,
                    },
                    LeanPresentationPart::Literal { bytes } => PresentationPart::Literal {
                        text: String::from_utf8(bytes.clone())
                            .expect("generated presentation literal is UTF-8"),
                    },
                })
                .collect(),
        },
    }
}

fn lean_presented(spec: &LeanPayloadSpec) -> PresentedPayload {
    PresentedPayload {
        output: lean_payload_ref(spec.reference.close_id, spec.reference.stream),
        presentation: lean_presentation(&spec.presentation),
    }
}

fn lean_media(media: &LeanMedia<LeanPayloadSpec>) -> MediaBlock {
    MediaBlock {
        kind: lean_media_kind(&media.kind),
        data: match &media.data {
            LeanMediaData::Url { url } => MediaData::Url { url: url.clone() },
            LeanMediaData::Base64 { payload } => MediaData::Base64 {
                data: lean_payload_ref(payload.reference.close_id, payload.reference.stream),
            },
            LeanMediaData::Raw { payload } => MediaData::Raw {
                data: lean_payload_ref(payload.reference.close_id, payload.reference.stream),
            },
            LeanMediaData::String { payload } => MediaData::String {
                data: lean_payload_ref(payload.reference.close_id, payload.reference.stream),
            },
            LeanMediaData::Unknown => MediaData::Unknown,
        },
        media_type: media.media_type.as_ref().map(|kind| {
            serde_json::from_value(serde_json::Value::String(kind.clone()))
                .expect("generated media type is a protocol media type")
        }),
        detail: media.detail.as_ref().map(|detail| {
            serde_json::from_value(serde_json::Value::String(detail.clone()))
                .expect("generated media detail is a protocol image detail")
        }),
        additional_params: media
            .additional_params
            .as_ref()
            .map(|value| serde_json::from_str(value).expect("generated additional_params is JSON")),
    }
}

fn lean_block(block: &LeanMessageBlock<LeanPayloadSpec>) -> MessageBlock {
    match block {
        LeanMessageBlock::Text { payload } => MessageBlock::Text {
            text: lean_presented(payload),
        },
        LeanMessageBlock::Reasoning { id, parts } => MessageBlock::Reasoning {
            id: id.clone(),
            parts: parts
                .iter()
                .map(|part| match part {
                    LeanReasoningPart::Text { payload, signature } => ReasoningPart::Text {
                        text: lean_presented(payload).output,
                        signature: signature.clone(),
                    },
                    LeanReasoningPart::Encrypted { payload } => ReasoningPart::Encrypted {
                        data: lean_presented(payload).output,
                    },
                    LeanReasoningPart::Redacted { payload } => ReasoningPart::Redacted {
                        data: lean_presented(payload).output,
                    },
                    LeanReasoningPart::Summary { payload } => ReasoningPart::Summary {
                        text: lean_presented(payload).output,
                    },
                })
                .collect(),
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
            tool_call_doc_id: nat(*doc_id),
            id: id.clone(),
            call_id: call_id.clone(),
            name: name.clone(),
            arguments: lean_payload_ref(arguments.reference.close_id, arguments.reference.stream),
            signature: signature.clone(),
            additional_params: additional_params.as_ref().map(|value| {
                serde_json::from_str(value).expect("generated additional_params is JSON")
            }),
        },
        LeanMessageBlock::ToolResult {
            doc_id,
            id,
            call_id,
            parts,
        } => MessageBlock::ToolResult {
            tool_call_doc_id: nat(*doc_id),
            id: id.clone(),
            call_id: call_id.clone(),
            parts: parts
                .iter()
                .map(|part| match part {
                    LeanResultPart::Text { payload } => ToolResultPart::Text {
                        text: lean_presented(payload),
                    },
                    LeanResultPart::Media { value } => ToolResultPart::Media(lean_media(value)),
                })
                .collect(),
        },
        LeanMessageBlock::Media { value } => MessageBlock::Media(lean_media(value)),
    }
}

fn lean_message(message: &LeanCanonicalMessage<LeanPayloadSpec>) -> TranscriptMessage {
    TranscriptMessage {
        message_key: message.key.clone(),
        session_id: nat(message.header.session),
        node_did: NODE_DID.to_owned(),
        requester_did: None,
        request_doc_id: message.header.request.map(nat),
        publication: lean_publication(&message.header.publication),
        outcome: lean_outcome(&message.header.outcome),
        sequence: u32::try_from(message.sequence).expect("generated sequence fits u32"),
        role: lean_role(&message.header.role),
        native_id: message.native_id.clone(),
        native_request_id: message.native_request_id.clone(),
        blocks: message.blocks.iter().map(lean_block).collect(),
        created_at: rfc3339(message.created_at),
    }
}

fn translate_records(
    records: &[LeanCanonicalSegment],
    session: u64,
) -> Vec<(String, OutputSegment)> {
    records
        .iter()
        .map(|record| {
            let close = record.close.as_ref().map(|closure| match closure {
                LeanCanonicalClosure::Closed {
                    outcome,
                    segments,
                    stream_bytes,
                } => SourceClose::Closed {
                    outcome: lean_outcome(outcome),
                    segments: u32::try_from(*segments).expect("generated extent fits u32"),
                    stream_bytes: stream_bytes.clone(),
                },
                LeanCanonicalClosure::Retracted => SourceClose::Retracted,
            });
            let (ordinal, runs, payload) = match &record.flush {
                Some(flush) => (
                    Some(u32::try_from(flush.ordinal).expect("generated ordinal fits u32")),
                    flush
                        .runs
                        .iter()
                        .map(|run| SegmentRun {
                            stream: u32::try_from(run.stream).expect("generated stream fits u32"),
                            bytes: u32::try_from(run.bytes).expect("generated run fits u32"),
                            declaration: run.declaration.as_ref().map(|declaration| {
                                StreamDeclaration {
                                    block_index: u32::try_from(declaration.block)
                                        .expect("generated block index fits u32"),
                                    part_index: u32::try_from(declaration.part)
                                        .expect("generated part index fits u32"),
                                    payload: lean_payload_kind(
                                        &declaration.kind,
                                        declaration.tool.as_ref(),
                                        declaration.media_kind.as_ref(),
                                    ),
                                }
                            }),
                        })
                        .collect(),
                    String::from_utf8(flush.payload.clone())
                        .expect("generated flush payload is UTF-8"),
                ),
                None => (None, Vec::new(), String::new()),
            };
            let segment = OutputSegment {
                node_did: NODE_DID.to_owned(),
                requester_did: None,
                session_id: nat(session),
                request_doc_id: nat(record.coordinate.request),
                source: lean_source(&record.coordinate.source),
                writer: lean_writer(&record.writer),
                ordinal,
                runs,
                payload,
                close,
                created_at: rfc3339(record.created_at),
            };
            (nat(record.id), segment)
        })
        .collect()
}

#[test]
fn generated_auxiliary_cases_drive_native_audit_and_public_projection() {
    let snapshot: serde_json::Value =
        gents_lean_contract::load_contract_snapshot().expect("generate Lean contract");
    let cases: Vec<LeanAuxiliaryOutputCase> = serde_json::from_value(
        snapshot
            .get("auxiliary_output_cases")
            .expect("auxiliary output case group")
            .clone(),
    )
    .expect("strict auxiliary output cases");
    assert!(!cases.is_empty(), "auxiliary output case group is empty");
    for case in &cases {
        let input = &case.observation;
        let records = translate_records(&input.records, input.session);
        let observed = records
            .iter()
            .map(|(doc_id, segment)| ObservedSegment { doc_id, segment })
            .collect::<Vec<_>>();
        let request_doc_id = nat(input.request);
        let session_id = nat(input.session);
        let source = lean_source(&input.target.coordinate.source);
        let writer = lean_writer(&input.target.writer);
        assert!(source.is_auxiliary_audit(), "{}", case.name);
        let actual_audit =
            reconstruct_dense_prefix(&observed, &request_doc_id, &source, &writer, None);
        match (&case.expected.audit, actual_audit) {
            (LeanReasoningAuditResult::Ok { streams: expected }, Ok(actual)) => {
                assert_eq!(actual.streams.len(), expected.len(), "{}", case.name);
                for (actual, expected) in actual.streams.iter().zip(expected) {
                    assert_eq!(
                        actual.declaration.block_index,
                        u32::try_from(expected.declaration.block).unwrap(),
                        "{}",
                        case.name
                    );
                    assert_eq!(
                        actual.declaration.part_index,
                        u32::try_from(expected.declaration.part).unwrap(),
                        "{}",
                        case.name
                    );
                    assert_eq!(
                        actual.declaration.payload,
                        lean_payload_kind(
                            &expected.declaration.kind,
                            expected.declaration.tool.as_ref(),
                            expected.declaration.media_kind.as_ref(),
                        ),
                        "{}",
                        case.name
                    );
                    assert_eq!(
                        actual.text.as_bytes(),
                        expected.bytes.as_slice(),
                        "{}",
                        case.name
                    );
                }
            }
            (expected, actual) => panic!(
                "auxiliary audit case `{}`: expected {expected:?}, got {actual:?}",
                case.name
            ),
        }

        let messages = input
            .messages
            .iter()
            .map(|message| (nat(message.header.id), lean_message(message)))
            .collect::<Vec<_>>();
        let message_refs = messages
            .iter()
            .map(|(id, message)| (id.as_str(), message))
            .collect::<Vec<_>>();
        let denied_headers = input
            .denied_headers
            .iter()
            .map(|id| nat(*id))
            .collect::<Vec<_>>();
        let denied_segments = input
            .denied_segments
            .iter()
            .map(|id| nat(*id))
            .collect::<Vec<_>>();
        let dependency_denials = input
            .dependency_denials
            .iter()
            .map(|denial| DependencyDenial {
                root_close_id: nat(denial.root_close_id),
                denied_doc_id: nat(denial.denied_doc_id),
            })
            .collect::<Vec<_>>();
        let current_request = input
            .owner
            .current_request
            .map(|(request, generation)| (nat(request), nat(generation)));
        let live_tools = input
            .owner
            .live_tools
            .iter()
            .map(|id| nat(*id))
            .collect::<Vec<_>>();
        let owner = OwnerLiveness {
            current_request: current_request
                .as_ref()
                .map(|(request, generation)| (request.as_str(), generation.as_str())),
            live_tools: live_tools.iter().map(String::as_str).collect(),
        };
        let target_message_id = input.target.message_id.map(nat);
        let target_request_id = nat(input.target.coordinate.request);
        let terminal_selection =
            input
                .terminal_selection
                .as_ref()
                .map(|selection| match selection {
                    gents_lean_vocab::LeanTerminalSelection::Message { id } => {
                        TerminalOutput::Message {
                            message_doc_id: nat(*id),
                        }
                    }
                    gents_lean_vocab::LeanTerminalSelection::NoMessage => TerminalOutput::NoMessage,
                });
        let actual_view = project_live(&LiveObservation {
            request_doc_id: &request_doc_id,
            session_id: &session_id,
            target: LiveTarget {
                request_doc_id: &target_request_id,
                source: &source,
                writer: &writer,
                message_id: target_message_id.as_deref(),
            },
            messages: &message_refs,
            node_did: NODE_DID,
            requester_did: None,
            records: &observed,
            denied_headers: &denied_headers,
            denied_segments: &denied_segments,
            dependency_denials: &dependency_denials,
            owner,
            request_terminal: input.request_terminal,
            terminal_selection,
        });
        assert!(
            matches!(&case.expected.public_view, View::Absent),
            "{}",
            case.name
        );
        assert_eq!(actual_view, LiveView::Absent, "{}", case.name);

        let planned = plan_recovery_prefix(&observed, &request_doc_id, &source, &writer)
            .unwrap_or_else(|error| panic!("{}: {error}", case.name));
        let recovery_closing =
            translate_records(std::slice::from_ref(&case.recovery_closing), input.session);
        assert_eq!(
            Some(planned.close.clone()),
            recovery_closing[0].1.close,
            "{}",
            case.name
        );
        assert!(planned.retained_streams.is_empty(), "{}", case.name);
        assert!(
            planned.retained_blocks("native-close").unwrap().is_empty(),
            "{}",
            case.name
        );
    }
}
