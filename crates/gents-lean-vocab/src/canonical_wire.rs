use gents_protocol::output::reconstruction::ReconstructedStream;
use gents_protocol::output::{
    MediaBlock, MediaData, MediaKind as OutputMediaKind, MessageBlock, MessagePublication,
    MessageRole, OutputOutcome, OutputSegment, OutputSource, OutputWriter, PayloadPresentation,
    PayloadRef as OutputPayloadRef, PresentationPart as OutputPresentationPart, PresentedPayload,
    ReasoningPart as OutputReasoningPart, SegmentRun, SourceClose, StreamDeclaration,
    StreamPayload, ToolResultPart, TranscriptMessage,
};
use gents_protocol::rendered_request::{CaptureScope, CaptureScopeKind};
use serde::{Deserialize, Serialize};

/// The node every translated canonical record and message belongs to; the
/// model has one node, so its identity is not part of the wire.
pub const NODE_DID: &str = "did:key:z6MkLeanConformanceAgent";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Segment {
    pub id: u64,
    pub coordinate: Coordinate,
    pub writer: Writer,
    pub flush: Option<Flush>,
    pub close: Option<Closure>,
    pub created_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Coordinate {
    pub request: u64,
    pub source: Source,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Source {
    Provider {
        scope: u64,
        turn: u64,
        attempt: u64,
    },
    Auxiliary {
        auxiliary_kind: AuxiliaryKind,
        scope: u64,
        turn: u64,
        attempt: u64,
    },
    ToolEffectArguments {
        call: u64,
    },
    Tool {
        call: u64,
    },
    Authored {
        key: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuxiliaryKind {
    Compaction,
    CompactionFallback,
    Title,
    AccessReview,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Writer {
    Request { generation: u64 },
    Tool { call: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Flush {
    pub ordinal: u64,
    pub runs: Vec<Run>,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Run {
    pub stream: u64,
    pub bytes: u64,
    pub declaration: Option<Declaration>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Declaration {
    pub block: u64,
    pub part: u64,
    pub kind: PayloadKind,
    pub tool: Option<ToolIdentity>,
    pub media_kind: Option<MediaKind>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolIdentity {
    pub id: String,
    pub call_id: Option<String>,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Closure {
    Closed {
        outcome: Outcome,
        segments: u64,
        stream_bytes: Vec<u64>,
    },
    Retracted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Complete,
    Partial,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PayloadKind {
    Text,
    Reasoning,
    Signature,
    ReasoningStop,
    Summary,
    Encrypted,
    Redacted,
    Arguments,
    ToolOutput,
    Media,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Image,
    Audio,
    Video,
    Document,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PayloadRef {
    pub close_id: u64,
    pub stream: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PayloadSpec {
    pub reference: PayloadRef,
    pub presentation: Presentation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Presentation {
    Full,
    Composed { parts: Vec<PresentationPart> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PresentationPart {
    Range { start: u64, end: u64 },
    Literal { bytes: Vec<u8> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Header {
    pub id: u64,
    pub session: u64,
    pub request: Option<u64>,
    pub origin: Option<u64>,
    pub refs: Vec<PayloadRef>,
    pub outcome: Outcome,
    pub role: Role,
    pub publication: Publication,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Publication {
    RequestExecution { generation: u64 },
    RequestRecovery { generation: u64 },
    ToolDelivery { call: u64 },
    Fork { origin: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Message<T> {
    pub header: Header,
    pub key: String,
    pub sequence: u64,
    pub native_id: Option<String>,
    pub native_request_id: Option<String>,
    pub blocks: Vec<Block<T>>,
    pub created_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Block<T> {
    Text {
        payload: T,
    },
    Reasoning {
        id: Option<String>,
        parts: Vec<ReasoningPart<T>>,
    },
    ToolCall {
        doc_id: u64,
        id: String,
        call_id: Option<String>,
        name: String,
        arguments: T,
        signature: Option<String>,
        additional_params: Option<String>,
    },
    ToolResult {
        doc_id: u64,
        id: String,
        call_id: Option<String>,
        parts: Vec<ResultPart<T>>,
    },
    Media {
        value: Media<T>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReasoningPart<T> {
    Text {
        payload: T,
        signature: Option<String>,
    },
    Encrypted {
        payload: T,
    },
    Redacted {
        payload: T,
    },
    Summary {
        payload: T,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ResultPart<T> {
    Text { payload: T },
    Media { value: Media<T> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Media<T> {
    pub kind: MediaKind,
    pub data: MediaPayload<T>,
    pub media_type: Option<String>,
    pub detail: Option<String>,
    pub additional_params: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MediaPayload<T> {
    Url { url: String },
    Base64 { payload: T },
    Raw { payload: T },
    String { payload: T },
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reconstructed {
    pub role: Role,
    pub native_id: Option<String>,
    pub native_request_id: Option<String>,
    pub blocks: Vec<Block<Vec<u8>>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DependencyDenial {
    pub root_close_id: u64,
    pub denied_doc_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Owner {
    pub current_request: Option<(u64, u64)>,
    pub live_tools: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub coordinate: Coordinate,
    pub writer: Writer,
    pub message_id: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TerminalSelection {
    Message { id: u64 },
    NoMessage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub request: u64,
    pub session: u64,
    pub records: Vec<Segment>,
    pub messages: Vec<Message<PayloadSpec>>,
    pub denied_headers: Vec<u64>,
    pub denied_segments: Vec<u64>,
    pub dependency_denials: Vec<DependencyDenial>,
    pub owner: Owner,
    pub target: Target,
    pub request_terminal: bool,
    pub terminal_selection: Option<TerminalSelection>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stream {
    pub declaration: Declaration,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum View {
    Absent,
    Live {
        streams: Vec<Stream>,
    },
    Settling {
        streams: Vec<Stream>,
    },
    Loading,
    Denied,
    Conflicted,
    Invalid,
    Retracted,
    RetainedPartial {
        streams: Vec<Stream>,
    },
    Published {
        message: Message<PayloadSpec>,
        native: Box<Reconstructed>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AuditResult {
    Ok { streams: Vec<Stream> },
    Loading,
    Conflicted,
    Invalid,
}

pub fn decode<T>(value: &serde_json::Value) -> T
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_value(value.clone()).expect("canonical output wire decodes")
}

pub fn encode<T>(value: &T) -> serde_json::Value
where
    T: Serialize,
{
    serde_json::to_value(value).expect("canonical output wire encodes")
}

pub fn nat(id: u64) -> String {
    format!("nat-{id}")
}

pub fn rfc3339(seconds: u64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(
        i64::try_from(seconds).expect("modeled timestamp fits i64"),
        0,
    )
    .expect("modeled timestamp is representable")
    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn small(value: u64) -> u32 {
    u32::try_from(value).expect("modeled index fits u32")
}

pub fn outcome(outcome: Outcome) -> OutputOutcome {
    match outcome {
        Outcome::Complete => OutputOutcome::Complete,
        Outcome::Partial => OutputOutcome::Partial,
    }
}

pub fn source(source: &Source) -> OutputSource {
    match source {
        Source::Provider {
            scope,
            turn,
            attempt,
        } => OutputSource::ProviderTurn {
            scope: CaptureScope {
                kind: CaptureScopeKind::Inference,
                seq: scope + 1,
            },
            turn_index: small(*turn),
            attempt: small(*attempt),
        },
        Source::Auxiliary {
            auxiliary_kind,
            scope,
            turn,
            attempt,
        } => OutputSource::ProviderTurn {
            scope: CaptureScope {
                kind: match auxiliary_kind {
                    AuxiliaryKind::Compaction => CaptureScopeKind::Compaction,
                    AuxiliaryKind::CompactionFallback => CaptureScopeKind::CompactionFallback,
                    AuxiliaryKind::Title => CaptureScopeKind::Title,
                    AuxiliaryKind::AccessReview => CaptureScopeKind::AccessReview,
                },
                seq: *scope,
            },
            turn_index: small(*turn),
            attempt: small(*attempt),
        },
        Source::ToolEffectArguments { call } => OutputSource::ToolEffectArguments {
            tool_call_doc_id: nat(*call),
        },
        Source::Tool { call } => OutputSource::ToolCall {
            tool_call_doc_id: nat(*call),
        },
        Source::Authored { key } => OutputSource::Authored { key: nat(*key) },
    }
}

pub fn writer(writer: &Writer) -> OutputWriter {
    match writer {
        Writer::Request { generation } => OutputWriter::RequestExecution {
            execution_generation: nat(*generation),
        },
        Writer::Tool { call } => OutputWriter::ToolExecution {
            tool_call_doc_id: nat(*call),
        },
    }
}

pub fn publication(publication: &Publication) -> MessagePublication {
    match publication {
        Publication::RequestExecution { generation } => MessagePublication::RequestExecution {
            execution_generation: nat(*generation),
        },
        Publication::RequestRecovery { generation } => MessagePublication::RequestRecovery {
            execution_generation: nat(*generation),
        },
        Publication::ToolDelivery { call } => MessagePublication::ToolDelivery {
            tool_call_doc_id: nat(*call),
        },
        Publication::Fork { origin } => MessagePublication::Fork {
            origin_message_doc_id: nat(*origin),
        },
    }
}

pub fn role(role: Role) -> MessageRole {
    match role {
        Role::System => MessageRole::System,
        Role::User => MessageRole::User,
        Role::Assistant => MessageRole::Assistant,
    }
}

pub fn media_kind(kind: MediaKind) -> OutputMediaKind {
    match kind {
        MediaKind::Image => OutputMediaKind::Image,
        MediaKind::Audio => OutputMediaKind::Audio,
        MediaKind::Video => OutputMediaKind::Video,
        MediaKind::Document => OutputMediaKind::Document,
    }
}

pub fn payload(declaration: &Declaration) -> StreamPayload {
    match declaration.kind {
        PayloadKind::Text => StreamPayload::Text,
        PayloadKind::Reasoning => StreamPayload::Reasoning,
        PayloadKind::Signature => StreamPayload::ReasoningSignature,
        PayloadKind::ReasoningStop => StreamPayload::ReasoningStop,
        PayloadKind::Summary => StreamPayload::ReasoningSummary,
        PayloadKind::Encrypted => StreamPayload::ReasoningEncrypted,
        PayloadKind::Redacted => StreamPayload::ReasoningRedacted,
        PayloadKind::Arguments => {
            let tool = declaration
                .tool
                .as_ref()
                .expect("modeled arguments run carries tool identity");
            StreamPayload::ToolArguments {
                id: tool.id.clone(),
                call_id: tool.call_id.clone(),
                name: tool.name.clone(),
            }
        }
        PayloadKind::ToolOutput => StreamPayload::ToolOutput,
        PayloadKind::Media => StreamPayload::Media {
            media_kind: media_kind(
                declaration
                    .media_kind
                    .expect("modeled media run carries a kind"),
            ),
        },
    }
}

pub fn stream_declaration(declaration: &Declaration) -> StreamDeclaration {
    StreamDeclaration {
        block_index: small(declaration.block),
        part_index: small(declaration.part),
        payload: payload(declaration),
    }
}

pub fn payload_ref(reference: &PayloadRef) -> OutputPayloadRef {
    OutputPayloadRef {
        close_doc_id: nat(reference.close_id),
        stream: small(reference.stream),
    }
}

pub fn presentation(presentation: &Presentation) -> PayloadPresentation {
    match presentation {
        Presentation::Full => PayloadPresentation::Full,
        Presentation::Composed { parts } => PayloadPresentation::Composed {
            parts: parts
                .iter()
                .map(|part| match part {
                    PresentationPart::Range { start, end } => OutputPresentationPart::OutputRange {
                        start_byte: *start,
                        end_byte: *end,
                    },
                    PresentationPart::Literal { bytes } => OutputPresentationPart::Literal {
                        text: String::from_utf8(bytes.clone())
                            .expect("modeled presentation literal is UTF-8"),
                    },
                })
                .collect(),
        },
    }
}

pub fn presented(spec: &PayloadSpec) -> PresentedPayload {
    PresentedPayload {
        output: payload_ref(&spec.reference),
        presentation: presentation(&spec.presentation),
    }
}

pub fn media(media: &Media<PayloadSpec>) -> MediaBlock {
    MediaBlock {
        kind: media_kind(media.kind),
        data: match &media.data {
            MediaPayload::Url { url } => MediaData::Url { url: url.clone() },
            MediaPayload::Base64 { payload } => MediaData::Base64 {
                data: payload_ref(&payload.reference),
            },
            MediaPayload::Raw { payload } => MediaData::Raw {
                data: payload_ref(&payload.reference),
            },
            MediaPayload::String { payload } => MediaData::String {
                data: payload_ref(&payload.reference),
            },
            MediaPayload::Unknown => MediaData::Unknown,
        },
        media_type: media.media_type.as_ref().map(|kind| {
            serde_json::from_value(serde_json::Value::String(kind.clone()))
                .expect("modeled media type is a protocol media type")
        }),
        detail: media.detail.as_ref().map(|detail| {
            serde_json::from_value(serde_json::Value::String(detail.clone()))
                .expect("modeled media detail is a protocol image detail")
        }),
        additional_params: media
            .additional_params
            .as_ref()
            .map(|value| serde_json::from_str(value).expect("modeled additional_params is JSON")),
    }
}

pub fn block(block: &Block<PayloadSpec>) -> MessageBlock {
    match block {
        Block::Text { payload } => MessageBlock::Text {
            text: presented(payload),
        },
        Block::Reasoning { id, parts } => MessageBlock::Reasoning {
            id: id.clone(),
            parts: parts
                .iter()
                .map(|part| match part {
                    ReasoningPart::Text { payload, signature } => OutputReasoningPart::Text {
                        text: payload_ref(&payload.reference),
                        signature: signature.clone(),
                    },
                    ReasoningPart::Encrypted { payload } => OutputReasoningPart::Encrypted {
                        data: payload_ref(&payload.reference),
                    },
                    ReasoningPart::Redacted { payload } => OutputReasoningPart::Redacted {
                        data: payload_ref(&payload.reference),
                    },
                    ReasoningPart::Summary { payload } => OutputReasoningPart::Summary {
                        text: payload_ref(&payload.reference),
                    },
                })
                .collect(),
        },
        Block::ToolCall {
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
            arguments: payload_ref(&arguments.reference),
            signature: signature.clone(),
            additional_params: additional_params.as_ref().map(|value| {
                serde_json::from_str(value).expect("modeled additional_params is JSON")
            }),
        },
        Block::ToolResult {
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
                    ResultPart::Text { payload } => ToolResultPart::Text {
                        text: presented(payload),
                    },
                    ResultPart::Media { value } => ToolResultPart::Media(media(value)),
                })
                .collect(),
        },
        Block::Media { value } => MessageBlock::Media(media(value)),
    }
}

pub fn message(message: &Message<PayloadSpec>) -> TranscriptMessage {
    TranscriptMessage {
        message_key: message.key.clone(),
        session_id: nat(message.header.session),
        node_did: NODE_DID.to_owned(),
        requester_did: None,
        request_doc_id: message.header.request.map(nat),
        publication: publication(&message.header.publication),
        outcome: outcome(message.header.outcome),
        sequence: small(message.sequence),
        role: role(message.header.role),
        native_id: message.native_id.clone(),
        native_request_id: message.native_request_id.clone(),
        blocks: message.blocks.iter().map(block).collect(),
        created_at: rfc3339(message.created_at),
    }
}

pub fn closure(closure: &Closure) -> SourceClose {
    match closure {
        Closure::Closed {
            outcome: closed,
            segments,
            stream_bytes,
        } => SourceClose::Closed {
            outcome: outcome(*closed),
            segments: small(*segments),
            stream_bytes: stream_bytes.clone(),
        },
        Closure::Retracted => SourceClose::Retracted,
    }
}

pub fn segment(record: &Segment, session: u64) -> (String, OutputSegment) {
    let (ordinal, runs, payload_text) = match &record.flush {
        Some(flush) => (
            Some(small(flush.ordinal)),
            flush
                .runs
                .iter()
                .map(|run| SegmentRun {
                    stream: small(run.stream),
                    bytes: small(run.bytes),
                    declaration: run.declaration.as_ref().map(stream_declaration),
                })
                .collect(),
            String::from_utf8(flush.payload.clone()).expect("modeled flush payload is UTF-8"),
        ),
        None => (None, Vec::new(), String::new()),
    };
    let translated = OutputSegment {
        node_did: NODE_DID.to_owned(),
        requester_did: None,
        session_id: nat(session),
        request_doc_id: nat(record.coordinate.request),
        source: source(&record.coordinate.source),
        writer: writer(&record.writer),
        ordinal,
        runs,
        payload: payload_text,
        close: record.close.as_ref().map(closure),
        created_at: rfc3339(record.created_at),
    };
    (nat(record.id), translated)
}

pub fn segments(records: &[Segment], session: u64) -> Vec<(String, OutputSegment)> {
    records
        .iter()
        .map(|record| segment(record, session))
        .collect()
}

pub fn declaration_wire(declaration: &StreamDeclaration) -> Declaration {
    let (kind, tool, media) = match &declaration.payload {
        StreamPayload::Text => (PayloadKind::Text, None, None),
        StreamPayload::Reasoning => (PayloadKind::Reasoning, None, None),
        StreamPayload::ReasoningSummary => (PayloadKind::Summary, None, None),
        StreamPayload::ReasoningEncrypted => (PayloadKind::Encrypted, None, None),
        StreamPayload::ReasoningRedacted => (PayloadKind::Redacted, None, None),
        StreamPayload::ReasoningSignature => (PayloadKind::Signature, None, None),
        StreamPayload::ReasoningStop => (PayloadKind::ReasoningStop, None, None),
        StreamPayload::ToolArguments { id, call_id, name } => (
            PayloadKind::Arguments,
            Some(ToolIdentity {
                id: id.clone(),
                call_id: call_id.clone(),
                name: name.clone(),
            }),
            None,
        ),
        StreamPayload::ToolOutput => (PayloadKind::ToolOutput, None, None),
        StreamPayload::Media { media_kind } => (
            PayloadKind::Media,
            None,
            Some(match media_kind {
                OutputMediaKind::Image => MediaKind::Image,
                OutputMediaKind::Audio => MediaKind::Audio,
                OutputMediaKind::Video => MediaKind::Video,
                OutputMediaKind::Document => MediaKind::Document,
            }),
        ),
    };
    Declaration {
        block: u64::from(declaration.block_index),
        part: u64::from(declaration.part_index),
        kind,
        tool,
        media_kind: media,
    }
}

pub fn stream_wire(stream: &ReconstructedStream) -> Stream {
    Stream {
        declaration: declaration_wire(&stream.declaration),
        bytes: stream.text.as_bytes().to_vec(),
    }
}
