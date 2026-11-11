use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCanonicalOutputProjectionCase {
    pub name: String,
    pub input: LeanCanonicalOutputObservation,
    pub expected: LeanCanonicalOutputView,
    pub rendered_kinds: Vec<LeanRenderedKind>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeanRenderedKind {
    Text,
    Reasoning,
    Summary,
    Arguments,
    ToolOutput,
    Media,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCanonicalOutputObservation {
    pub request: u64,
    pub session: u64,
    pub records: Vec<LeanCanonicalSegment>,
    pub messages: Vec<LeanCanonicalMessage<LeanPayloadSpec>>,
    pub denied_headers: Vec<u64>,
    pub denied_segments: Vec<u64>,
    pub dependency_denials: Vec<LeanDependencyDenial>,
    pub owner: LeanOutputOwner,
    pub target: LeanOutputTarget,
    pub request_terminal: bool,
    pub terminal_selection: Option<LeanTerminalSelection>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanDependencyDenial {
    pub root_close_id: u64,
    pub denied_doc_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanOutputOwner {
    pub current_request: Option<(u64, u64)>,
    pub live_tools: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanOutputTarget {
    pub coordinate: LeanCanonicalCoordinate,
    pub writer: LeanCanonicalWriter,
    pub message_id: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanCanonicalSource {
    Provider {
        scope: u64,
        turn: u64,
        attempt: u64,
    },
    Auxiliary {
        auxiliary_kind: LeanAuxiliaryKind,
        scope: u64,
        turn: u64,
        attempt: u64,
    },
    Tool {
        call: u64,
    },
    Authored {
        key: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeanAuxiliaryKind {
    Compaction,
    CompactionFallback,
    Title,
    AccessReview,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCanonicalCoordinate {
    pub request: u64,
    pub source: LeanCanonicalSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanCanonicalWriter {
    Request { generation: u64 },
    Tool { call: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCanonicalSegment {
    pub id: u64,
    pub coordinate: LeanCanonicalCoordinate,
    pub writer: LeanCanonicalWriter,
    pub flush: Option<LeanCanonicalFlush>,
    pub close: Option<LeanCanonicalClosure>,
    pub created_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCanonicalFlush {
    pub ordinal: u64,
    pub runs: Vec<LeanCanonicalRun>,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCanonicalRun {
    pub stream: u64,
    pub bytes: u64,
    pub declaration: Option<LeanCanonicalDeclaration>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCanonicalDeclaration {
    pub block: u64,
    pub part: u64,
    pub kind: LeanPayloadKind,
    pub tool: Option<LeanToolIdentity>,
    pub media_kind: Option<LeanMediaKind>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeanPayloadKind {
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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanReasoningAuditCase {
    pub name: String,
    pub input: LeanCanonicalOutputObservation,
    pub expected: LeanReasoningAuditResult,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanReasoningAuditResult {
    Ok { streams: Vec<LeanCanonicalStream> },
    Loading,
    Conflicted,
    Invalid,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanReasoningSignatureCase {
    pub name: String,
    pub records: Vec<LeanCanonicalSegment>,
    pub payload: LeanPayloadSpec,
    pub signature: Option<String>,
    pub accepted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanAuxiliaryOutputCase {
    pub name: String,
    pub raw: LeanCanonicalSegment,
    pub closing: LeanCanonicalSegment,
    pub stale_closing: LeanCanonicalSegment,
    pub recovery_closing: LeanCanonicalSegment,
    pub authority: LeanAuxiliaryAuthority,
    pub publication_message: LeanCanonicalMessage<LeanPayloadSpec>,
    pub native_publication_closing: LeanCanonicalSegment,
    pub native_publication_message: LeanCanonicalMessage<LeanPayloadSpec>,
    pub observation: LeanCanonicalOutputObservation,
    pub expected: LeanAuxiliaryOutputExpected,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanAuxiliaryAuthority {
    pub claimed_request_state: String,
    pub generation: u64,
    pub begin_boundary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanAuxiliaryOutputExpected {
    pub append_accepted: bool,
    pub claimed_append_accepted: bool,
    pub begin_accepted: bool,
    pub begun_append_accepted: bool,
    pub close_accepted: bool,
    pub stale_close_replay_accepted: bool,
    pub recovery_accepted: bool,
    pub recovery_with_header_accepted: bool,
    pub publication_accepted: bool,
    pub empty_header_accepted: bool,
    pub native_publication_accepted: bool,
    pub audit: LeanReasoningAuditResult,
    pub public_view: LeanCanonicalOutputView,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeanMediaKind {
    Image,
    Audio,
    Video,
    Document,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeanOutcome {
    Complete,
    Partial,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeanMessageRole {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanToolIdentity {
    pub id: String,
    pub call_id: Option<String>,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanCanonicalClosure {
    Closed {
        outcome: LeanOutcome,
        segments: u64,
        stream_bytes: Vec<u64>,
    },
    Retracted,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPayloadRef {
    pub close_id: u64,
    pub stream: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPayloadSpec {
    pub reference: LeanPayloadRef,
    pub presentation: LeanPresentation,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanPresentation {
    Full,
    Composed { parts: Vec<LeanPresentationPart> },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanPresentationPart {
    Range { start: u64, end: u64 },
    Literal { bytes: Vec<u8> },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCanonicalHeader {
    pub id: u64,
    pub session: u64,
    pub request: Option<u64>,
    pub origin: Option<u64>,
    pub refs: Vec<LeanPayloadRef>,
    pub outcome: LeanOutcome,
    pub role: LeanMessageRole,
    pub publication: LeanMessagePublication,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanMessagePublication {
    RequestExecution { generation: u64 },
    RequestRecovery { generation: u64 },
    ToolDelivery { call: u64 },
    Fork { origin: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCanonicalMessage<T> {
    pub header: LeanCanonicalHeader,
    pub key: String,
    pub sequence: u64,
    pub native_id: Option<String>,
    pub native_request_id: Option<String>,
    pub blocks: Vec<LeanMessageBlock<T>>,
    pub created_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanMessageBlock<T> {
    Text {
        payload: T,
    },
    Reasoning {
        id: Option<String>,
        parts: Vec<LeanReasoningPart<T>>,
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
        parts: Vec<LeanResultPart<T>>,
    },
    Media {
        value: LeanMedia<T>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanReasoningPart<T> {
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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanResultPart<T> {
    Text { payload: T },
    Media { value: LeanMedia<T> },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanMedia<T> {
    pub kind: LeanMediaKind,
    pub data: LeanMediaData<T>,
    pub media_type: Option<String>,
    pub detail: Option<String>,
    pub additional_params: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanMediaData<T> {
    Url { url: String },
    Base64 { payload: T },
    Raw { payload: T },
    String { payload: T },
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanTerminalSelection {
    Message { id: u64 },
    NoMessage,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanCanonicalOutputView {
    Absent,
    Live {
        streams: Vec<LeanCanonicalStream>,
    },
    Settling {
        streams: Vec<LeanCanonicalStream>,
    },
    Loading,
    Denied,
    Conflicted,
    Invalid,
    Retracted,
    RetainedPartial {
        streams: Vec<LeanCanonicalStream>,
    },
    Published {
        message: LeanCanonicalMessage<LeanPayloadSpec>,
        native: Box<LeanReconstructedMessage>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCanonicalStream {
    pub declaration: LeanCanonicalDeclaration,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanReconstructedMessage {
    pub role: LeanMessageRole,
    pub native_id: Option<String>,
    pub native_request_id: Option<String>,
    pub blocks: Vec<LeanMessageBlock<Vec<u8>>>,
}
