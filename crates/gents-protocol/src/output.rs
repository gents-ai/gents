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
/// them, and a live reader can find in-flight output without a mutable
/// placeholder document.
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
    /// predecessor's segments remain as retained partial output and are simply
    /// never referenced by a message.
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

/// Which native payload a stream carries. Present on every segment so a live
/// projection can render unsealed output without a header.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PayloadKind {
    Text,
    Reasoning,
    ReasoningSummary,
    /// Provider-opaque reasoning (`Encrypted` / `Redacted`); never rendered.
    ReasoningOpaque,
    /// Canonical JSON text of a tool call's arguments.
    ToolArguments,
    ToolOutput,
    /// Inline media data exactly as the native value carries it.
    Media,
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
/// `execution_generation` are an integrity conflict that every reader must
/// surface; the storage index is ordinary, not unique, because a unique index
/// can hide the losing twin of a remote conflict (#1073).
///
/// The segment write is the semantic-progress write: it goes through the
/// existing execution-lease owner in the same transaction that renews the
/// lease, exactly as the response progress write did. There is no separate
/// heartbeat.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputSegment {
    pub agent_did: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requester_did: Option<String>,
    pub session_id: String,
    /// Exact physical request that owns the execution producing this output.
    pub request_doc_id: String,
    /// The claim generation that wrote this segment. A writer whose generation
    /// is no longer current cannot commit; a segment from a superseded
    /// generation is never referenced by a message.
    pub execution_generation: String,
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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PayloadRef {
    /// Owning request of the referenced segments.
    pub request_doc_id: String,
    pub source: OutputSource,
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
        text: PayloadRef,
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
    Text {
        text: PayloadRef,
        /// Absent when the model received the full stream.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        presented: Option<PresentedWindow>,
    },
    Media(MediaBlock),
}

/// The deterministic narrowing applied to a tool output at the provider-input
/// boundary. Replaces the spill document plus truncated copy: the full output
/// is the stream, and the presentation is a window over it.
///
/// Design TODO: confirm against `truncation::TruncationResult` that head/tail
/// byte windows plus the trigger reproduce every current truncation mode
/// exactly, before Lean models it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresentedWindow {
    pub head_bytes: u64,
    pub tail_bytes: u64,
    pub original_lines: u64,
    pub original_bytes: u64,
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
    Gap { reference: PayloadRef, ordinal: u32 },
    /// Two visible segments share a coordinate.
    ConflictingSegments { reference: PayloadRef, ordinal: u32 },
    /// The assembled stream disagrees with the sealed byte length or kind.
    ExtentMismatch { reference: PayloadRef, bytes: u64 },
    /// A referenced tool call or request is missing or not authorized.
    UnresolvedReference { doc_id: String },
}

/// Unsealed output for a request: segments no message references yet, grouped
/// by stream in coordinate order, restricted to the current execution
/// generation and, per provider turn, the highest attempt. This is the live
/// preview and streaming view; it uses the same segments the completed
/// transcript will seal, so there is no rollover, overlap, or repair step.
#[derive(Clone, Debug, PartialEq)]
pub struct LiveOutput {
    pub request_doc_id: String,
    pub streams: Vec<LiveStream>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LiveStream {
    pub source: OutputSource,
    pub stream: u32,
    pub kind: PayloadKind,
    /// Contiguous payload from ordinal zero; stops at the first gap.
    pub text: String,
    pub next_ordinal: u32,
}
