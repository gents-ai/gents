pub mod storage;
use serde::{Deserialize, Serialize};

use crate::message::{
    AudioMediaType, DocumentMediaType, ImageDetail, ImageMediaType, VideoMediaType,
};
use crate::rendered_request::{CaptureScope, CaptureScopeKind};

pub mod extent;
pub mod live;
pub mod origin;
pub mod reconstruction;
pub mod recovery;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OutputSource {
    /// The output of one provider call within the owning request, joined to its
    /// `RenderedRequest` capture by the same coordinate. A retried turn writes
    /// under a higher `attempt` after its predecessor is sealed `Retracted`.
    /// Scope allocation survives reclaim/restart; a new execution cannot reuse
    /// an old coordinate.
    ProviderTurn {
        scope: CaptureScope,
        turn_index: u32,
        attempt: u32,
    },
    /// The output of one tool execution, identified by its exact lifecycle
    /// document. It can outlive the provider turn, and for background tools the
    /// request's active execution.
    ToolCall { tool_call_doc_id: String },
    /// Arguments emitted by an installed plugin for one derived tool call;
    /// audit evidence, not a provider turn or a transcript message.
    ToolEffectArguments { tool_call_doc_id: String },
    /// Content the requester or runtime supplies whole; `key` is the existing
    /// request-scoped idempotency key of that content. Small runtime-written
    /// text around other output is an inline presentation literal, not a source.
    Authored { key: String },
}

impl OutputSource {
    pub fn is_auxiliary_audit(&self) -> bool {
        matches!(
            self,
            Self::ToolEffectArguments { .. }
                | Self::ProviderTurn {
                    scope: CaptureScope {
                        kind: CaptureScopeKind::Compaction
                            | CaptureScopeKind::CompactionFallback
                            | CaptureScopeKind::Title
                            | CaptureScopeKind::AccessReview,
                        ..
                    },
                    ..
                }
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamDeclaration {
    /// Original native position, not a recovered header's compacted index.
    pub block_index: u32,
    pub part_index: u32,
    pub payload: StreamPayload,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StreamPayload {
    Text,
    Reasoning,
    ReasoningSummary,
    /// Retained for replay but never rendered.
    ReasoningEncrypted,
    ReasoningRedacted,
    /// The signature field of a reasoning text part; never rendered.
    ReasoningSignature,
    /// Native JSON argument text exactly as emitted; it may be incomplete
    /// until sealed and is never rewritten to canonicalize.
    ToolArguments {
        id: String,
        call_id: Option<String>,
        name: String,
    },
    ToolOutput,
    Media {
        media_kind: MediaKind,
    },
}

/// One record of a source. Stored as `AgentOutputSegment`. A **flush**
/// (`ordinal` and `runs` present) holds the bytes since the last flush; a
/// **final flush** also carries `close`; a **terminal-only** record (`close`
/// alone) is written when there are no new bytes. Flush and closure
/// coordinates can never collide: a flush is `(request_doc_id, source,
/// ordinal)` dense from zero, closure is `(request_doc_id, source)` with no
/// ordinal, so a late flush landing beyond a closed extent is inert, not a
/// twin. Repeated delivery of the same coordinate and content is idempotent;
/// two visible flushes sharing an ordinal but differing in writer, runs or
/// payload, or two visible closures, are an integrity conflict every reader
/// must surface. The storage index is ordinary, not unique, because a unique
/// index can hide the losing twin of a remote conflict (#1073). A replay
/// reuses the persisted document, including its creation timestamp; an
/// already-existing create is accepted only after verifying the stored fact.
///
/// A flush records output, not lease liveness, and never renews the request
/// lease. It commits inside the existing `config_client::txn::MutationWriteGate`;
/// the owner stamps `created_at` inside that gate. Only that runtime, reading
/// its own store inside the same gate, may decide a lease expired; its deadline
/// is exactly `execution_lease_expires_at`, maintained by bounded explicit
/// owner renewal. A lagging replica observes; it never expires work. Admission
/// rereads generation, lifecycle and effective expiry under the gate — the same
/// effective-expiry check applies to producer closure/retraction, publication,
/// dispatch, terminalization and explicit renewal before their CAS; matching
/// generation alone is insufficient. Recovery uses its existing recovery
/// authority and may close the expired producer's committed extent.
///
/// Only a plain flush is unfenced. Everything that *decides* — any record
/// carrying `close`, accepting and publishing a turn, dispatch,
/// terminalization, recovery — still commits under the matching-generation CAS
/// on the request. Tool-owned output follows the tool lifecycle and never
/// revives a terminal request. There is no per-flush heartbeat; due-only
/// explicit renewal covers both streaming and silent work. Publication and
/// dispatch do not implicitly extend the deadline. Tool-owned closure uses the
/// tool lifecycle's terminal/delivery guard, not a request CAS that could
/// revive the already-terminal originating request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputSegment {
    pub node_did: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requester_did: Option<String>,
    pub session_id: String,
    pub request_doc_id: String,
    pub source: OutputSource,
    /// The source's producer, the same on every record of the source.
    pub writer: OutputWriter,
    /// Present exactly when `runs` is non-empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ordinal: Option<u32>,
    #[serde(
        default,
        deserialize_with = "crate::row::deserialize_null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub runs: Vec<SegmentRun>,
    #[serde(
        default,
        deserialize_with = "crate::row::deserialize_null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub payload: String,
    /// Present on the one record that ends the source. A record has `runs`,
    /// `close`, or both; never neither. No decoder may infer missing closure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub close: Option<SourceClose>,
    pub created_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SegmentRun {
    pub stream: u32,
    pub bytes: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declaration: Option<StreamDeclaration>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OutputWriter {
    /// A request execution generation. A superseded generation can still land
    /// an inert flush, but cannot close, publish, dispatch or terminalize.
    RequestExecution { execution_generation: String },
    /// The tool lifecycle admits output until terminalization.
    ToolExecution { tool_call_doc_id: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputOutcome {
    Complete,
    Partial,
}

/// The one terminal fact about a source, carried by its closing record. At
/// most one per `(request_doc_id, source)`; visible twins are an integrity
/// conflict. It stays valid after its producer's generation is no longer
/// active. Closing a request-owned source commits under the
/// matching-generation CAS on the request, so the producer and recovery can
/// never both close it. Recovery closes exactly the flushes already committed,
/// with a terminal-only record; it never appends bytes for a stale writer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SourceClose {
    /// The exact extent: `segments` flushes and `stream_bytes[n]` bytes in
    /// stream `n`. A reader holding fewer flushes, a gap in ordinals, or a
    /// byte mismatch has an incomplete or conflicting replica and must not
    /// present the content as complete. Beyond-extent flushes are inert:
    /// excluded before data-twin checks, never hiding another close. Fresh
    /// Complete acceptance under the owner gate must include all committed
    /// data flushes, with nondecreasing timestamps no later than closure.
    Closed {
        outcome: OutputOutcome,
        segments: u32,
        #[serde(deserialize_with = "crate::row::deserialize_null_default")]
        stream_bytes: Vec<u64>,
    },
    /// The attempt was abandoned before retry backoff began. Its flushes are
    /// retained but never referenced by a message or shown live, even when the
    /// replacement attempt has not yet emitted a byte.
    Retracted,
}

/// A reference preserves the origin payload and its ACP scope; it grants no new access.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PayloadRef {
    pub close_doc_id: String,
    pub stream: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    System,
    User,
    Assistant,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MessagePublication {
    RequestExecution {
        execution_generation: String,
    },
    RequestRecovery {
        /// Recovery publishes only retained text from an unheaded provider
        /// source; never fabricate native metadata.
        execution_generation: String,
    },
    /// Tool-owned invocation reply or completion notification. Payload
    /// references name the originating tool's source; publication request
    /// membership is not payload-source ownership.
    ToolDelivery {
        tool_call_doc_id: String,
    },
    /// No live request membership; all referenced payloads remain at origin.
    /// Forks retain origin tool IDs as provenance, never copied executable rows.
    Fork {
        origin_message_doc_id: String,
    },
}

/// The terminal owner writes this with terminal state and final header publication.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TerminalOutput {
    Message {
        message_doc_id: String,
    },
    /// Explicit absence of an answer, including admission/pre-output failure.
    /// A deliberately published empty assistant message uses Message instead.
    NoMessage,
}

/// The single durable transcript message. Stored as `AgentMessage`. Create-only:
/// written once, after every closing record it references, and never updated.
/// Two visible messages sharing `message_key` or `(session_id, sequence)` with
/// different content are an integrity conflict, surfaced the same way as
/// segment twins.
///
/// A successfully accepted provider turn atomically publishes its Complete
/// closing record, assistant header and pending `AgentToolCall` rows before any
/// tool is dispatched (#945). Partial turns may publish diagnostic headers, but
/// any retained tool-call blocks name terminal, nondispatchable lifecycle rows.
/// Once published, an accepted turn cannot be retracted/resampled because a
/// later dispatch fails. Recovery resumes its existing rows, never new calls.
///
/// `blocks` is in native content order, so reconstruction yields the exact
/// native `Message` (including `Message::Assistant.id` and `request_id` via `native_id` and
/// `native_request_id`).
///
/// Recovery of an unheaded provider source is deliberately narrower: close its
/// committed extent Partial (or reuse its existing Partial closure), then
/// publish only ordinary Text streams at native part zero as Full
/// PresentedPayload references, in original block order, with outcome Partial
/// and native_id and native_request_id None. Never turn argument/reasoning/media bytes into text or
/// fabricate their missing metadata. If no eligible text stream exists, publish
/// no assistant header for that source; retain every omitted stream as
/// diagnostic data. An existing published header is resolved unchanged, not
/// replaced by this recovery policy. Provider-input narrowing still applies.
/// Validate source extent/run accounting for all streams, but decode native
/// payloads only for referenced blocks.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptMessage {
    /// Existing owner-defined idempotency key (tool-result key, request-scoped
    /// authored key, or provider-turn coordinate).
    pub message_key: String,
    pub session_id: String,
    pub node_did: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requester_did: Option<String>,
    /// Exact request this message belongs to, not necessarily the request that
    /// produced its referenced bytes. Absent only for history a fork placed in
    /// a child session, which must not acquire live request membership.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_doc_id: Option<String>,
    pub publication: MessagePublication,
    /// Set by the publisher; never derived from the closing records its blocks
    /// reference, so a message with no payloads needs no closing record.
    pub outcome: OutputOutcome,
    pub sequence: u32,
    pub role: MessageRole,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_id: Option<String>,
    /// The provider's response request id, carried like `native_id`: only a
    /// provider assistant message has one (never system, user or recovery).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_request_id: Option<String>,
    #[serde(
        default,
        deserialize_with = "storage::deserialize_blocks",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub blocks: Vec<MessageBlock>,
    pub created_at: String,
}

impl TranscriptMessage {
    /// Every sealed payload dependency named by this header, in native block
    /// order.
    pub fn payload_references(&self) -> Vec<&PayloadRef> {
        let mut references = Vec::new();
        for block in &self.blocks {
            match block {
                MessageBlock::Text { text } => references.push(&text.output),
                MessageBlock::Reasoning { parts, .. } => {
                    for part in parts {
                        match part {
                            ReasoningPart::Text { text, .. } | ReasoningPart::Summary { text } => {
                                references.push(text)
                            }
                            ReasoningPart::Encrypted { data }
                            | ReasoningPart::Redacted { data } => references.push(data),
                        }
                    }
                }
                MessageBlock::ToolCall { arguments, .. } => references.push(arguments),
                MessageBlock::ToolResult { parts, .. } => {
                    for part in parts {
                        match part {
                            ToolResultPart::Text { text } => references.push(&text.output),
                            ToolResultPart::Media(media) => match &media.data {
                                MediaData::Base64 { data }
                                | MediaData::Raw { data }
                                | MediaData::String { data } => references.push(data),
                                MediaData::Url { .. } | MediaData::Unknown => {}
                            },
                        }
                    }
                }
                MessageBlock::Media(media) => match &media.data {
                    MediaData::Base64 { data }
                    | MediaData::Raw { data }
                    | MediaData::String { data } => references.push(data),
                    MediaData::Url { .. } | MediaData::Unknown => {}
                },
            }
        }
        references
    }
}

/// One native content item with its payload strings replaced by references.
/// Small provider-opaque values (ids, signatures, additional params) stay
/// inline: they are structure, not streamed output.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum MessageBlock {
    Text {
        /// Also composes background notification wrappers around tool output
        /// without copying that output into a new authored stream.
        text: PresentedPayload,
    },
    Reasoning {
        id: Option<String>,
        parts: Vec<ReasoningPart>,
    },
    /// The model's call. Arguments are output of the provider turn; execution
    /// state is `AgentToolCall`, which carries no payload.
    ToolCall {
        tool_call_doc_id: String,
        id: String,
        call_id: Option<String>,
        name: String,
        arguments: PayloadRef,
        signature: Option<String>,
        additional_params: Option<serde_json::Value>,
    },
    /// The result paired to its call by exact document identity; its text
    /// parts reference the tool call's own output stream.
    ToolResult {
        tool_call_doc_id: String,
        id: String,
        call_id: Option<String>,
        parts: Vec<ToolResultPart>,
    },
    Media(MediaBlock),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReasoningPart {
    Text {
        text: PayloadRef,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    Encrypted {
        data: PayloadRef,
    },
    Redacted {
        data: PayloadRef,
    },
    Summary {
        text: PayloadRef,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ToolResultPart {
    Text { text: PresentedPayload },
    Media(MediaBlock),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresentedPayload {
    /// Full output, retained for inspection and retrieval.
    pub output: PayloadRef,
    /// Exact native text selected at the owned provider-input boundary.
    pub presentation: PayloadPresentation,
}

/// Deterministic byte composition, not a rerun of mutable truncation policy.
/// Ranges index the full output at UTF-8 boundaries; literals are the small
/// runtime-written pieces between them, stored once, inline, here. Empty parts
/// means an empty presentation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PayloadPresentation {
    Full,
    Composed { parts: Vec<PresentationPart> },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PresentationPart {
    OutputRange { start_byte: u64, end_byte: u64 },
    Literal { text: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Image,
    Audio,
    Video,
    Document,
}

/// Native media with inline data moved to a stream. URL sources stay inline.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaBlock {
    pub kind: MediaKind,
    pub data: MediaData,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<MediaType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<ImageDetail>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_params: Option<serde_json::Value>,
}

/// Mirrors `DocumentSourceKind` with the data-bearing variants referenced.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum MediaData {
    Url {
        url: String,
    },
    Base64 {
        data: PayloadRef,
    },
    /// Raw bytes, stored base64 in the stream and decoded on reconstruction.
    Raw {
        data: PayloadRef,
    },
    String {
        data: PayloadRef,
    },
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MediaType {
    Image(ImageMediaType),
    Audio(AudioMediaType),
    Video(VideoMediaType),
    Document(DocumentMediaType),
}

/// Why a projection could not produce content: the only ways a transcript read
/// can fail; none degrades to shorter text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReconstructionError {
    /// The closing record is unavailable, with no known authorization denial.
    /// Absence alone proves neither lag nor denial.
    UnresolvedClose { close_doc_id: String },
    /// The authorization owner explicitly denied a required dependency. This
    /// is an access error, never a loading state; do not infer it from absence.
    AccessDenied { doc_id: String },
    /// A reference names a plain flush, retracted source or absent stream.
    InvalidReference { reference: PayloadRef },
    /// An ordinal inside the sealed extent is not visible; the source is
    /// incomplete, not shorter.
    MissingSegment { close_doc_id: String, ordinal: u32 },
    /// Visible twins at a coordinate disagree in writer, runs or payload.
    ConflictingSegments { close_doc_id: String, ordinal: u32 },
    /// A segment's runs do not account for its payload, the assembled stream
    /// disagrees with the sealed byte length, or its declaration disagrees
    /// with the block that references it.
    ExtentMismatch { reference: PayloadRef, bytes: u64 },
    /// More than one closing record is visible for a source.
    ConflictingClosures {
        request_doc_id: String,
        source: OutputSource,
    },
    ConflictingMessages {
        session_id: String,
        message_key: String,
    },
    /// Segments within the sealed extent, or the closing record, disagree about the
    /// source's producer.
    InvalidWriter {
        request_doc_id: String,
        source: OutputSource,
    },
    /// Terminal lifecycle was observed without its output selection/dependencies.
    UnresolvedTerminalOutput { request_doc_id: String },
    /// Illegal role/block combination or non-native block order.
    InvalidStructure { detail: String },
    /// A presentation range is out of bounds or splits a UTF-8 sequence.
    InvalidPresentation { reference: PayloadRef },
    /// Sealed bytes do not decode as their declared payload (argument JSON,
    /// base64 media).
    InvalidPayload { reference: PayloadRef },
}

impl ReconstructionError {
    /// Recoverable when immutable dependencies replicate; authorization and
    /// integrity failures are terminal.
    pub fn is_incomplete(&self) -> bool {
        matches!(
            self,
            Self::UnresolvedClose { .. }
                | Self::MissingSegment { .. }
                | Self::UnresolvedTerminalOutput { .. }
        )
    }
}

impl std::fmt::Display for ReconstructionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnresolvedClose { close_doc_id } => {
                write!(f, "unresolved closing segment {close_doc_id}")
            }
            Self::AccessDenied { doc_id } => {
                write!(f, "access denied to output dependency {doc_id}")
            }
            Self::InvalidReference { reference } => write!(
                f,
                "invalid output reference {}:{}",
                reference.close_doc_id, reference.stream
            ),
            Self::MissingSegment {
                close_doc_id,
                ordinal,
            } => write!(
                f,
                "missing output segment {ordinal} for closure {close_doc_id}"
            ),
            Self::ConflictingSegments {
                close_doc_id,
                ordinal,
            } => write!(
                f,
                "conflicting output segment {ordinal} for closure {close_doc_id}"
            ),
            Self::ExtentMismatch { reference, bytes } => write!(
                f,
                "sealed output extent mismatch for {}:{} (expected {bytes} bytes)",
                reference.close_doc_id, reference.stream
            ),
            Self::ConflictingClosures {
                request_doc_id,
                source,
            } => write!(
                f,
                "conflicting output closures for request {request_doc_id}, source {source:?}"
            ),
            Self::ConflictingMessages {
                session_id,
                message_key,
            } => write!(
                f,
                "conflicting transcript message {message_key} in session {session_id}"
            ),
            Self::InvalidWriter {
                request_doc_id,
                source,
            } => write!(
                f,
                "invalid output writer for request {request_doc_id}, source {source:?}"
            ),
            Self::UnresolvedTerminalOutput { request_doc_id } => {
                write!(f, "unresolved terminal output for request {request_doc_id}")
            }
            Self::InvalidStructure { detail } => {
                write!(f, "invalid canonical output structure: {detail}")
            }
            Self::InvalidPresentation { reference } => write!(
                f,
                "invalid presentation for {}:{}",
                reference.close_doc_id, reference.stream
            ),
            Self::InvalidPayload { reference } => write!(
                f,
                "invalid payload for {}:{}",
                reference.close_doc_id, reference.stream
            ),
        }
    }
}

impl std::error::Error for ReconstructionError {}

/// Output for a request that no message references yet, grouped by declared
/// native position; retracted sources excluded. Built from the same segments
/// the transcript will reference, so there is no rollover, overlap or repair
/// step. Headers, closing records and segments may replicate in any order and
/// are projected together: a header whose segments have not arrived is
/// incomplete, never a second live copy. Unclosed request-owned output is
/// eligible only for the current generation of a nonterminal request; tool-owned
/// output follows the tool lifecycle. Authored sources are never shown live:
/// they appear only with their header. This projection is an observation for
/// display; it never decides that work expired. After terminal selection is
/// visible, unreferenced streams from a closed Partial provider source are
/// RetainedPartial, not pending publication. Resolve a selected header before
/// classifying its streams. Opaque reasoning is never rendered, including in
/// diagnostics.
#[derive(Clone, Debug, PartialEq)]
pub struct LiveOutput {
    pub request_doc_id: String,
    pub streams: Vec<LiveStream>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LiveStream {
    pub source: OutputSource,
    pub stream: u32,
    pub declaration: StreamDeclaration,
    pub state: LiveStreamState,
    /// This stream's bytes from the source's contiguous segments, starting at
    /// ordinal zero and stopping at the first missing one.
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiveStreamState {
    /// No terminal fact is locally visible; this does not prove remote liveness.
    Unclosed,
    /// Closure/publication is known but the exact message or its dependencies
    /// are not yet reconstructable; never label a known-closed source as live
    /// activity.
    PendingPublication { outcome: OutputOutcome },
    /// Closed Partial provider bytes outside published native messages after
    /// request terminalization; diagnostic-only.
    RetainedPartial,
}
