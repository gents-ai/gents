//! Canonical durable output (#1571): payloads are persisted once; small runtime
//! presentation literals live in headers. Neither document shape is updated.
//!
//! Two immutable, create-only document shapes:
//!
//! - [`OutputSegment`] — a flush, a terminal closure, or both. Large payloads
//!   live here; normal closure rides on the final flush without another document.
//! - [`TranscriptMessage`] — a header mirroring the native
//!   [`crate::message::Message`] structure, with every payload string replaced
//!   by a [`PayloadRef`] to a closed stream, plus small presentation literals.
//!
//! The collections only grow, so replication is set union and every reader
//! question is "which facts are visible": no visible closing record means closure is
//! unknown, not proof that a producer is still running. A closing record with missing
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

/// Shared closed-source reconstruction; header and live-view owners build on it.
pub mod reconstruction;

/// What produced a run of content. Its identity is known before the first
/// byte arrives, so segments never wait on the closing record or message that later
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
/// the run that opens the stream, so live views render unclosed output in
/// native structure without any control document.
///
/// It names no role: provider output is assistant content and tool output is
/// a tool result. Authored content is committed with its header in one
/// transaction, but a replica may receive the parts in any order, so the live
/// projection never shows an Authored source until its header is visible.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamDeclaration {
    /// Original native position, not a recovered header's compacted index.
    /// Recovery may omit unpublishable blocks but preserves survivor order.
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

/// One record of a source. Stored as `AgentOutputSegment`.
///
/// A record carries a flush of bytes, the source's closure, or both:
///
/// - **Flush** (`ordinal` and `runs` present): the bytes that arrived since the
///   last flush, however many streams advanced. `payload` is the concatenation
///   of `runs` in arrival order, and a stream's content is the concatenation
///   of its runs across the source's flushes in `(ordinal, run)` order — the
///   native string exactly. Writers flush where the batch interval, a size
///   threshold, or the source's end falls, never per token.
/// - **Final flush** (also `close`): the last bytes and the closure in one
///   document. This is the normal end of a source and costs nothing extra.
/// - **Terminal-only** (`close` without `ordinal` or `runs`): closure written
///   when there are no new bytes — everything was already flushed, the source
///   produced nothing, the attempt is retracted, or recovery closes it.
///
/// Flushes and closure have separate coordinates so they can never collide.
/// A flush is `(request_doc_id, source, ordinal)`, dense from zero. Closure is
/// `(request_doc_id, source)`, and a terminal-only record occupies no ordinal.
/// So when recovery closes a source while the superseded writer's last flush
/// is still in flight, that flush lands at an ordinal beyond the closed extent
/// and is inert; it is not a twin of the closing record and never becomes a
/// reconstruction conflict.
///
/// Repeated delivery of the same coordinate and content is idempotent. Two
/// visible flushes sharing an ordinal but differing in writer, runs or
/// payload, or two visible closures of one source, are an integrity conflict
/// every reader must surface; the storage index is ordinary, not unique,
/// because a unique index can hide the losing twin of a remote conflict
/// (#1073). A replay reuses the persisted document, including its creation
/// timestamp; an already-existing create is accepted only after verifying the
/// stored fact.
///
/// A flush records output, not lease liveness. It never renews the request lease.
/// A request-owned flush does not rewrite the request. It commits inside the
/// existing `config_client::txn::MutationWriteGate`. The owner stamps
/// `created_at` inside that gate when admitting the write; only a committed
/// fact counts (non-decreasing within a source; replay reuses the timestamp).
/// Only that runtime, reading its own store inside the same gate, may decide a
/// lease expired. Its deadline is exactly `execution_lease_expires_at`, maintained
/// by bounded explicit owner renewal, independently of output and foreground tool
/// reads. A lagging replica observes; it never expires work.
/// Admission rereads generation, lifecycle and effective expiry under the gate.
/// An expired generation cannot revive itself by timestamping a fresh flush;
/// it is rejected even if recovery has not installed a replacement yet.
/// The same effective-expiry check applies to producer closure/retraction,
/// publication, dispatch, terminalization and explicit renewal before their CAS. Matching
/// generation alone is insufficient. Recovery instead uses the existing recovery
/// authority and may close the expired producer's committed extent.
///
/// Only a plain flush is unfenced. One that loses a race with recovery's
/// generation swap is inert: it names a superseded writer, renews nothing and
/// lies beyond the extent recovery closed. Everything that *decides* — any
/// record carrying `close`, accepting and publishing a turn, dispatch,
/// terminalization, recovery — still commits under the matching-generation
/// CAS on the request, so it is definitively ordered against cancellation and
/// recovery and a loser writes none of them. Tool-owned output follows the
/// tool lifecycle and never revives a terminal request. There is no per-flush heartbeat
/// or progress counter; due-only explicit renewal covers both streaming and
/// silent work. Publication and dispatch do not implicitly extend the deadline.
/// Tool-owned closure uses the tool lifecycle's terminal/delivery guard, not a
/// request CAS that could revive the already-terminal originating request.
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
    /// The source's producer, the same on every record of the source. A
    /// terminal-only record written by recovery still names the producer; the
    /// CAS it committed under is its authority, not a replacement identity.
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
    /// `close`, or both; never neither. With no ordinal, runs and payload must
    /// be empty and close must be present. No decoder may infer missing closure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub close: Option<SourceClose>,
    pub created_at: String,
}

/// The next `bytes` of `payload` belong to `stream`. Runs split only at UTF-8
/// boundaries. The run that opens a stream carries its declaration; an empty
/// native string is an opening run of zero bytes.
/// A zero-byte continuation is not a flush: it carries no new output fact and
/// cannot be used as a streaming heartbeat. Silent work uses explicit renewal.
/// Runs cover the payload exactly; streams open densely from zero and declare
/// exactly once. Missing earlier flushes never permit inventing a declaration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SegmentRun {
    pub stream: u32,
    pub bytes: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declaration: Option<StreamDeclaration>,
}

/// The existing authority that produced a source; never a new lease or host
/// identity. Every record names it, which lets the shared projection classify
/// unclosed output and ignore a superseded generation. It never renews a lease.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OutputWriter {
    /// A request execution generation. A superseded generation can still land
    /// an inert flush, but cannot close, publish, dispatch or terminalize.
    RequestExecution { execution_generation: String },
    /// The tool lifecycle admits output until terminalization. Its delivery
    /// owner also admits an authored background-start receipt while running
    /// and the separate completion notification afterward. The receipt does
    /// not close the continuing tool output source.
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

/// The one terminal fact about a source, carried by its closing record.
///
/// At most one per `(request_doc_id, source)`; visible twins are an integrity
/// conflict. It stays valid after its producer's generation is no longer
/// active. Closing a request-owned source commits under the
/// matching-generation CAS on the request, so the producer and recovery can
/// never both close it. Recovery closes exactly the flushes already committed,
/// with a terminal-only record; it never appends bytes for a stale writer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SourceClose {
    /// The exact extent: `segments` flushes (ordinals `0..segments`, including
    /// this record's own flush if it has one) and `stream_bytes[n]` bytes in
    /// stream `n`. A reader holding fewer flushes, a gap in ordinals, or a
    /// byte mismatch has an incomplete or conflicting replica and must not
    /// present the content as complete. A final flush has ordinal segments - 1.
    /// A terminal-only record contributes no flush to the count. Every stream
    /// has an opening run, including empty strings; a source with no streams
    /// has segments = 0 and no stream_bytes. Beyond-extent flushes are inert:
    /// they are excluded before data-twin checks, but never hide another close.
    /// Fresh Complete acceptance under the owner gate must include all committed
    /// data flushes, with nondecreasing timestamps no later than closure. This
    /// writer check is distinct from replica reconstruction and exact replay.
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

/// One stream of a closed source, by the exact identity of the record that
/// closed it. That record is immutable, so its identity pins the extent it
/// states; there is no logical key or repeated extent to disagree with it. A
/// forked message carries the same references as its origin — payloads are
/// never copied.
///
/// Closing records, the flushes within their extents, and the request/tool
/// provenance they name are retained across session close or removal; this
/// stack introduces no payload GC. ACP applies to every dependency: a
/// reference never broadens access.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PayloadRef {
    pub close_doc_id: String,
    pub stream: u32,
}

/// Argument-only admission input for a remotely delegated tool call. Stored
/// immutably on the existing addressed AgentToolCall, never on local calls.
/// The accepting coordinator verifies exact bytes against `source` and commits
/// this value with the accepted header and pending call row. The host trusts
/// the existing authenticated coordinator/ACP route; it does not fetch parent
/// output to verify this projection. `source` is provenance, not read authority.
/// This one-time copy preserves document-level disclosure boundaries when a
/// source segment also contains other calls, text, or private reasoning.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DelegatedToolInput {
    pub source: PayloadRef,
    /// Exact emitted JSON argument text; no normalization or reconstructed JSON.
    pub arguments: String,
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
        /// Recovery publishes only retained text from an unheaded provider source;
        /// see TranscriptMessage. This is not permission to invent native metadata.
        execution_generation: String,
    },
    /// Tool-owned invocation reply or completion notification. A background
    /// invocation can reply with a native tool-result receipt while execution
    /// remains running, then publish an ordinary-text notification on terminal
    /// completion. Each publication retains its own immutable replay identity.
    /// The existing notification/queue owner binds ordinary completion messages
    /// to the coalesced wake request (or the parent for Goal-owned input-only
    /// delivery). Payload references still name the originating tool's source;
    /// publication request membership is not payload-source ownership.
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
/// Create-only: written once, after every closing record it references, and never
/// updated. There is no in-flight message row; unclosed output is visible as
/// segments. `sequence` is the order coordinate within the session and keeps
/// its existing durable allocator. Two visible messages sharing `message_key`
/// or `(session_id, sequence)` with different content are an integrity
/// conflict, surfaced the same way as segment twins.
///
/// A successfully accepted provider turn atomically publishes its Complete
/// closing record, assistant header and pending `AgentToolCall` rows before any tool is
/// dispatched. The assistant turn is durable, with its sequence allocated,
/// before a tool can run or a
/// background completion can append (#945) — the guarantee in-flight upserts
/// used to provide. Local dispatch and recovery read arguments through the block;
/// a remote host consumes the addressed call's immutable [`DelegatedToolInput`].
/// Partial turns may publish diagnostic headers, but any
/// retained tool-call blocks name terminal, nondispatchable lifecycle rows.
/// Pending intent is not execution permission: the existing tool owner checks
/// current request ownership, cancellation/deadline and policy at dispatch.
/// Once published, an accepted turn cannot be retracted/resampled because a
/// later dispatch fails. Recovery resumes its existing rows, never new calls.
///
/// `blocks` is in native content order, so reconstruction yields the exact
/// native `Message` (including `Message::Assistant.id` via `native_id`).
///
/// Recovery of an unheaded provider source is deliberately narrower: close its
/// committed extent Partial (or reuse its existing Partial closure), then publish
/// only ordinary Text streams at native part zero as Full
/// PresentedPayload references, in original block order, with outcome Partial
/// and native_id None. Do not turn argument/reasoning/media bytes into text or
/// fabricate their missing metadata. Omitted blocks leave gaps in original
/// declaration positions; validate survivor order, not compacted index equality.
/// If no eligible text stream exists, publish no assistant header for that source. Retain
/// every omitted stream as diagnostic data, never executable tool intent or
/// provider input. An existing published header is resolved unchanged, not
/// replaced by this recovery policy. Provider-input narrowing still applies.
/// Validate source extent/run accounting for all streams, but decode native
/// payloads only for referenced blocks: omitted incomplete JSON must not invalidate
/// an otherwise reconstructable text-only recovery header.
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
    /// Exact request this message belongs to, not necessarily the request that
    /// produced its referenced bytes. A background completion notification is
    /// bound by the existing queue owner to its wake request; Goal-owned
    /// input-only delivery stays parent-bound. Absent only for history a fork
    /// placed in a child session, which must not acquire live request
    /// membership. The logical `request_id` is not repeated here (#1425).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_doc_id: Option<String>,
    pub publication: MessagePublication,
    /// Whether this message was published whole or as kept partial output.
    /// Set by the publisher; never derived from the closing records its blocks reference
    /// (a complete notification may wrap a tool's partial output), so a message
    /// with no payloads needs no closing record. Forks retain it.
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
    /// The closing record is unavailable, with no known authorization denial.
    /// This may be replication lag; absence alone proves neither lag nor denial.
    UnresolvedClose { close_doc_id: String },
    /// The authorization owner explicitly denied a required dependency. Applies
    /// to headers, closing records, segments and provenance, not just PayloadRef.
    /// This is an access error, never a loading state. Do not infer it from absence.
    AccessDenied { doc_id: String },
    /// A reference names a plain flush, retracted source or absent stream.
    InvalidReference { reference: PayloadRef },
    /// An ordinal inside the sealed extent is not visible. Expected while
    /// replication is behind; the source is incomplete, not shorter.
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

/// Output for a request that no message references yet, grouped by declared
/// native position: sources with no visible closing record, and closed sources awaiting
/// publication (tool output before delivery, interrupted output before
/// recovery publishes it), and retained partial diagnostics. Retracted sources
/// are excluded. This is the live
/// preview and streaming view, built from the same segments the transcript
/// will reference, so there is no rollover, overlap, or repair step.
///
/// Headers, closing records and segments may replicate in any order and are projected
/// together: a header whose segments have not arrived is incomplete, never a
/// second live copy.
/// Unclosed request-owned output is eligible only for the current generation
/// of a nonterminal request; tool-owned output follows the tool lifecycle.
/// Each segment names its writer; a missing request/tool owner observation
/// cannot establish live eligibility. Authored sources are never shown live:
/// they appear only with their header. This projection is an observation for
/// display. It never decides that work expired; only the owning runtime does.
/// Superseded sources remain retained history, not current output. Even an
/// eligible unclosed source is only an observation: its closing record may be in transit.
/// Closed historical output never depends on today's active generation.
/// After terminal selection is visible, unreferenced streams from a closed
/// Partial provider source are RetainedPartial, not pending publication. Resolve
/// a selected header before classifying its streams; missing dependencies stay
/// unresolved. Opaque reasoning is never rendered, including in diagnostics.
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
    /// are not yet reconstructable. Keep a validated contiguous preview while
    /// reporting loading; never label a known-closed source as live activity.
    PendingPublication {
        outcome: OutputOutcome,
    },
    /// These closed Partial provider bytes remain outside published native messages
    /// after request terminalization, including fragments omitted by recovery.
    /// Diagnostic-only: not current activity, pending delivery or provider input.
    RetainedPartial,
}
