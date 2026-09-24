use std::future::Future;
use std::pin::Pin;

use serde::Deserialize;

pub(crate) type ProjectionFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanCanonicalOutputProjectionCase {
    pub(crate) name: String,
    pub(crate) input: LeanCanonicalOutputObservation,
    pub(crate) expected: LeanCanonicalOutputView,
    pub(crate) rendered_kinds: Vec<LeanRenderedKind>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LeanRenderedKind {
    Text,
    Reasoning,
    Summary,
    Arguments,
    ToolOutput,
    Media,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanCanonicalOutputObservation {
    pub(crate) request: u64,
    pub(crate) session: u64,
    pub(crate) records: Vec<LeanCanonicalSegment>,
    pub(crate) messages: Vec<LeanCanonicalMessage<LeanPayloadSpec>>,
    pub(crate) denied_headers: Vec<u64>,
    pub(crate) denied_segments: Vec<u64>,
    pub(crate) dependency_denials: Vec<LeanDependencyDenial>,
    pub(crate) owner: LeanOutputOwner,
    pub(crate) target: LeanOutputTarget,
    pub(crate) request_terminal: bool,
    pub(crate) terminal_selection: Option<LeanTerminalSelection>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanDependencyDenial {
    pub(crate) root_close_id: u64,
    pub(crate) denied_doc_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanOutputOwner {
    pub(crate) current_request: Option<(u64, u64)>,
    pub(crate) live_tools: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanOutputTarget {
    pub(crate) coordinate: LeanCanonicalCoordinate,
    pub(crate) writer: LeanCanonicalWriter,
    pub(crate) message_id: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum LeanCanonicalSource {
    Provider { scope: u64, turn: u64, attempt: u64 },
    Tool { call: u64 },
    Authored { key: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanCanonicalCoordinate {
    pub(crate) request: u64,
    pub(crate) source: LeanCanonicalSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum LeanCanonicalWriter {
    Request { generation: u64 },
    Tool { call: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanCanonicalSegment {
    pub(crate) id: u64,
    pub(crate) coordinate: LeanCanonicalCoordinate,
    pub(crate) writer: LeanCanonicalWriter,
    pub(crate) flush: Option<LeanCanonicalFlush>,
    pub(crate) close: Option<LeanCanonicalClosure>,
    pub(crate) created_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanCanonicalFlush {
    pub(crate) ordinal: u64,
    pub(crate) runs: Vec<LeanCanonicalRun>,
    pub(crate) payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanCanonicalRun {
    pub(crate) stream: u64,
    pub(crate) bytes: u64,
    pub(crate) declaration: Option<LeanCanonicalDeclaration>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanCanonicalDeclaration {
    pub(crate) block: u64,
    pub(crate) part: u64,
    pub(crate) kind: LeanPayloadKind,
    pub(crate) tool: Option<LeanToolIdentity>,
    pub(crate) media_kind: Option<LeanMediaKind>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LeanPayloadKind {
    Text,
    Reasoning,
    Summary,
    Opaque,
    Arguments,
    ToolOutput,
    Media,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LeanMediaKind {
    Image,
    Audio,
    Video,
    Document,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LeanOutcome {
    Complete,
    Partial,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LeanMessageRole {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanToolIdentity {
    pub(crate) id: String,
    pub(crate) call_id: Option<String>,
    pub(crate) name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum LeanCanonicalClosure {
    Closed {
        outcome: LeanOutcome,
        segments: u64,
        stream_bytes: Vec<u64>,
    },
    Retracted,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanPayloadRef {
    pub(crate) close_id: u64,
    pub(crate) stream: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanPayloadSpec {
    pub(crate) reference: LeanPayloadRef,
    pub(crate) presentation: LeanPresentation,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum LeanPresentation {
    Full,
    Composed { parts: Vec<LeanPresentationPart> },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum LeanPresentationPart {
    Range { start: u64, end: u64 },
    Literal { bytes: Vec<u8> },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanCanonicalHeader {
    pub(crate) id: u64,
    pub(crate) session: u64,
    pub(crate) request: Option<u64>,
    pub(crate) origin: Option<u64>,
    pub(crate) refs: Vec<LeanPayloadRef>,
    pub(crate) outcome: LeanOutcome,
    pub(crate) role: LeanMessageRole,
    pub(crate) publication: LeanMessagePublication,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum LeanMessagePublication {
    RequestExecution { generation: u64 },
    RequestRecovery { generation: u64 },
    ToolDelivery { call: u64 },
    Fork { origin: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanCanonicalMessage<T> {
    pub(crate) header: LeanCanonicalHeader,
    pub(crate) key: String,
    pub(crate) sequence: u64,
    pub(crate) native_id: Option<String>,
    pub(crate) blocks: Vec<LeanMessageBlock<T>>,
    pub(crate) created_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum LeanMessageBlock<T> {
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
pub(crate) enum LeanReasoningPart<T> {
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
pub(crate) enum LeanResultPart<T> {
    Text { payload: T },
    Media { value: LeanMedia<T> },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanMedia<T> {
    pub(crate) kind: LeanMediaKind,
    pub(crate) data: LeanMediaData<T>,
    pub(crate) media_type: Option<String>,
    pub(crate) detail: Option<String>,
    pub(crate) additional_params: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum LeanMediaData<T> {
    Url { url: String },
    Base64 { payload: T },
    Raw { payload: T },
    String { payload: T },
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum LeanTerminalSelection {
    Message { id: u64 },
    NoMessage,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum LeanCanonicalOutputView {
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
        native: LeanReconstructedMessage,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanCanonicalStream {
    pub(crate) declaration: LeanCanonicalDeclaration,
    pub(crate) bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanReconstructedMessage {
    pub(crate) role: LeanMessageRole,
    pub(crate) native_id: Option<String>,
    pub(crate) blocks: Vec<LeanMessageBlock<Vec<u8>>>,
}

/// Asynchronous boundary for the native projection owner. The adapter receives
/// only modeled inputs; fixture expectations remain owned by the harness.
pub(crate) trait CanonicalOutputProjectionAdapter {
    type Error: std::fmt::Display;

    fn project<'a>(
        &'a mut self,
        input: &'a LeanCanonicalOutputObservation,
    ) -> ProjectionFuture<'a, Result<LeanCanonicalOutputView, Self::Error>>;
}

/// Drives a native adapter using only the modeled observation. Keeping the
/// expected value outside the adapter prevents it from implementing the fixture
/// comparison instead of the production projection.
pub(crate) async fn assert_canonical_output_projection_cases<A>(
    cases: &[LeanCanonicalOutputProjectionCase],
    adapter: &mut A,
) -> Result<(), String>
where
    A: CanonicalOutputProjectionAdapter,
{
    if cases.is_empty() {
        return Err("canonical output projection fixture set must not be empty".to_owned());
    }
    for case in cases {
        let actual = adapter.project(&case.input).await.map_err(|error| {
            format!(
                "canonical output case `{}`: adapter failed: {error}",
                case.name
            )
        })?;
        if actual != case.expected {
            return Err(format!(
                "canonical output case `{}`: expected {:?}, got {actual:?}",
                case.name, case.expected
            ));
        }
    }
    Ok(())
}
