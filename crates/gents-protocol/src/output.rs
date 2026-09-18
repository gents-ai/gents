//! Canonical durable output (#1571): every byte of model, tool, and authored
//! content is persisted once, as an immutable [`OutputSegment`].
//!
//! A transcript message is a create-only [`TranscriptMessage`] header written
//! when its content is complete. The header mirrors the native
//! [`crate::message::Message`] structure exactly, except that every payload
//! string is replaced by a [`PayloadRef`] naming a run of segments. Live
//! previews, answer streaming, completed transcripts, provider input, forks,
//! and exports are all projections of the same segments; no consumer reads a
//! second durable text copy, because none exists.
//!
//! What this replaces: cumulative-prefix rewrites of `AgentResponse.content` /
//! `reasoning`, the serialized native message in `AgentMessage.content`, the
//! extracted `AgentMessage.reasoning` copy, in-flight `AgentMessage` upserts,
//! `AgentToolCall.args` / `result` / `partial_output_tail`, and the
//! `AgentToolResult` spill collection.
//!
//! Distinct facts that remain, each for a distinct guarantee:
//! `AgentRequest.content` is the requester-signed admission input, not the
//! transcript (the runtime may template or wrap it before it becomes a user
//! message). `RenderedRequest` is what a provider received. Neither is a
//! transcript source, and the transcript is not reconstructed from them.

use serde::{Deserialize, Serialize};

use crate::message::{
    AudioMediaType, DocumentMediaType, ImageDetail, ImageMediaType, VideoMediaType,
};
use crate::rendered_request::CaptureScope;

/// What produced a run of content. Its identity is known before the first
/// byte arrives, so segments never wait on the message that later references
/// them. Payload-free source control supplies live disposition and structure.
///
/// No variant mints a new logical ID: each reuses a coordinate or document
/// identity that already exists for another reason (#1425).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OutputSource {
    /// The output of one provider call within the owning request. This is the
    /// same `(scope, turn_index, attempt)` coordinate as that call's
    /// `RenderedRequest` capture: the input and output of a provider call join
    /// on it. A retried or retracted turn writes under a higher `attempt`; its
    /// predecessor's segments remain as retained partial output. Retraction
    /// must first commit `OutputSourceState::Retracted`, even if the next
    /// attempt never emits a byte. Scope allocation survives reclaim/restart;
    /// a new execution cannot reuse an old provider coordinate.
    ProviderTurn {
        scope: CaptureScope,
        turn_index: u32,
        attempt: u32,
    },
    /// The output of one tool execution, identified by its exact lifecycle
    /// document. Tool output may stream (bash) or arrive whole; both are
    /// segments. It can outlive the provider turn, and for background tools the
    /// request's active execution.
    ToolCall { tool_call_doc_id: String },
    /// Content the requester or runtime supplies whole: the prompt as placed in
    /// the transcript, request context, output-obligation reminders, and
    /// background-completion notifications. `key` is the existing
    /// request-scoped idempotency key of that message.
    Authored { key: String },
}

/// Which native payload a stream carries. Present on every segment for
/// consistency checking against its source declaration and sealed block.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PayloadKind {
    Text,
    Reasoning,
    ReasoningSummary,
    /// Provider-opaque reasoning (`Encrypted` / `Redacted`); never rendered.
    ReasoningOpaque,
    /// Native JSON argument text, which may be incomplete while streaming.
    /// Decode only after sealing; never rewrite emitted fragments to canonicalize.
    ToolArguments,
    ToolOutput,
    /// Inline media data exactly as the native value carries it.
    Media,
}

/// Existing authority that admits a write; never a new lease or host identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OutputWriter {
    /// The request lifecycle CAS admits progress and renews its lease in the
    /// same transaction. Stale generations cannot append or publish.
    RequestExecution { execution_generation: String },
    /// The existing tool lifecycle admits output until terminalization, and
    /// its delivery owner admits authored completion notifications afterward.
    /// This does not renew or reopen the originating request's lease.
    ToolExecution { tool_call_doc_id: String },
}

/// Payload-free source control document (`AgentOutputSource`). The existing
/// producing owner opens it before writing segments. Stream declarations may
/// only be appended; each declaration is immutable after publication.
/// Closing/retracting and the final segment write share the owner's transaction.
/// Closed sources remain readable across request generation changes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputSourceRecord {
    pub agent_did: String,
    pub requester_did: Option<String>,
    pub session_id: String,
    pub request_doc_id: String,
    pub source: OutputSource,
    pub writer: OutputWriter,
    pub streams: Vec<StreamDeclaration>,
    pub state: OutputSourceState,
    pub created_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum OutputSourceState {
    Open,
    /// Exact extents, including empty streams. Closure is terminal; message
    /// publication can be replayed by its existing idempotency owner.
    Closed {
        outcome: MessageOutcome,
        payloads: Vec<PayloadRef>,
    },
    /// Terminal and never eligible for message publication or live preview.
    Retracted,
}

/// Published before this stream's first segment, including empty streams.
/// Native block/part positions are assigned on opening, not by payload kind.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamDeclaration {
    pub stream: u32,
    pub block_index: u32,
    pub part_index: u32,
    pub role: MessageRole,
    pub payload: LivePayload,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LivePayload {
    Text,
    Reasoning,
    ReasoningSummary,
    ReasoningOpaque,
    /// Provider identity is known before dispatch creates a tool lifecycle row.
    /// The sealed block adds that row's exact identity. Buffer argument bytes
    /// until this metadata is known; do not create a fake tool execution.
    ToolArguments {
        id: String,
        call_id: Option<String>,
        name: String,
    },
    ToolOutput {
        tool_call_doc_id: String,
    },
    Media {
        media_kind: MediaKind,
    },
}

/// One immutable, create-only run of payload. Stored as `AgentOutputSegment`.
///
/// Coordinate: `(request_doc_id, source, stream, ordinal)`. `stream` numbers
/// the payloads within a source in the order they open; `ordinal` is dense
/// from zero within a stream. Concatenating a stream's payloads in ordinal
/// order yields the native string exactly. Writers split only where the
/// batch interval, a size threshold, or a stream boundary falls — never per
/// token — and a stream always ends at a segment boundary.
///
/// Repeated delivery of the same coordinate and payload is idempotent. Two
/// visible segments sharing a coordinate but differing in payload or
/// writer or kind are an integrity conflict that every reader must
/// surface; the storage index is ordinary, not unique, because a unique index
/// can hide the losing twin of a remote conflict (#1073).
///
/// The segment write is semantic progress, authorized by `OutputWriter`.
/// Exact replay does not renew a lease. Request-owned progress renews in the
/// same transaction; independently running tools use their lifecycle owner.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputSegment {
    pub agent_did: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requester_did: Option<String>,
    pub session_id: String,
    /// Exact physical request that owns the execution producing this output.
    pub request_doc_id: String,
    pub writer: OutputWriter,
    pub source: OutputSource,
    pub stream: u32,
    pub ordinal: u32,
    pub kind: PayloadKind,
    pub payload: String,
    pub created_at: String,
}

/// A sealed reference to one complete stream. `segments` and `bytes` are the
/// exact extent: a reader holding fewer segments, a gap in ordinals, or a byte
/// mismatch has an incomplete or conflicting replica and must not present the
/// message as complete. An empty native string is `segments: 0, bytes: 0`.
///
/// The reference carries its own source so a message can name output it did
/// not produce: a tool-result block names the tool call's stream, a background
/// notification names the completed tool's output, and a forked message names
/// its origin's segments instead of copying them.
/// Origin source/segment documents and referenced request/tool provenance are
/// retained across session close/removal; this stack introduces no payload GC.
/// ACP still applies to every dependency. A fork cannot broaden origin access.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PayloadRef {
    /// Owning request of the referenced segments.
    pub request_doc_id: String,
    pub source: OutputSource,
    /// Binds the seal to its historical producer, not today's active claim.
    pub writer: OutputWriter,
    pub stream: u32,
    pub segments: u32,
    pub bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    System,
    User,
    Assistant,
}

/// Why a message's content ends where it does. A partial turn kept after
/// interruption or failure is an explicit outcome, never a complete message
/// with silently shorter text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageOutcome {
    Complete,
    Interrupted,
    Failed,
}

/// Publication authority/provenance, checked atomically by the existing owner.
/// Recovery may seal persisted partial output only through the winning request
/// terminalization CAS; it cannot append bytes on behalf of a stale writer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MessagePublication {
    RequestExecution {
        execution_generation: String,
    },
    RequestRecovery {
        execution_generation: String,
    },
    ToolDelivery {
        tool_call_doc_id: String,
    },
    /// No live request membership; all referenced payloads remain at origin.
    /// Forks retain origin tool IDs as provenance, never copied executable rows.
    Fork {
        origin_message_doc_id: String,
    },
}

/// The single durable transcript message. Stored as `AgentMessage`.
///
/// Create-only: written once, when every stream it references is complete, and
/// never updated. There is no in-flight message row; unsealed output is
/// visible as segments. `sequence` is the order coordinate within the session
/// and keeps its existing durable allocator. Two visible messages sharing
/// `message_key` or `(session_id, sequence)` with different content are an
/// integrity conflict, surfaced the same way as segment twins.
///
/// `blocks` is in native content order, so reconstruction yields the exact
/// native `Message` (including `Message::Assistant.id` via `native_id`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptMessage {
    /// Existing owner-defined idempotency key (tool-result key, request-scoped
    /// authored key, or provider-turn coordinate).
    pub message_key: String,
    pub session_id: String,
    pub agent_did: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requester_did: Option<String>,
    /// Exact request this message belongs to. Absent only for history a fork
    /// placed in a child session, which must not acquire live request
    /// membership. The logical `request_id` is not repeated here (#1425).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_doc_id: Option<String>,
    pub publication: MessagePublication,
    pub sequence: u32,
    pub role: MessageRole,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_id: Option<String>,
    pub blocks: Vec<MessageBlock>,
    pub outcome: MessageOutcome,
    pub created_at: String,
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
    /// The result paired to its call by exact document identity. Its text
    /// parts reference the tool call's own output stream rather than copying
    /// it into the message.
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
/// Ranges index the full output at UTF-8 boundaries. Authored pieces reference
/// ordinary authored segments (markers, normalized separators, retrieval hints).
/// This represents line normalization and head/tail presentation without a
/// second copy of selected tool bytes. Retrieval hints identify the tool call,
/// never the retired spill collection. Empty parts means an empty presentation.
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
    Authored { text: PayloadRef },
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

/// Why a projection could not produce content. Every consumer goes through the
/// one shared reconstruction, so these are the only ways a transcript read can
/// fail; none of them degrades to shorter text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReconstructionError {
    /// Fewer segments are locally visible than the reference seals. Expected
    /// while replication is behind; the message is incomplete, not shorter.
    Incomplete {
        reference: PayloadRef,
        visible_segments: u32,
    },
    /// An ordinal inside the sealed extent is absent.
    Gap {
        reference: PayloadRef,
        ordinal: u32,
    },
    /// Visible twins at a coordinate disagree in payload, writer or kind.
    ConflictingSegments {
        reference: PayloadRef,
        ordinal: u32,
    },
    /// The assembled stream disagrees with the sealed byte length or kind.
    ExtentMismatch {
        reference: PayloadRef,
        bytes: u64,
    },
    /// A referenced tool call or request is missing or not authorized.
    UnresolvedReference {
        doc_id: String,
    },
    ConflictingMessages {
        session_id: String,
        message_key: String,
    },
    ConflictingSources {
        request_doc_id: String,
        source: OutputSource,
    },
    UnresolvedSource {
        request_doc_id: String,
        source: OutputSource,
    },
    InvalidStructure {
        detail: String,
    },
    InvalidPresentation {
        reference: PayloadRef,
    },
    InvalidPayload {
        reference: PayloadRef,
    },
}

/// Unsealed output for a request: segments no message references yet, grouped
/// by declared native position. Source control is required: absent control is
/// incomplete replication, never evidence that an attempt is live. Retracted
/// sources are excluded even before replacement output arrives. Open request
/// sources require the current generation; tool sources follow tool lifecycle.
/// Closed, unpublished sources remain visible as pending publication, including
/// interrupted/failed output. Header/source/segment observations are projected
/// together: a header arriving before its segments is incomplete, not a second
/// live copy. Historical seals never depend on today's execution generation.
#[derive(Clone, Debug, PartialEq)]
pub struct LiveOutput {
    pub request_doc_id: String,
    pub streams: Vec<LiveStream>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LiveStream {
    pub source: OutputSource,
    pub declaration: StreamDeclaration,
    pub state: LiveStreamState,
    /// Contiguous payload from ordinal zero; stops at the first gap.
    pub text: String,
    pub next_ordinal: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiveStreamState {
    Streaming,
    PendingPublication { outcome: MessageOutcome },
}
