//! Canonical durable output (#1571): every byte of model, tool, and authored
//! content is persisted once, and nothing in this model is ever updated.
//!
//! Three immutable, create-only facts:
//!
//! - [`OutputSegment`] — one flush of a source: the bytes that arrived since
//!   the last flush. The only place content lives, and the only progress fact.
//! - [`OutputSeal`] — the one terminal fact about a source: closed with its
//!   exact extent, or retracted.
//! - [`TranscriptMessage`] — a header mirroring the native
//!   [`crate::message::Message`] structure, with every payload string replaced
//!   by a [`PayloadRef`] to a sealed stream.
//!
//! The collections only grow, so replication is set union and every reader
//! question is "which facts are visible": no visible seal means closure is
//! unknown, not proof that a producer is still running. A seal with missing
//! segments means the replica is incomplete. Live previews, answer streaming,
//! completed transcripts, provider input, forks and
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
    /// the transcript, request context and output-obligation reminders. `key`
    /// is the existing request-scoped idempotency key of that content. Small
    /// runtime-written text around other output (truncation markers,
    /// notification wrappers) is an inline presentation literal, not a source.
    Authored { key: String },
}

/// What a stream carries and where it sits in the native message. Carried by
/// the run that opens the stream, so live views render unsealed output in
/// native structure without any control document.
///
/// It names no role: provider output is assistant content, tool output is a
/// tool result, and authored content arrives whole and is published with its
/// seal and header in one transaction, so it is never live without its header.
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

/// One flush of a source. Stored as `AgentOutputSegment`.
///
/// Coordinate: `(request_doc_id, source, ordinal)`, with `ordinal` dense from
/// zero within the source. A flush is one document however many streams
/// advanced: `payload` is the concatenation of `runs` in arrival order, and a
/// stream's content is the concatenation of its runs across the source's
/// segments in `(ordinal, run)` order — the native string exactly. Writers
/// flush where the batch interval, a size threshold, or the source's end
/// falls, never per token; a turn that fits in one interval is one segment.
///
/// Repeated delivery of the same coordinate and content is idempotent. Two
/// visible segments sharing a coordinate but differing in writer, runs or
/// payload are an integrity conflict every reader must surface; the storage
/// index is ordinary, not unique, because a unique index can hide the losing
/// twin of a remote conflict (#1073). A replay reuses the persisted document,
/// including its creation timestamp; an already-existing create is accepted
/// only after verifying the stored fact.
///
/// **A segment is the progress fact; progress is not recorded anywhere else.**
/// Request-owned output does not rewrite the request: the execution lease is
/// live until `created_at + execution_lease_secs` of the newest segment, seal
/// or header written by the request's current generation (or the claim's own
/// deadline if later). The write validates, in its transaction, that its
/// generation is current and unexpired, but fencing does not depend on that
/// read: a stale generation's segment names a stale writer, never counts as
/// progress, and lies outside any extent recovery already sealed, so it is
/// inert. Tool-owned output follows the tool lifecycle and never revives a
/// terminal request. There is no heartbeat and no progress counter.
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
    pub ordinal: u32,
    pub writer: OutputWriter,
    pub runs: Vec<SegmentRun>,
    pub payload: String,
    pub created_at: String,
}

/// The next `bytes` of `payload` belong to `stream`. Runs split only at UTF-8
/// boundaries. The run that opens a stream carries its declaration; an empty
/// native string is an opening run of zero bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SegmentRun {
    pub stream: u32,
    pub bytes: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declaration: Option<StreamDeclaration>,
}

/// The existing authority that produced a source; never a new lease or host
/// identity. Every segment names its writer, which is what lets a reader or
/// the lease owner classify unsealed output and ignore a stale generation.
/// The seal names the source's producer, not today's authority to recover it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OutputWriter {
    /// The request lifecycle CAS. Stale generations cannot append or seal.
    RequestExecution { execution_generation: String },
    /// The tool lifecycle admits output until terminalization, and its
    /// delivery owner admits authored completion notifications afterward.
    ToolExecution { tool_call_doc_id: String },
}

/// Whether content ends where its producer meant it to. This is completeness
/// only: partial output is kept and labelled, never presented as a complete
/// source with silently shorter text. Why it was cut short — interruption,
/// provider failure, deadline — and whether the producing operation succeeded
/// are owned by the request and tool lifecycles and are not classified again
/// here. A tool that fails with its full output has complete output.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputOutcome {
    Complete,
    Partial,
}

/// The one terminal fact about a source. Stored as `AgentOutputSeal`.
///
/// Normal closure commits with the final flush. Retraction, recovery or
/// closure after a flush seals the committed bytes without appending.
/// At most one seal per `(request_doc_id, source)`; visible twins are
/// an integrity conflict. A sealed source accepts no further segments, and a
/// seal stays valid after its producer's generation is no longer active.
/// Recovery seals only the bytes already committed, under the winning request
/// terminalization CAS; it never appends on behalf of a stale writer.
/// Its `writer` remains the producer its segments name; the recovery CAS
/// supplies authorization, not a replacement producer identity.
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
    /// The exact extent: `segments` flushes, and `stream_bytes[n]` bytes in
    /// stream `n`. A reader holding fewer segments, a gap in ordinals, or a
    /// byte mismatch has an incomplete or conflicting replica and must not
    /// present the content as complete. Segments beyond the extent are inert.
    Closed {
        outcome: OutputOutcome,
        segments: u32,
        stream_bytes: Vec<u64>,
    },
    /// The attempt was abandoned before retry backoff began. Its segments are
    /// retained but never referenced by a message or shown live, even when the
    /// replacement attempt has not yet emitted a byte.
    Retracted,
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

/// Runtime-owned `AgentRequest.terminal_output`, selected by the existing
/// terminalization owner in the same transaction as terminal lifecycle state
/// and any final header publication. It is absent before terminalization and
/// never changed afterward, including by late background delivery.
///
/// A terminal row missing this field is incomplete/invalid, not `NoMessage`.
/// Readers resolve the exact assistant header and all its dependencies in the
/// physical request's agent/session/requester scope. Missing data never falls
/// back to the latest locally visible message. Lifecycle/failure_reason still
/// determine request success or failure; message presence alone does not.
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

/// The single durable transcript message. Stored as `AgentMessage`.
///
/// Create-only: written once, after every seal it references, and never
/// updated. There is no in-flight message row; unsealed output is visible as
/// segments. `sequence` is the order coordinate within the session and keeps
/// its existing durable allocator. Two visible messages sharing `message_key`
/// or `(session_id, sequence)` with different content are an integrity
/// conflict, surfaced the same way as segment twins.
///
/// A successfully accepted provider turn atomically publishes its Complete
/// seal, assistant header and pending `AgentToolCall` rows before any tool is
/// dispatched. The assistant turn is durable, with its sequence allocated,
/// before a tool can run or a
/// background completion can append (#945) — the guarantee in-flight upserts
/// used to provide. Dispatch and recovery read arguments through the block.
/// Partial turns may publish diagnostic headers, but any
/// retained tool-call blocks name terminal, nondispatchable lifecycle rows.
/// Pending intent is not execution permission: the existing tool owner checks
/// current request ownership, cancellation/deadline and policy at dispatch.
/// Once published, an accepted turn cannot be retracted/resampled because a
/// later dispatch fails. Recovery resumes its existing rows, never new calls.
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
    /// Whether this message was published whole or as kept partial output.
    /// Set by the publisher; never derived from the seals its blocks reference
    /// (a complete notification may wrap a tool's partial output), so a message
    /// with no payloads needs no seal. Forks retain it.
    pub outcome: OutputOutcome,
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
/// Ranges index the full output at UTF-8 boundaries. Literals are the small
/// runtime-written pieces between them (markers, normalized separators,
/// retrieval hints, notification wrappers), stored once, inline, here.
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
    /// An ordinal inside the sealed extent is not visible. Expected while
    /// replication is behind; the source is incomplete, not shorter.
    MissingSegment { seal_doc_id: String, ordinal: u32 },
    /// Visible twins at a coordinate disagree in writer, runs or payload.
    ConflictingSegments { seal_doc_id: String, ordinal: u32 },
    /// A segment's runs do not account for its payload, the assembled stream
    /// disagrees with the sealed byte length, or its declaration disagrees
    /// with the block that references it.
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
    /// Segments within the sealed extent, or the seal, disagree about the
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
/// Unsealed request-owned output is eligible only for the current generation
/// of a nonterminal request; tool-owned output follows the tool lifecycle.
/// Each segment names its writer; a missing request/tool owner observation
/// cannot establish live eligibility.
/// Superseded sources remain retained history, not current output. Even an
/// eligible unsealed source is only an observation: its seal may be in transit.
/// Closed historical output never depends on today's active generation.
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
    Unsealed,
    PendingPublication {
        outcome: OutputOutcome,
    },
}
