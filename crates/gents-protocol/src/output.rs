//! Canonical durable output (#1571): every byte of model, tool, and authored
//! content is persisted once, and nothing in this model is ever updated.
//!
//! Three immutable, create-only facts:
//!
//! - [`OutputSegment`] — a run of payload bytes. The only place content lives.
//! - [`OutputSeal`] — the one terminal fact about a source: closed with exact
//!   stream extents, or retracted.
//! - [`TranscriptMessage`] — a header mirroring the native
//!   [`crate::message::Message`] structure, with every payload string replaced
//!   by a [`PayloadRef`] to a sealed stream.
//!
//! The collections only grow, so replication is set union and every reader
//! question is "which facts are visible": no seal means the source is still
//! open, a seal with missing segments means the replica is incomplete. Live
//! previews, answer streaming, completed transcripts, provider input, forks and
//! exports are projections of these same facts through one shared
//! reconstruction; no consumer reads a second durable text copy, because none
//! exists.
//!
//! What this replaces: `AgentResponse` and its cumulative-prefix rewrites, the
//! serialized native message in `AgentMessage.content`, the extracted
//! `AgentMessage.reasoning` copy, in-flight `AgentMessage` upserts,
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
/// byte arrives, so segments never wait on the seal or message that later
/// names them.
///
/// No variant mints a new logical ID: each reuses a coordinate or document
/// identity that already exists for another reason (#1425).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OutputSource {
    /// The output of one provider call within the owning request. This is the
    /// same `(scope, turn_index, attempt)` coordinate as that call's
    /// `RenderedRequest` capture: the input and output of a provider call join
    /// on it. A retried turn writes under a higher `attempt` after its
    /// predecessor is sealed `Retracted`. Scope allocation survives
    /// reclaim/restart; a new execution cannot reuse an old coordinate.
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
    /// the transcript, request context, output-obligation reminders,
    /// presentation markers, and background-completion notifications. `key` is
    /// the existing request-scoped idempotency key of that content.
    Authored { key: String },
}

/// What a stream carries and where it sits in the native message. Carried by
/// the stream's first segment, so live views render unsealed output in native
/// structure without any control document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamDeclaration {
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
    /// Provider-opaque reasoning (`Encrypted` / `Redacted`); never rendered.
    ReasoningOpaque,
    /// Native JSON argument text exactly as emitted; it may be incomplete
    /// until sealed and is never rewritten to canonicalize. Buffer argument
    /// bytes until the provider's call identity is known.
    ToolArguments {
        id: String,
        call_id: Option<String>,
        name: String,
    },
    ToolOutput,
    /// Inline media data exactly as the native value carries it.
    Media {
        media_kind: MediaKind,
    },
}

/// One immutable run of payload. Stored as `AgentOutputSegment`.
///
/// Coordinate: `(request_doc_id, source, stream, ordinal)`. `stream` numbers
/// the payloads within a source in the order they open; `ordinal` is dense
/// from zero within a stream. Every stream has an ordinal-zero segment, which
/// carries its declaration; an empty native string is that one segment with an
/// empty payload. Concatenating a stream's payloads in ordinal order yields
/// the native string exactly. Writers split only where the batch interval, a
/// size threshold, or a stream boundary falls — never per token.
///
/// Repeated delivery of the same coordinate and content is idempotent. Two
/// visible segments sharing a coordinate but differing in payload or
/// declaration are an integrity conflict every reader must surface; the
/// storage index is ordinary, not unique, because a unique index can hide the
/// losing twin of a remote conflict (#1073).
///
/// Writing a segment is semantic progress. The source's existing owner admits
/// it: request-owned progress commits through the execution-lease CAS and
/// renews the lease in the same transaction (exact replay does not renew);
/// tool-owned output commits through the tool lifecycle and never revives a
/// terminal request. There is no separate heartbeat.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputSegment {
    pub agent_did: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requester_did: Option<String>,
    pub session_id: String,
    /// Exact physical request that owns the execution producing this output.
    pub request_doc_id: String,
    pub source: OutputSource,
    pub stream: u32,
    pub ordinal: u32,
    /// Present exactly on ordinal zero.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declaration: Option<StreamDeclaration>,
    pub payload: String,
    pub created_at: String,
}

/// The existing authority that produced a source; never a new lease or host
/// identity. Recorded once, on the seal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OutputWriter {
    /// The request lifecycle CAS. Stale generations cannot append or seal.
    RequestExecution { execution_generation: String },
    /// The tool lifecycle admits output until terminalization, and its
    /// delivery owner admits authored completion notifications afterward.
    ToolExecution { tool_call_doc_id: String },
}

/// Why a source's content ends where it does. Partial output kept after
/// interruption or failure is an explicit outcome, never a complete source
/// with silently shorter text. A message's outcome is that of the seals it
/// references; it is not stored again on the header.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputOutcome {
    Complete,
    Interrupted,
    Failed,
}

/// The one terminal fact about a source. Stored as `AgentOutputSeal`.
///
/// Written once by the source's owner in the transaction that writes its last
/// segment. At most one seal per `(request_doc_id, source)`; visible twins are
/// an integrity conflict. A sealed source accepts no further segments, and a
/// seal stays valid after its producer's generation is no longer active.
/// Recovery seals only the bytes already committed, under the winning request
/// terminalization CAS; it never appends on behalf of a stale writer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputSeal {
    pub agent_did: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requester_did: Option<String>,
    pub session_id: String,
    pub request_doc_id: String,
    pub source: OutputSource,
    pub writer: OutputWriter,
    pub disposition: SealDisposition,
    pub created_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SealDisposition {
    /// `streams[n]` is the exact extent of stream `n`. A reader holding fewer
    /// segments, a gap in ordinals, or a byte mismatch has an incomplete or
    /// conflicting replica and must not present the content as complete.
    Closed {
        outcome: OutputOutcome,
        streams: Vec<StreamExtent>,
    },
    /// The attempt was abandoned before retry backoff began. Its segments are
    /// retained but never referenced by a message or shown live, even when the
    /// replacement attempt has not yet emitted a byte.
    Retracted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamExtent {
    pub segments: u32,
    pub bytes: u64,
}

/// One sealed stream, by the seal's exact document identity. The seal is
/// immutable, so this pins the content it names; there is no logical key or
/// repeated extent to disagree with it. A forked message carries the same
/// references as its origin — payloads are never copied.
///
/// Seals, their segments, and the request/tool provenance they name are
/// retained across session close or removal; this stack introduces no payload
/// GC. ACP applies to every dependency: a reference never broadens access.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PayloadRef {
    pub seal_doc_id: String,
    pub stream: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    System,
    User,
    Assistant,
}

/// Publication authority/provenance, checked atomically by the existing owner.
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
/// Create-only: written once, after every seal it references, and never
/// updated. There is no in-flight message row; unsealed output is visible as
/// segments. `sequence` is the order coordinate within the session and keeps
/// its existing durable allocator. Two visible messages sharing `message_key`
/// or `(session_id, sequence)` with different content are an integrity
/// conflict, surfaced the same way as segment twins.
///
/// An assistant message is published when its provider turn seals, in one
/// transaction with the pending `AgentToolCall` rows for its tool-call blocks,
/// and **before any of those tools is dispatched**. The assistant turn is
/// therefore durable, with its sequence allocated, before a tool can run or a
/// background completion can append (#945) — the guarantee in-flight upserts
/// used to provide. Dispatch and recovery read arguments through the block.
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
/// ordinary authored streams (markers, normalized separators, retrieval hints).
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
    /// The seal is not visible or not authorized. While replication is behind
    /// this is expected; the message is incomplete, not empty.
    UnresolvedSeal { seal_doc_id: String },
    /// A message references a retracted source or a stream the seal lacks.
    InvalidReference { reference: PayloadRef },
    /// Fewer segments are visible than the seal's extent.
    Incomplete {
        reference: PayloadRef,
        visible_segments: u32,
    },
    /// An ordinal inside the sealed extent is absent.
    Gap { reference: PayloadRef, ordinal: u32 },
    /// Visible twins at a coordinate disagree in payload or declaration.
    ConflictingSegments { reference: PayloadRef, ordinal: u32 },
    /// The assembled stream disagrees with the sealed byte length, or its
    /// declaration disagrees with the block that references it.
    ExtentMismatch { reference: PayloadRef, bytes: u64 },
    /// More than one seal is visible for a source.
    ConflictingSeals {
        request_doc_id: String,
        source: OutputSource,
    },
    ConflictingMessages {
        session_id: String,
        message_key: String,
    },
    /// Illegal role/block combination or non-native block order.
    InvalidStructure { detail: String },
    /// A presentation range is out of bounds or splits a UTF-8 sequence.
    InvalidPresentation { reference: PayloadRef },
    /// Sealed bytes do not decode as their declared payload (argument JSON,
    /// base64 media).
    InvalidPayload { reference: PayloadRef },
}

/// Output for a request that no message references yet, grouped by declared
/// native position: sources with no visible seal, and closed sources awaiting
/// publication (tool output before delivery, interrupted output before
/// recovery publishes it). Retracted sources are excluded. This is the live
/// preview and streaming view, built from the same segments the transcript
/// will reference, so there is no rollover, overlap, or repair step.
///
/// Headers, seals and segments may replicate in any order and are projected
/// together: a header whose segments have not arrived is incomplete, never a
/// second live copy.
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
    /// `None` while no seal is visible.
    pub sealed: Option<OutputOutcome>,
    /// Contiguous payload from ordinal zero; stops at the first gap.
    pub text: String,
    pub next_ordinal: u32,
}
