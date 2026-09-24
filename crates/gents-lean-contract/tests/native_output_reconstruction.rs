//! Native conformance boundary for canonical-output stream reconstruction
//! (#1571). The generated `canonical_output_projection_cases` drive the actual
//! `gents_protocol::output::reconstruction::{reconstruct_stream,
//! reconstruct_presented_payload}` through the runtime's generated DTO
//! vocabulary (`lean_vocab_test/canonical_output.rs`); nothing here restates a
//! policy constant or a fixture expectation.
//!
//! Boundary scope: for a concrete reference, `reconstruct_stream` observes one
//! sealed payload closure — a closing record, its extent, denial evidence and
//! duplicate raw records — and itself reports `AccessDenied`, `UnresolvedClose`
//! and `MissingSegment`. What stays above this boundary is the projection's
//! *view classification* (whether an observation renders as loading, denied,
//! absent, live, settling, retracted, retained-partial, or invalid) and
//! message selection/identity. Cases whose modeled view is such a
//! classification are excluded by name and reported in the test summary,
//! never replaced with fake fixtures. Excluded cases whose input carries
//! concrete header references still get direct primitive probes: the
//! expectations there come from the input evidence and the protocol's own
//! error contract, not from a re-modeled policy.

#[path = "../../gents/src/lean_vocab_test/canonical_output.rs"]
mod runtime_contract;

use std::collections::BTreeSet;

use gents_protocol::output::live::{
    project_live, reconstruct_audit_prefix, DensePrefixError, LiveObservation, LiveTarget,
    LiveView, OwnerLiveness,
};
use gents_protocol::output::reconstruction::{
    reconstruct_presented_payload, reconstruct_stream, DependencyDenial, ObservedSegment,
};
use gents_protocol::output::recovery::plan_recovery_prefix;
use gents_protocol::output::{
    MediaBlock, MediaData, MediaKind, MessageBlock, MessagePublication, MessageRole, OutputOutcome,
    OutputSegment, OutputSource, OutputWriter, PayloadPresentation, PayloadRef, PresentationPart,
    PresentedPayload, ReasoningPart, ReconstructionError, SegmentRun, SourceClose,
    StreamDeclaration, StreamPayload, TerminalOutput, ToolResultPart, TranscriptMessage,
};
use gents_protocol::rendered_request::{CaptureScope, CaptureScopeKind};
use runtime_contract::{
    LeanAuxiliaryKind, LeanAuxiliaryOutputCase, LeanCanonicalClosure, LeanCanonicalMessage,
    LeanCanonicalOutputProjectionCase, LeanCanonicalOutputView as View, LeanCanonicalSegment,
    LeanCanonicalSource, LeanCanonicalWriter, LeanMedia, LeanMediaData, LeanMediaKind,
    LeanMessageBlock, LeanMessagePublication, LeanMessageRole, LeanOutcome, LeanPayloadKind,
    LeanPayloadSpec, LeanPresentation, LeanPresentationPart, LeanReasoningAuditCase,
    LeanReasoningAuditResult, LeanReasoningPart, LeanReasoningSignatureCase, LeanResultPart,
};

/// View classes this adapter can exercise through the reconstruction
/// primitives.
const EXERCISABLE_VIEWS: [&str; 2] = ["published", "conflicted"];

/// Adapter-level document identity. The Lean NatID world has no DID; every
/// generated segment and header shares the one runtime agent identity, which
/// is all the reconstruction primitive requires (`request_doc_id` + `source`
/// are the real coordinates). This is transport identity, not policy.
const AGENT_DID: &str = "did:key:z6MkLeanConformanceAgent";

/// The NatID → string translation. Injective, so distinct modeled identities
/// stay distinct physical identities, and idempotent replay (the same modeled
/// record delivered twice) maps to the same physical document identity — the
/// exact behavior `raw_exact_record_replay_is_idempotent` proves.
fn nat(id: u64) -> String {
    format!("nat-{id}")
}

/// The Lean model measures observation time in whole seconds; the protocol
/// document carries an RFC 3339 timestamp string. This is a representation
/// bridge, not a policy choice: the modeled instant is preserved exactly.
fn rfc3339(seconds: u64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(
        i64::try_from(seconds).expect("generated timestamp fits i64"),
        0,
    )
    .expect("generated timestamp is representable")
    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// What the adapter does with one generated case's modeled view.
enum Expected {
    /// The model publishes a header from this observation; exercised below
    /// through the actual primitives, input-driven.
    Published,
    /// The model reports an integrity conflict; the stream primitive must
    /// surface a record-level conflict when the twins are record facts.
    /// Message-identity conflicts live above this boundary and are excluded.
    Conflicted,
    /// The modeled view class is a projection classification above the
    /// stream-primitive boundary.
    Excluded {
        class: &'static str,
        reason: &'static str,
    },
}

/// A generated case translated into strict protocol documents: NatIDs become
/// strings, byte payloads stay bytes, UTF-8 payloads become strings.
struct TranslatedCase {
    name: String,
    /// `(doc_id, segment)` observations in generated arrival order.
    segments: Vec<(String, OutputSegment)>,
    /// Modeled denial evidence, translated verbatim (segment ids and header
    /// ids share one denial namespace at this boundary).
    denied: Vec<String>,
    dependency_denials: Vec<DependencyDenial>,
    expected: Expected,
}

fn view_class(view: &View) -> &'static str {
    match view {
        View::Published { .. } => "published",
        View::Conflicted => "conflicted",
        View::Loading => "loading",
        View::Denied => "denied",
        View::Absent => "absent",
        View::Live { .. } => "live",
        View::Settling { .. } => "settling",
        View::Retracted => "retracted",
        View::RetainedPartial { .. } => "retained_partial",
        View::Invalid => "invalid",
    }
}

/// Why a view *classification* is above the `reconstruct_stream` boundary.
/// The primitive itself still reports `AccessDenied` and incomplete errors
/// for concrete references; only the verdict (loading vs denied vs absent)
/// belongs to the projection owner.
fn exclusion_reason(view: &View) -> &'static str {
    match view {
        View::Loading => {
            "the loading-vs-denied-vs-absent verdict is the projection owner's \
             classification; reconstruct_stream reports UnresolvedClose/\
             MissingSegment per reference but cannot render the view"
        }
        View::Denied => {
            "the denial verdict is the projection owner's classification; \
             reconstruct_stream reports AccessDenied per reference but cannot \
             render the view"
        }
        View::Absent => {
            "absence (stale generation, unheaded authored) is projection selection \
             over writer liveness, not a sealed-payload result"
        }
        View::Live { .. } | View::Settling { .. } => {
            "live/settling preview classification over unclosed sources is owned by \
             the projection owner; reconstruct_stream observes sealed extents only"
        }
        View::Retracted => {
            "retracted-source rendering is a projection decision over the Retracted \
             closure, not a sealed-payload result"
        }
        View::RetainedPartial { .. } => {
            "retained-partial diagnostics come from the terminalization owner's \
             selection, not the sealed-payload primitive"
        }
        View::Invalid => {
            "header/native-shape invalidity is validated by reconstruct_message, \
             outside this stream-only adapter"
        }
        View::Published { .. } | View::Conflicted => "",
    }
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
            // Lean models the provider scope as its allocation number; the
            // protocol coordinate is the capture scope label. Every generated
            // provider record uses scope allocation 1 of the inference loop.
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
    tool: Option<&runtime_contract::LeanToolIdentity>,
    media_kind: Option<&LeanMediaKind>,
) -> StreamPayload {
    match kind {
        LeanPayloadKind::Text => StreamPayload::Text,
        LeanPayloadKind::Reasoning => StreamPayload::Reasoning,
        LeanPayloadKind::Signature => StreamPayload::ReasoningSignature,
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
                    // The Lean presentation literal is raw bytes; the protocol
                    // literal is runtime-authored text. Presentation equality
                    // is validated by present_stream/reconstruct_message, not
                    // by this adapter, which probes the reconstruction
                    // primitives only.
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
        agent_did: AGENT_DID.to_owned(),
        requester_did: None,
        request_doc_id: message.header.request.map(nat),
        publication: lean_publication(&message.header.publication),
        outcome: lean_outcome(&message.header.outcome),
        sequence: u32::try_from(message.sequence).expect("generated sequence fits u32"),
        role: lean_role(&message.header.role),
        native_id: message.native_id.clone(),
        blocks: message.blocks.iter().map(lean_block).collect(),
        created_at: rfc3339(message.created_at),
    }
}

/// Translate one generated case. Never invents a fact: every protocol value is
/// a faithful translation of the modeled observation or expectation.
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
                    // The flush payload is byte data; the protocol segment
                    // stores it as the exact UTF-8 string of those bytes.
                    String::from_utf8(flush.payload.clone())
                        .expect("generated flush payload is UTF-8"),
                ),
                None => (None, Vec::new(), String::new()),
            };
            let segment = OutputSegment {
                agent_did: AGENT_DID.to_owned(),
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

fn translate(case: &LeanCanonicalOutputProjectionCase) -> TranslatedCase {
    let input = &case.input;
    let segments = translate_records(&input.records, input.session);
    let denied = input
        .denied_segments
        .iter()
        .chain(input.denied_headers.iter())
        .map(|id| nat(*id))
        .collect();
    let dependency_denials = input
        .dependency_denials
        .iter()
        .map(|denial| DependencyDenial {
            root_close_id: nat(denial.root_close_id),
            denied_doc_id: nat(denial.denied_doc_id),
        })
        .collect();
    let expected = match &case.expected {
        View::Published { .. } => Expected::Published,
        View::Conflicted => Expected::Conflicted,
        other => Expected::Excluded {
            class: view_class(other),
            reason: exclusion_reason(other),
        },
    };
    TranslatedCase {
        name: case.name.clone(),
        segments,
        denied,
        dependency_denials,
        expected,
    }
}

/// One plain (Full-presentation) payload reference: `reconstruct_stream` must
/// return exactly the model's native bytes for it.
fn assert_plain_ref(
    case: &TranslatedCase,
    observations: &[ObservedSegment<'_>],
    reference: &PayloadRef,
    expected: &[u8],
) {
    let stream = reconstruct_stream(
        observations,
        &case.denied,
        &case.dependency_denials,
        reference,
    )
    .unwrap_or_else(|error| {
        panic!(
            "case `{}`: reconstruct_stream({reference:?}) failed: {error:?}",
            case.name
        )
    });
    assert_eq!(
        stream.text.as_bytes(),
        expected,
        "case `{}`: reconstructed bytes disagree with the model for {reference:?}",
        case.name
    );
}

/// One presented payload (Text / ToolResult Text): the production
/// `reconstruct_presented_payload` owner applies the presentation over the
/// reconstructed stream; the result must be exactly the model's native bytes.
fn assert_presented(
    case: &TranslatedCase,
    observations: &[ObservedSegment<'_>],
    payload: &PresentedPayload,
    expected: &[u8],
) {
    let rendered = reconstruct_presented_payload(
        observations,
        &case.denied,
        &case.dependency_denials,
        payload,
    )
    .unwrap_or_else(|error| {
        panic!(
            "case `{}`: reconstruct_presented_payload({payload:?}) failed: {error:?}",
            case.name
        )
    });
    assert_eq!(
        rendered.as_bytes(),
        expected,
        "case `{}`: presented bytes disagree with the model for {payload:?}",
        case.name
    );
}

/// Drive the actual primitives for a published case. The selected header comes
/// from `case.input.messages` (the adapter asserts the model's published view
/// is that input envelope; selection itself stays outside this adapter), and
/// each block is paired directly with its model-derived native block — no
/// reference-keyed map, because the same reference may appear under different
/// presentations.
fn assert_published(case: &LeanCanonicalOutputProjectionCase, translated: &TranslatedCase) {
    let View::Published {
        message: expected_message,
        native,
    } = &case.expected
    else {
        unreachable!("assert_published is only called on published cases")
    };
    let input_message = case
        .input
        .messages
        .iter()
        .find(|message| message.header.id == expected_message.header.id)
        .unwrap_or_else(|| {
            panic!(
                "case `{}`: published header {} is not among case.input.messages",
                case.name, expected_message.header.id
            )
        });
    assert_eq!(
        input_message, expected_message,
        "case `{}`: the model's published view must be the selected input envelope",
        case.name
    );
    let message = lean_message(input_message);
    let observations = observations(translated);
    assert_eq!(
        message.blocks.len(),
        native.blocks.len(),
        "case `{}`: model header/native block counts disagree",
        case.name
    );
    let mut probed_parts = 0usize;
    for (block, native_block) in message.blocks.iter().zip(&native.blocks) {
        match (block, native_block) {
            (MessageBlock::Text { text }, LeanMessageBlock::Text { payload }) => {
                assert_presented(translated, &observations, text, payload);
                probed_parts += 1;
            }
            (
                MessageBlock::Reasoning { parts, .. },
                LeanMessageBlock::Reasoning {
                    parts: native_parts,
                    ..
                },
            ) => {
                assert_eq!(
                    parts.len(),
                    native_parts.len(),
                    "case `{}`: model reasoning part counts disagree",
                    case.name
                );
                for (part, native_part) in parts.iter().zip(native_parts) {
                    // Reasoning parts are plain references (the model requires
                    // full presentation outside text/tool-output positions).
                    let (reference, bytes) = match (part, native_part) {
                        (
                            ReasoningPart::Text { text, .. },
                            LeanReasoningPart::Text { payload, .. },
                        )
                        | (
                            ReasoningPart::Encrypted { data: text },
                            LeanReasoningPart::Encrypted { payload },
                        )
                        | (
                            ReasoningPart::Redacted { data: text },
                            LeanReasoningPart::Redacted { payload },
                        )
                        | (
                            ReasoningPart::Summary { text },
                            LeanReasoningPart::Summary { payload },
                        ) => (text, payload),
                        // The generated contract pairs part kinds positionally;
                        // any other pairing is a translation bug, not a fixture.
                        _ => panic!(
                            "case `{}`: model/generated reasoning part kinds disagree",
                            case.name
                        ),
                    };
                    assert_plain_ref(translated, &observations, reference, bytes);
                    probed_parts += 1;
                }
            }
            (
                MessageBlock::ToolCall { arguments, .. },
                LeanMessageBlock::ToolCall {
                    arguments: native_arguments,
                    ..
                },
            ) => {
                assert_plain_ref(translated, &observations, arguments, native_arguments);
                probed_parts += 1;
            }
            (
                MessageBlock::ToolResult { parts, .. },
                LeanMessageBlock::ToolResult {
                    parts: native_parts,
                    ..
                },
            ) => {
                assert_eq!(
                    parts.len(),
                    native_parts.len(),
                    "case `{}`: model tool-result part counts disagree",
                    case.name
                );
                for (part, native_part) in parts.iter().zip(native_parts) {
                    match (part, native_part) {
                        (ToolResultPart::Text { text }, LeanResultPart::Text { payload }) => {
                            assert_presented(translated, &observations, text, payload);
                            probed_parts += 1;
                        }
                        (ToolResultPart::Media(media), LeanResultPart::Media { value }) => {
                            // Each media part compares against its own paired
                            // native media value, never the block's first one.
                            if let Some(reference) = media_reference(&media.data) {
                                assert_plain_ref(
                                    translated,
                                    &observations,
                                    reference,
                                    &native_media_bytes(value),
                                );
                                probed_parts += 1;
                            }
                        }
                        _ => panic!(
                            "case `{}`: model/generated tool-result part kinds disagree",
                            case.name
                        ),
                    }
                }
            }
            (MessageBlock::Media(media), LeanMessageBlock::Media { value }) => {
                if let Some(reference) = media_reference(&media.data) {
                    assert_plain_ref(
                        translated,
                        &observations,
                        reference,
                        &native_media_bytes(value),
                    );
                    probed_parts += 1;
                }
            }
            _ => panic!("case `{}`: model/generated block kinds disagree", case.name),
        }
    }
    assert!(
        probed_parts > 0,
        "case `{}`: the published header exercised no reconstructable payload part",
        case.name
    );
}

fn media_reference(data: &MediaData) -> Option<&PayloadRef> {
    match data {
        MediaData::Base64 { data } | MediaData::Raw { data } | MediaData::String { data } => {
            Some(data)
        }
        MediaData::Url { .. } | MediaData::Unknown => None,
    }
}

fn native_media_bytes(media: &LeanMedia<Vec<u8>>) -> Vec<u8> {
    match &media.data {
        LeanMediaData::Base64 { payload }
        | LeanMediaData::Raw { payload }
        | LeanMediaData::String { payload } => payload.clone(),
        LeanMediaData::Url { .. } | LeanMediaData::Unknown => Vec::new(),
    }
}

/// Drive `reconstruct_stream` over every sealed source in a conflicted case.
/// A record-level twin must surface a conflict error. Message-identity twins
/// are excluded using input evidence before invoking the primitive; a missing
/// native conflict must never become an automatic coverage exclusion.
fn assert_conflicted(
    case: &TranslatedCase,
    input: &LeanCanonicalOutputProjectionCase,
) -> Option<String> {
    let Expected::Conflicted = &case.expected else {
        unreachable!("assert_conflicted is only called on conflicted cases")
    };
    let observations = observations(case);
    // Exclusions must be decided from input facts, never from a native
    // implementation failing to detect the expected conflict.
    let messages = &input.input.messages;
    if messages.iter().enumerate().any(|(i, message)| {
        messages[i + 1..]
            .iter()
            .any(|other| message.header.id == other.header.id && message != other)
    }) {
        return Some(
            "conflicting input message identities belong to the message projection owner"
                .to_owned(),
        );
    }
    let mut references: Vec<PayloadRef> = Vec::new();
    for (doc_id, segment) in &case.segments {
        if let Some(SourceClose::Closed { stream_bytes, .. }) = &segment.close {
            for stream in 0..stream_bytes.len() {
                let reference = PayloadRef {
                    close_doc_id: doc_id.clone(),
                    stream: stream as u32,
                };
                if !references.contains(&reference) {
                    references.push(reference);
                }
            }
        }
    }
    assert!(
        !references.is_empty(),
        "case `{}`: conflicted observation has no sealed source to probe",
        case.name
    );
    let mut surfaced = Vec::new();
    for reference in &references {
        match reconstruct_stream(
            &observations,
            &case.denied,
            &case.dependency_denials,
            reference,
        ) {
            Err(
                error @ (ReconstructionError::ConflictingSegments { .. }
                | ReconstructionError::ConflictingClosures { .. }),
            ) => surfaced.push(format!("{error:?}")),
            Err(error) => panic!(
                "case `{}`: reconstruct_stream({reference:?}) failed with a \
                 non-conflict error: {error:?}",
                case.name
            ),
            Ok(_) => {}
        }
    }
    {
        assert!(
            !surfaced.is_empty(),
            "case `{}`: native reconstruction missed the modeled record conflict",
            case.name
        );
        assert!(
            surfaced.len() == references.len(),
            "case `{}`: conflict surfaced inconsistently across references",
            case.name
        );
        None
    }
}

/// Direct primitive checks for excluded cases whose input carries concrete
/// header references. The reference comes from `case.input.messages`; the
/// expectation is the protocol's own error contract — `AccessDenied` for a
/// denied dependency, an incomplete error (`UnresolvedClose`/`MissingSegment`)
/// for a missing one — never a re-modeled classification. A reference that
/// fully resolves proves the modeled loading/denied verdict was header-level,
/// which is exactly the fact above this boundary.
fn probe_excluded_primitive_facts(
    case: &LeanCanonicalOutputProjectionCase,
    translated: &TranslatedCase,
    class: &str,
) -> Vec<String> {
    let denied: BTreeSet<&str> = translated.denied.iter().map(String::as_str).collect();
    let dep_roots: BTreeSet<&str> = translated
        .dependency_denials
        .iter()
        .map(|denial| denial.root_close_id.as_str())
        .collect();
    let dep_denied: BTreeSet<&str> = translated
        .dependency_denials
        .iter()
        .map(|denial| denial.denied_doc_id.as_str())
        .collect();
    let observations = observations(translated);
    let mut notes = Vec::new();
    for message in &case.input.messages {
        for modeled in &message.header.refs {
            let reference = lean_payload_ref(modeled.close_id, modeled.stream);
            let is_denied = denied.contains(reference.close_doc_id.as_str())
                || dep_roots.contains(reference.close_doc_id.as_str());
            match class {
                "denied" if is_denied => {
                    match reconstruct_stream(
                        &observations,
                        &translated.denied,
                        &translated.dependency_denials,
                        &reference,
                    ) {
                        Err(ReconstructionError::AccessDenied { doc_id }) => {
                            assert!(
                                denied.contains(doc_id.as_str())
                                    || dep_denied.contains(doc_id.as_str()),
                                "case `{}`: AccessDenied for {reference:?} names an \
                                 unmodeled denial {doc_id}",
                                case.name
                            );
                            notes.push(format!(
                                "case `{}`: reconstruct_stream({reference:?}) → \
                                 AccessDenied({doc_id})",
                                case.name
                            ));
                        }
                        other => panic!(
                            "case `{}`: denied dependency {reference:?} must surface \
                             AccessDenied, got {other:?}",
                            case.name
                        ),
                    }
                }
                "loading" if !is_denied => {
                    match reconstruct_stream(
                        &observations,
                        &translated.denied,
                        &translated.dependency_denials,
                        &reference,
                    ) {
                        Err(error) => {
                            assert!(
                                error.is_incomplete(),
                                "case `{}`: missing dependency {reference:?} must \
                                 surface an incomplete error, got {error:?}",
                                case.name
                            );
                            notes.push(format!(
                                "case `{}`: reconstruct_stream({reference:?}) → \
                                 incomplete ({error})",
                                case.name
                            ));
                        }
                        Ok(_) => notes.push(format!(
                            "case `{}`: reconstruct_stream({reference:?}) resolves; \
                             the modeled loading verdict was header-level",
                            case.name
                        )),
                    }
                }
                _ => {}
            }
        }
    }
    notes
}

fn observations(case: &TranslatedCase) -> Vec<ObservedSegment<'_>> {
    case.segments
        .iter()
        .map(|(doc_id, segment)| ObservedSegment { doc_id, segment })
        .collect()
}

#[test]
fn generated_canonical_output_cases_drive_native_reconstruction() {
    let snapshot: serde_json::Value =
        gents_lean_contract::load_contract_snapshot().expect("generate Lean contract");
    let cases: Vec<LeanCanonicalOutputProjectionCase> = serde_json::from_value(
        snapshot
            .get("canonical_output_projection_cases")
            .expect("canonical output projection group")
            .clone(),
    )
    .expect("strict canonical output projection cases");
    assert!(
        !cases.is_empty(),
        "the generated canonical output projection case set must not be empty"
    );

    let mut exercised: BTreeSet<&str> = BTreeSet::new();
    let mut exclusions: Vec<String> = Vec::new();
    let mut primitive_notes: Vec<String> = Vec::new();
    let mut accounted = 0usize;
    for case in &cases {
        let translated = translate(case);
        match &translated.expected {
            Expected::Published => {
                assert_published(case, &translated);
                exercised.insert("published");
                accounted += 1;
            }
            Expected::Conflicted => {
                if let Some(reason) = assert_conflicted(&translated, case) {
                    exclusions.push(format!(
                        "excluded view class `conflicted` (case `{}`): {reason}",
                        translated.name
                    ));
                } else {
                    exercised.insert("conflicted");
                }
                accounted += 1;
            }
            Expected::Excluded { class, reason } => {
                exclusions.push(format!(
                    "excluded view class `{class}` (case `{}`): {reason}",
                    translated.name
                ));
                primitive_notes.extend(probe_excluded_primitive_facts(case, &translated, class));
                accounted += 1;
            }
        }
    }

    // Report every exclusion and every direct primitive probe at this named
    // boundary. Coverage is stated honestly: exactly the generated cases are
    // accounted for, each either exercised through the primitives or excluded
    // and reported.
    for exclusion in &exclusions {
        eprintln!("[native_output_reconstruction] {exclusion}");
    }
    for note in &primitive_notes {
        eprintln!("[native_output_reconstruction] primitive probe: {note}");
    }
    assert_eq!(
        accounted,
        cases.len(),
        "every generated case must be either exercised through the primitives \
         or explicitly excluded and reported"
    );
    let exercisable: BTreeSet<&str> = EXERCISABLE_VIEWS.into_iter().collect();
    assert!(
        exercised.is_subset(&exercisable),
        "the adapter must not exercise view classes outside its named boundary: \
         {exercised:?}"
    );
    assert!(
        exercised.contains("published"),
        "generated published cases must exercise the native reconstruction \
         primitives"
    );
    eprintln!(
        "[native_output_reconstruction] exercised view classes: {exercised:?}; \
         excluded cases: {}; primitive probes: {}",
        exclusions.len(),
        primitive_notes.len()
    );
}

#[test]
fn generated_reasoning_audit_cases_drive_exact_native_prefix() {
    let snapshot: serde_json::Value =
        gents_lean_contract::load_contract_snapshot().expect("generate Lean contract");
    let cases: Vec<LeanReasoningAuditCase> = serde_json::from_value(
        snapshot
            .get("reasoning_audit_cases")
            .expect("reasoning audit case group")
            .clone(),
    )
    .expect("strict reasoning audit cases");
    assert!(!cases.is_empty(), "reasoning audit case group is empty");
    for case in &cases {
        let records = translate_records(&case.input.records, case.input.session);
        let observed = records
            .iter()
            .map(|(doc_id, segment)| ObservedSegment { doc_id, segment })
            .collect::<Vec<_>>();
        let actual = reconstruct_audit_prefix(
            &observed,
            &nat(case.input.target.coordinate.request),
            &lean_source(&case.input.target.coordinate.source),
            &lean_writer(&case.input.target.writer),
            None,
        );
        match (&case.expected, actual) {
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
            (LeanReasoningAuditResult::Loading, Err(DensePrefixError::Loading))
            | (LeanReasoningAuditResult::Conflicted, Err(DensePrefixError::Conflicted { .. }))
            | (LeanReasoningAuditResult::Invalid, Err(DensePrefixError::Invalid { .. })) => {}
            (expected, actual) => panic!(
                "reasoning audit case `{}`: expected {expected:?}, got {actual:?}",
                case.name
            ),
        }
    }
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
            reconstruct_audit_prefix(&observed, &request_doc_id, &source, &writer, None);
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
                    runtime_contract::LeanTerminalSelection::Message { id } => {
                        TerminalOutput::Message {
                            message_doc_id: nat(*id),
                        }
                    }
                    runtime_contract::LeanTerminalSelection::NoMessage => TerminalOutput::NoMessage,
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
            agent_did: AGENT_DID,
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

#[test]
fn generated_reasoning_signature_cases_drive_native_header_validation() {
    let snapshot: serde_json::Value =
        gents_lean_contract::load_contract_snapshot().expect("generate Lean contract");
    let cases: Vec<LeanReasoningSignatureCase> = serde_json::from_value(
        snapshot
            .get("reasoning_signature_cases")
            .expect("reasoning signature case group")
            .clone(),
    )
    .expect("strict reasoning signature cases");
    assert!(!cases.is_empty(), "reasoning signature case group is empty");
    let mut byte_exclusions = 0;
    for case in &cases {
        if case.name == "invalid_signature_bytes" {
            byte_exclusions += 1;
            assert!(!case.accepted, "invalid UTF-8 cannot publish a signature");
            assert!(
                case.records
                    .iter()
                    .filter_map(|record| record.flush.as_ref())
                    .any(|flush| String::from_utf8(flush.payload.clone()).is_err()),
                "invalid_signature_bytes must fail at the protocol UTF-8 string boundary"
            );
            continue;
        }
        assert!(
            matches!(&case.payload.presentation, LeanPresentation::Full),
            "{}: signature validation requires the modeled full payload",
            case.name
        );
        let records = translate_records(&case.records, 0);
        let observed = records
            .iter()
            .map(|(doc_id, segment)| ObservedSegment { doc_id, segment })
            .collect::<Vec<_>>();
        let first = &records[0].1;
        let OutputWriter::RequestExecution {
            execution_generation,
        } = &first.writer
        else {
            panic!("{}: signature fixture has no request writer", case.name);
        };
        let header = TranscriptMessage {
            message_key: format!("reasoning-signature:{}", case.name),
            session_id: first.session_id.clone(),
            agent_did: first.agent_did.clone(),
            requester_did: None,
            request_doc_id: Some(first.request_doc_id.clone()),
            publication: MessagePublication::RequestExecution {
                execution_generation: execution_generation.clone(),
            },
            outcome: OutputOutcome::Complete,
            sequence: 0,
            role: MessageRole::Assistant,
            native_id: None,
            blocks: vec![MessageBlock::Reasoning {
                id: None,
                parts: vec![ReasoningPart::Text {
                    text: lean_payload_ref(
                        case.payload.reference.close_id,
                        case.payload.reference.stream,
                    ),
                    signature: case.signature.clone(),
                }],
            }],
            created_at: first.created_at.clone(),
        };
        let actual = gents_protocol::output::reconstruction::reconstruct_message(
            &observed,
            &[],
            &[],
            &header,
        );
        assert_eq!(actual.is_ok(), case.accepted, "{}: {actual:?}", case.name);
        if let Err(error) = actual {
            assert!(
                !error.is_incomplete(),
                "{}: mismatch must fail closed",
                case.name
            );
            assert!(
                matches!(&error, ReconstructionError::InvalidStructure { detail } if detail.contains("reasoning signature")),
                "{}: rejected header must fail its signature check: {error:?}",
                case.name
            );
        }
    }
    assert_eq!(
        byte_exclusions, 1,
        "exactly one modeled byte case is outside String payloads"
    );
}
