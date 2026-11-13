use gents_protocol::message::{
    AssistantContent, Message as NativeMessage, Reasoning, ReasoningContent, Text,
    ToolResultContent, UserContent,
};
use gents_protocol::output::live::{
    project_live, reconstruct_dense_prefix, DensePrefix, DensePrefixError, LiveObservation,
    LiveTarget, LiveView, OwnerLiveness,
};
use gents_protocol::output::reconstruction::{
    DependencyDenial as OutputDependencyDenial, ObservedSegment, ReconstructedStream,
};
use gents_protocol::output::{
    MediaBlock, MediaData, MediaKind as OutputMediaKind, MessageBlock, MessagePublication,
    MessageRole, OutputOutcome, OutputSegment, OutputSource, OutputWriter, PayloadPresentation,
    PayloadRef as OutputPayloadRef, PresentationPart as OutputPresentationPart, PresentedPayload,
    ReasoningPart as OutputReasoningPart, SegmentRun, SourceClose, StreamDeclaration,
    StreamPayload, TerminalOutput, ToolResultPart, TranscriptMessage,
};
use gents_protocol::rendered_request::{CaptureScope, CaptureScopeKind};
use gents_protocol::request_admission::RequestPurpose;
use gents_protocol::request_lifecycle::RequestLifecycleState;
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolAdmission {
    pub document: u64,
    pub call_id: u64,
    pub request_id: u64,
    pub state: String,
    pub operation: String,
    pub deadline: u64,
    pub started_at: Option<u64>,
    pub current_time: u64,
    pub failure_class: Option<String>,
    pub persistence: String,
    pub await_mode: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpawnedToolAdmission {
    pub document: u64,
    pub parent_tool_document: u64,
    pub call_id: u64,
    pub request_id: u64,
    pub state: String,
    pub operation: String,
    pub deadline: u64,
    pub started_at: Option<u64>,
    pub current_time: u64,
    pub failure_class: Option<String>,
    pub persistence: String,
    pub await_mode: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryItem {
    pub closing: Segment,
    pub message: Option<Message<PayloadSpec>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TitleBinding {
    pub purpose: RequestPurpose,
    pub physical_request: u64,
    pub logical_request: u64,
    pub parent_physical: u64,
    pub parent_logical: u64,
    pub node: u64,
    pub session: u64,
    pub authenticated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseStatus {
    Vacant,
    Active,
    Recoverable,
    Terminal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionOutcome {
    Completed,
    Failed,
    Interrupted,
    Dead,
    Superseded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lease {
    pub status: LeaseStatus,
    pub generation: Option<u64>,
    pub duration: Option<u64>,
    pub explicit_deadline: Option<u64>,
    pub outcome: Option<ExecutionOutcome>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeaseWorld {
    pub request: RequestLifecycleState,
    pub lease: Lease,
    pub used_generations: Vec<u64>,
    pub now: u64,
    pub effective_expiry: u64,
    pub continuation_required: bool,
    pub token_charge_required: bool,
    pub continuation_count: u64,
    pub token_charge_count: u64,
}

/// The modeled request a native execution script starts from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Seed {
    pub purpose: RequestPurpose,
    pub title_binding: Option<TitleBinding>,
    pub request_id: u64,
    pub session_id: u64,
    pub node_did: u64,
    pub lease: LeaseWorld,
    pub transcript_session_id: u64,
    pub next_sequence: u64,
    pub segments: Vec<Segment>,
    pub messages: Vec<Message<PayloadSpec>>,
    pub tool_calls: Vec<u64>,
    pub in_flight: Vec<u64>,
}

/// One modeled execution operation, tagged by its `operation` name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    AcceptForeground {
        actor: u64,
        now: u64,
        generation: u64,
        closing: Segment,
        message: Message<PayloadSpec>,
        admissions: Vec<ToolAdmission>,
    },
    AcceptBackground {
        actor: u64,
        now: u64,
        generation: u64,
        closing: Segment,
        message: Message<PayloadSpec>,
        admissions: Vec<ToolAdmission>,
    },
    Dispatch {
        actor: u64,
        now: u64,
        generation: u64,
        call: u64,
        cancellation_allows: bool,
        tool_policy_allows: bool,
    },
    CloseForegroundTool {
        actor: u64,
        now: u64,
        document: u64,
        authority_outcome: String,
        record: Segment,
    },
    CompleteForegroundTool {
        actor: u64,
        now: u64,
        document: u64,
        authority_outcome: String,
        record: Segment,
        message: Message<PayloadSpec>,
    },
    DeliverForegroundResult {
        actor: u64,
        now: u64,
        document: u64,
        message: Message<PayloadSpec>,
    },
    TerminalizeCompleted {
        actor: u64,
        now: u64,
        generation: u64,
        outcome: String,
        selection: TerminalSelection,
    },
    RecoverExpiredGeneration {
        actor: u64,
        now: u64,
        expected_generation: u64,
        fresh_generation: u64,
        duration: u64,
        deadline: u64,
        items: Vec<RecoveryItem>,
    },
    RecoverExpiredTerminal {
        actor: u64,
        now: u64,
        expected_generation: u64,
        fresh_generation: u64,
        outcome: String,
        selection: TerminalSelection,
        items: Vec<RecoveryItem>,
    },
    ClosePartial {
        actor: u64,
        now: u64,
        generation: u64,
        item: RecoveryItem,
    },
    CloseAuxiliary {
        actor: u64,
        now: u64,
        generation: u64,
        closing: Segment,
    },
    RetractBeforeRetry {
        actor: u64,
        now: u64,
        generation: u64,
        record: Segment,
    },
    RenewLease {
        actor: u64,
        now: u64,
        generation: u64,
        expected_deadline: u64,
    },
    AppendOutput {
        actor: u64,
        now: u64,
        generation: u64,
        record: Segment,
    },
    AppendToolOutput {
        actor: u64,
        now: u64,
        document: u64,
        record: Segment,
    },
    AppendOutputWhileSiblingWaits {
        actor: u64,
        now: u64,
        generation: u64,
        record: Segment,
    },
    AcceptTurn {
        actor: u64,
        now: u64,
        generation: u64,
        closing: Segment,
        message: Message<PayloadSpec>,
        admissions: Vec<ToolAdmission>,
    },
    BackgroundTool {
        actor: u64,
        now: u64,
        generation: u64,
        document: u64,
        action: String,
    },
    PublishBackgroundReceipt {
        actor: u64,
        now: u64,
        parent_document: u64,
        closing: Segment,
        message: Message<PayloadSpec>,
    },
    AdmitSpawnedBackground {
        actor: u64,
        now: u64,
        generation: u64,
        admission: SpawnedToolAdmission,
    },
    PublishAuthored {
        actor: u64,
        now: u64,
        generation: u64,
        closing: Segment,
        message: Message<PayloadSpec>,
    },
    AdvanceCompactionCursor {
        actor: u64,
        now: u64,
        cursor: u64,
    },
    RevokeCorrupt {
        actor: u64,
        now: u64,
        expected_generation: u64,
        fresh_generation: u64,
        outcome: String,
        selection: TerminalSelection,
    },
    DeliverReplicatedSegment {
        actor: u64,
        now: u64,
        record: Segment,
    },
}

impl Operation {
    pub fn actor_and_now(&self) -> (u64, u64) {
        match self {
            Self::AcceptForeground { actor, now, .. }
            | Self::AcceptBackground { actor, now, .. }
            | Self::Dispatch { actor, now, .. }
            | Self::CloseForegroundTool { actor, now, .. }
            | Self::CompleteForegroundTool { actor, now, .. }
            | Self::DeliverForegroundResult { actor, now, .. }
            | Self::TerminalizeCompleted { actor, now, .. }
            | Self::RecoverExpiredGeneration { actor, now, .. }
            | Self::RecoverExpiredTerminal { actor, now, .. }
            | Self::ClosePartial { actor, now, .. }
            | Self::CloseAuxiliary { actor, now, .. }
            | Self::RetractBeforeRetry { actor, now, .. }
            | Self::RenewLease { actor, now, .. }
            | Self::AppendOutput { actor, now, .. }
            | Self::AppendToolOutput { actor, now, .. }
            | Self::AppendOutputWhileSiblingWaits { actor, now, .. }
            | Self::AcceptTurn { actor, now, .. }
            | Self::BackgroundTool { actor, now, .. }
            | Self::PublishBackgroundReceipt { actor, now, .. }
            | Self::AdmitSpawnedBackground { actor, now, .. }
            | Self::PublishAuthored { actor, now, .. }
            | Self::AdvanceCompactionCursor { actor, now, .. }
            | Self::RevokeCorrupt { actor, now, .. }
            | Self::DeliverReplicatedSegment { actor, now, .. } => (*actor, *now),
        }
    }
}

/// What a native execution step leaves durable. Segments and messages are a
/// set; `ExecutionObservation::wire` orders them as the Lean family does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionObservation {
    pub accepted: bool,
    pub generation: Option<u64>,
    pub terminal_generation: Option<u64>,
    pub request_state: String,
    pub terminal_selection: Option<TerminalSelection>,
    pub tool_state: Option<String>,
    pub tool_stuck_since: Option<u64>,
    pub in_flight: bool,
    pub next_sequence: u64,
    pub accepted_sequence: Option<u64>,
    pub physical_tool_request: Option<u64>,
    pub lease_deadline: Option<u64>,
    pub compaction_cursor: Option<u64>,
    pub segments: Vec<Segment>,
    pub messages: Vec<Message<PayloadSpec>>,
}

impl ExecutionObservation {
    /// The observation as a JSON value, its segments ordered by identifier
    /// then by `compact_model_json`, and its messages by header identifier
    /// then by `compact_model_json`: the order
    /// `Conformance.NativeExecutionContracts.observationWire` writes.
    pub fn wire(&self) -> serde_json::Value {
        let mut segments = self
            .segments
            .iter()
            .map(|segment| {
                let value = encode(segment);
                ((segment.id, crate::compact_model_json(&value)), value)
            })
            .collect::<Vec<_>>();
        segments.sort_by(|left, right| left.0.cmp(&right.0));
        let mut messages = self
            .messages
            .iter()
            .map(|message| {
                let value = encode(message);
                (
                    (message.header.id, crate::compact_model_json(&value)),
                    value,
                )
            })
            .collect::<Vec<_>>();
        messages.sort_by(|left, right| left.0.cmp(&right.0));
        let mut value = encode(self);
        value["segments"] =
            serde_json::Value::Array(segments.into_iter().map(|(_, value)| value).collect());
        value["messages"] =
            serde_json::Value::Array(messages.into_iter().map(|(_, value)| value).collect());
        value
    }
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

pub fn live_stream_wire(stream: &gents_protocol::output::LiveStream) -> Stream {
    Stream {
        declaration: declaration_wire(&stream.declaration),
        bytes: stream.text.as_bytes().to_vec(),
    }
}

fn utf8(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).expect("modeled native payload is UTF-8")
}

fn native_reasoning(id: &Option<String>, parts: &[ReasoningPart<Vec<u8>>]) -> Reasoning {
    Reasoning {
        id: id.clone(),
        content: parts
            .iter()
            .map(|part| match part {
                ReasoningPart::Text { payload, signature } => ReasoningContent::Text {
                    text: utf8(payload),
                    signature: signature.clone(),
                },
                ReasoningPart::Encrypted { payload } => ReasoningContent::Encrypted(utf8(payload)),
                ReasoningPart::Redacted { payload } => ReasoningContent::Redacted {
                    data: utf8(payload),
                },
                ReasoningPart::Summary { payload } => ReasoningContent::Summary(utf8(payload)),
            })
            .collect(),
    }
}

/// The native assistant message a modeled reconstruction stands for. Only
/// text and reasoning blocks have a native assistant form without the
/// transcript header that carries tool identities.
pub fn native_assistant(native: &Reconstructed) -> NativeMessage {
    assert_eq!(
        native.role,
        Role::Assistant,
        "modeled message is assistant-authored"
    );
    NativeMessage::Assistant {
        id: native.native_id.clone(),
        request_id: native.native_request_id.clone(),
        content: native
            .blocks
            .iter()
            .map(|block| match block {
                Block::Text { payload } => AssistantContent::Text(Text {
                    text: utf8(payload),
                }),
                Block::Reasoning { id, parts } => {
                    AssistantContent::Reasoning(native_reasoning(id, parts))
                }
                other => panic!("modeled assistant block {other:?} has no header-free native form"),
            })
            .collect(),
    }
}

fn doc_id(id: &str) -> u64 {
    id.strip_prefix("nat-")
        .and_then(|id| id.parse().ok())
        .unwrap_or_else(|| panic!("translated document id {id} is not a modeled nat"))
}

fn reasoning_wire(reasoning: &Reasoning) -> Block<Vec<u8>> {
    Block::Reasoning {
        id: reasoning.id.clone(),
        parts: reasoning
            .content
            .iter()
            .map(|part| match part {
                ReasoningContent::Text { text, signature } => ReasoningPart::Text {
                    payload: text.as_bytes().to_vec(),
                    signature: signature.clone(),
                },
                ReasoningContent::Encrypted(data) => ReasoningPart::Encrypted {
                    payload: data.as_bytes().to_vec(),
                },
                ReasoningContent::Redacted { data } => ReasoningPart::Redacted {
                    payload: data.as_bytes().to_vec(),
                },
                ReasoningContent::Summary(text) => ReasoningPart::Summary {
                    payload: text.as_bytes().to_vec(),
                },
            })
            .collect(),
    }
}

/// The modeled reconstruction of a native message production rebuilt from
/// `header`; tool documents come from the header's blocks at the same position.
pub fn reconstructed_wire(native: &NativeMessage, header: &TranscriptMessage) -> Reconstructed {
    let tool_doc = |index: usize| match header.blocks.get(index) {
        Some(MessageBlock::ToolCall {
            tool_call_doc_id, ..
        })
        | Some(MessageBlock::ToolResult {
            tool_call_doc_id, ..
        }) => doc_id(tool_call_doc_id),
        other => panic!("native tool block {index} has header block {other:?}"),
    };
    match native {
        NativeMessage::Assistant {
            id,
            request_id,
            content,
        } => Reconstructed {
            role: Role::Assistant,
            native_id: id.clone(),
            native_request_id: request_id.clone(),
            blocks: content
                .iter()
                .enumerate()
                .map(|(index, block)| match block {
                    AssistantContent::Text(text) => Block::Text {
                        payload: text.text.as_bytes().to_vec(),
                    },
                    AssistantContent::Reasoning(reasoning) => reasoning_wire(reasoning),
                    AssistantContent::ToolCall(call) => Block::ToolCall {
                        doc_id: tool_doc(index),
                        id: call.id.clone(),
                        call_id: call.call_id.clone(),
                        name: call.function.name.clone(),
                        arguments: call
                            .raw_arguments
                            .clone()
                            .unwrap_or_else(|| call.function.arguments.to_string())
                            .into_bytes(),
                        signature: call.signature.clone(),
                        additional_params: call
                            .additional_params
                            .as_ref()
                            .map(serde_json::Value::to_string),
                    },
                    AssistantContent::Image(image) => {
                        panic!("native assistant image {image:?} has no modeled projection case")
                    }
                })
                .collect(),
        },
        NativeMessage::User { content } => Reconstructed {
            role: Role::User,
            native_id: header.native_id.clone(),
            native_request_id: header.native_request_id.clone(),
            blocks: content
                .iter()
                .enumerate()
                .map(|(index, block)| match block {
                    UserContent::Text(text) => Block::Text {
                        payload: text.text.as_bytes().to_vec(),
                    },
                    UserContent::ToolResult(result) => Block::ToolResult {
                        doc_id: tool_doc(index),
                        id: result.id.clone(),
                        call_id: result.call_id.clone(),
                        parts: result
                            .content
                            .iter()
                            .map(|part| match part {
                                ToolResultContent::Text(text) => ResultPart::Text {
                                    payload: text.text.as_bytes().to_vec(),
                                },
                                ToolResultContent::Image(image) => panic!(
                                    "native tool-result image {image:?} has no modeled projection case"
                                ),
                            })
                            .collect(),
                    },
                    other => panic!("native user block {other:?} has no modeled projection case"),
                })
                .collect(),
        },
        NativeMessage::System { content } => Reconstructed {
            role: Role::System,
            native_id: header.native_id.clone(),
            native_request_id: header.native_request_id.clone(),
            blocks: vec![Block::Text {
                payload: content.as_bytes().to_vec(),
            }],
        },
    }
}

/// The modeled view production's projection stands for; a published header is
/// the observed envelope whose translation production selected.
pub fn view_wire(view: &LiveView, observation: &Observation) -> View {
    let streams = |streams: &[gents_protocol::output::LiveStream]| {
        streams.iter().map(live_stream_wire).collect()
    };
    match view {
        LiveView::Absent => View::Absent,
        LiveView::Live { streams: live } => View::Live {
            streams: streams(live),
        },
        LiveView::Settling { streams: live } => View::Settling {
            streams: streams(live),
        },
        LiveView::Loading => View::Loading,
        LiveView::Denied => View::Denied,
        LiveView::Conflicted => View::Conflicted,
        LiveView::Invalid => View::Invalid,
        LiveView::Retracted => View::Retracted,
        LiveView::RetainedPartial { streams: live } => View::RetainedPartial {
            streams: streams(live),
        },
        LiveView::Published {
            message: published,
            native,
        } => {
            let selected = observation
                .messages
                .iter()
                .find(|candidate| message(candidate) == **published)
                .unwrap_or_else(|| {
                    panic!("published header {published:?} is no observed envelope")
                });
            View::Published {
                message: selected.clone(),
                native: Box::new(reconstructed_wire(native, published)),
            }
        }
    }
}

/// Production's projection of an observation.
pub fn project(observation: &Observation) -> LiveView {
    let records = segments(&observation.records, observation.session);
    let observed: Vec<ObservedSegment<'_>> = records
        .iter()
        .map(|(doc_id, segment)| ObservedSegment { doc_id, segment })
        .collect();
    let messages: Vec<(String, TranscriptMessage)> = observation
        .messages
        .iter()
        .map(|candidate| (nat(candidate.header.id), message(candidate)))
        .collect();
    let message_refs: Vec<(&str, &TranscriptMessage)> = messages
        .iter()
        .map(|(id, candidate)| (id.as_str(), candidate))
        .collect();
    let ids = |ids: &[u64]| ids.iter().map(|id| nat(*id)).collect::<Vec<_>>();
    let denied_headers = ids(&observation.denied_headers);
    let denied_segments = ids(&observation.denied_segments);
    let dependency_denials: Vec<OutputDependencyDenial> = observation
        .dependency_denials
        .iter()
        .map(|denial| OutputDependencyDenial {
            root_close_id: nat(denial.root_close_id),
            denied_doc_id: nat(denial.denied_doc_id),
        })
        .collect();
    let current_request = observation
        .owner
        .current_request
        .map(|(request, generation)| (nat(request), nat(generation)));
    let live_tools = ids(&observation.owner.live_tools);
    let request_doc_id = nat(observation.request);
    let session_id = nat(observation.session);
    let target_request = nat(observation.target.coordinate.request);
    let target_source = source(&observation.target.coordinate.source);
    let target_writer = writer(&observation.target.writer);
    let target_message = observation.target.message_id.map(nat);
    project_live(&LiveObservation {
        request_doc_id: &request_doc_id,
        session_id: &session_id,
        target: LiveTarget {
            request_doc_id: &target_request,
            source: &target_source,
            writer: &target_writer,
            message_id: target_message.as_deref(),
        },
        messages: &message_refs,
        node_did: NODE_DID,
        requester_did: None,
        records: &observed,
        denied_headers: &denied_headers,
        denied_segments: &denied_segments,
        dependency_denials: &dependency_denials,
        owner: OwnerLiveness {
            current_request: current_request
                .as_ref()
                .map(|(request, generation)| (request.as_str(), generation.as_str())),
            live_tools: live_tools.iter().map(String::as_str).collect(),
        },
        request_terminal: observation.request_terminal,
        terminal_selection: observation.terminal_selection.as_ref().map(
            |selection| match selection {
                TerminalSelection::Message { id } => TerminalOutput::Message {
                    message_doc_id: nat(*id),
                },
                TerminalSelection::NoMessage => TerminalOutput::NoMessage,
            },
        ),
    })
}

pub fn outcome_wire(outcome: OutputOutcome) -> Outcome {
    match outcome {
        OutputOutcome::Complete => Outcome::Complete,
        OutputOutcome::Partial => Outcome::Partial,
    }
}

pub fn closure_wire(close: &SourceClose) -> Closure {
    match close {
        SourceClose::Closed {
            outcome,
            segments,
            stream_bytes,
        } => Closure::Closed {
            outcome: outcome_wire(*outcome),
            segments: u64::from(*segments),
            stream_bytes: stream_bytes.clone(),
        },
        SourceClose::Retracted => Closure::Retracted,
    }
}

/// The audit verdict of production's dense-prefix reconstruction.
pub fn audit_wire(prefix: Result<DensePrefix, DensePrefixError>) -> AuditResult {
    match prefix {
        Ok(prefix) => AuditResult::Ok {
            streams: prefix.streams.iter().map(stream_wire).collect(),
        },
        Err(DensePrefixError::Loading) => AuditResult::Loading,
        Err(DensePrefixError::Conflicted { .. }) => AuditResult::Conflicted,
        Err(DensePrefixError::Invalid { .. }) => AuditResult::Invalid,
    }
}

/// Production's dense audit prefix of an observation's target.
pub fn audit(observation: &Observation) -> AuditResult {
    let records = segments(&observation.records, observation.session);
    let observed: Vec<ObservedSegment<'_>> = records
        .iter()
        .map(|(doc_id, segment)| ObservedSegment { doc_id, segment })
        .collect();
    audit_wire(reconstruct_dense_prefix(
        &observed,
        &nat(observation.target.coordinate.request),
        &source(&observation.target.coordinate.source),
        &writer(&observation.target.writer),
        None,
    ))
}

fn media_payloads(media: &Media<Vec<u8>>) -> Vec<&Vec<u8>> {
    match &media.data {
        MediaPayload::Base64 { payload }
        | MediaPayload::Raw { payload }
        | MediaPayload::String { payload } => vec![payload],
        MediaPayload::Url { .. } | MediaPayload::Unknown => Vec::new(),
    }
}

/// The payload fields of a reconstructed message: inline identifiers, media
/// URLs and signatures are not payload.
pub fn payload_fields(native: &Reconstructed) -> Vec<&Vec<u8>> {
    native
        .blocks
        .iter()
        .flat_map(|block| match block {
            Block::Text { payload } => vec![payload],
            Block::Reasoning { parts, .. } => parts
                .iter()
                .map(|part| match part {
                    ReasoningPart::Text { payload, .. }
                    | ReasoningPart::Encrypted { payload }
                    | ReasoningPart::Redacted { payload }
                    | ReasoningPart::Summary { payload } => payload,
                })
                .collect(),
            Block::ToolCall { arguments, .. } => vec![arguments],
            Block::ToolResult { parts, .. } => parts
                .iter()
                .flat_map(|part| match part {
                    ResultPart::Text { payload } => vec![payload],
                    ResultPart::Media { value } => media_payloads(value),
                })
                .collect(),
            Block::Media { value } => media_payloads(value),
        })
        .collect()
}
