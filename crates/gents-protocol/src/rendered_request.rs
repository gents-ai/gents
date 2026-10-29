use serde::{Deserialize, Serialize};

use crate::message::{Message, ToolResultContent, UserContent};

/// Capture format version. A reader that does not know it must report the row
/// as unsupported rather than guessing.
pub const CAPTURE_VERSION: u32 = 4;

/// Provenance manifest version. A reader that does not know this number must
/// report `UnsupportedManifest` rather than guessing.
pub const PROVENANCE_MANIFEST_VERSION: u32 = 4;

/// Assembly-trace version, versioned independently of the manifest.
pub const ASSEMBLY_TRACE_VERSION: u32 = 4;

/// Version of the request-bound context accounting payload; the exact values
/// the runtime used for its dispatch decision, not a provider tokenizer claim.
pub const CONTEXT_ACCOUNTING_VERSION: u32 = 3;

/// Request paths whose body is a completion request. The capturing transport
/// only claims a pending capture for one of these.
const COMPLETION_REQUEST_PATHS: &[(&str, RenderedRequestSource)] = &[
    ("/responses", RenderedRequestSource::OpenAiResponses),
    (
        "/chat/completions",
        RenderedRequestSource::OpenAiChatCompletions,
    ),
    ("/messages", RenderedRequestSource::ClaudeCliSubscription),
];

/// The provider wire shape a captured body was actually sent on, derived from
/// the request path the transport posted to, never from agent configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RenderedRequestSource {
    #[serde(rename = "openai_responses")]
    OpenAiResponses,
    #[serde(rename = "openai_chat_completions")]
    OpenAiChatCompletions,
    #[serde(rename = "claude_cli_subscription")]
    ClaudeCliSubscription,
}

impl RenderedRequestSource {
    /// `None` means "not a completion request": the transport forwards without
    /// capturing.
    pub fn for_request_path(path: &str) -> Option<Self> {
        COMPLETION_REQUEST_PATHS
            .iter()
            .find(|(suffix, _)| path.ends_with(suffix))
            .map(|(_, source)| *source)
    }

    pub fn messages_field(self) -> &'static str {
        match self {
            Self::OpenAiResponses => "input",
            Self::OpenAiChatCompletions | Self::ClaudeCliSubscription => "messages",
        }
    }
}

/// Which completion loop inside a request is issuing provider calls.
/// Declaration order is the ordering `CaptureOrderKey` sorts kinds by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CaptureScopeKind {
    /// The request's own owned completion loop.
    Inference,
    /// The guided per-turn compaction summarizer.
    Compaction,
    /// The strict-JSON compaction fallback.
    CompactionFallback,
    /// Conversation title generation.
    Title,
    /// The one-shot runner.
    OneShot,
    /// A workspace access review with audit-only output.
    AccessReview,
}

impl CaptureScopeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inference => "inference",
            Self::Compaction => "compaction",
            Self::CompactionFallback => "compaction_fallback",
            Self::Title => "title",
            Self::OneShot => "oneshot",
            Self::AccessReview => "access_review",
        }
    }

    pub fn from_label(label: &str) -> Option<Self> {
        match label {
            "inference" => Some(Self::Inference),
            "compaction" => Some(Self::Compaction),
            "compaction_fallback" => Some(Self::CompactionFallback),
            "title" => Some(Self::Title),
            "oneshot" => Some(Self::OneShot),
            "access_review" => Some(Self::AccessReview),
            _ => None,
        }
    }

    pub const fn request_purpose(self) -> crate::request_admission::RequestPurpose {
        use crate::request_admission::RequestPurpose;
        match self {
            Self::Title => RequestPurpose::TitleAudit,
            Self::AccessReview => RequestPurpose::AccessReview,
            Self::Inference | Self::Compaction | Self::CompactionFallback | Self::OneShot => {
                RequestPurpose::Normal
            }
        }
    }
}

impl std::fmt::Display for CaptureScopeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A parsed `capture_scope` column: which loop, and which allocation of that
/// loop within the request. Serializes as `"{kind}.{seq}"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CaptureScope {
    pub kind: CaptureScopeKind,
    pub seq: u64,
}

impl From<CaptureScope> for String {
    fn from(scope: CaptureScope) -> Self {
        scope.to_string()
    }
}

impl TryFrom<String> for CaptureScope {
    type Error = CaptureScopeParseError;

    fn try_from(label: String) -> Result<Self, Self::Error> {
        label.parse()
    }
}

/// Malformed scope is an error a consumer must surface, never a default:
/// defaulting would silently merge the row into another loop's ordering.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CaptureScopeParseError {
    Empty,
    MissingSeparator(String),
    UnknownKind(String),
    InvalidSeq(String),
}

impl std::fmt::Display for CaptureScopeParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "capture scope is empty"),
            Self::MissingSeparator(scope) => {
                write!(f, "capture scope {scope:?} is not \"{{kind}}.{{seq}}\"")
            }
            Self::UnknownKind(kind) => write!(f, "unknown capture scope kind {kind:?}"),
            Self::InvalidSeq(seq) => write!(f, "capture scope seq {seq:?} is not a u64"),
        }
    }
}

impl std::error::Error for CaptureScopeParseError {}

impl std::str::FromStr for CaptureScope {
    type Err = CaptureScopeParseError;

    fn from_str(label: &str) -> Result<Self, Self::Err> {
        if label.is_empty() {
            return Err(CaptureScopeParseError::Empty);
        }
        let (kind, seq) = label
            .rsplit_once('.')
            .filter(|(kind, seq)| !kind.is_empty() && !seq.is_empty())
            .ok_or_else(|| CaptureScopeParseError::MissingSeparator(label.to_string()))?;
        let kind = CaptureScopeKind::from_label(kind)
            .ok_or_else(|| CaptureScopeParseError::UnknownKind(kind.to_string()))?;
        if !seq.bytes().all(|b| b.is_ascii_digit()) {
            return Err(CaptureScopeParseError::InvalidSeq(seq.to_string()));
        }
        let seq = seq
            .parse::<u64>()
            .map_err(|_| CaptureScopeParseError::InvalidSeq(seq.to_string()))?;
        Ok(Self { kind, seq })
    }
}

impl std::fmt::Display for CaptureScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.kind, self.seq)
    }
}

/// The stable identity ordering of capture rows within one request:
/// `(kind, seq, turn_index, attempt)`, with `seq` compared numerically.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CaptureOrderKey {
    pub scope: CaptureScope,
    pub turn_index: i64,
    pub attempt: i64,
}

impl CaptureOrderKey {
    pub fn padded(&self) -> String {
        format!(
            "{:02}.{}.{:010}.{:06}.{:06}",
            self.scope.kind as u8, self.scope.kind, self.scope.seq, self.turn_index, self.attempt
        )
    }
}

/// Why a row could not produce a [`CaptureOrderKey`]; missing core facts cannot
/// be defaulted to zero without colliding with real rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CaptureOrderKeyError {
    MissingScope,
    Scope(CaptureScopeParseError),
    MissingTurnIndex,
    MissingAttempt,
}

impl std::fmt::Display for CaptureOrderKeyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingScope => write!(f, "row has no capture_scope"),
            Self::Scope(err) => write!(f, "{err}"),
            Self::MissingTurnIndex => write!(f, "row has no turn_index"),
            Self::MissingAttempt => write!(f, "row has no attempt"),
        }
    }
}

impl std::error::Error for CaptureOrderKeyError {}

impl From<CaptureScopeParseError> for CaptureOrderKeyError {
    fn from(err: CaptureScopeParseError) -> Self {
        Self::Scope(err)
    }
}

/// Outcome of reading a `provenance_json` column.
#[derive(Clone, Debug, PartialEq)]
pub enum ParsedProvenance {
    Manifest(Box<ProvenanceManifest>),
    /// Written by a runtime this reader does not understand; the row is still
    /// listed, only its provenance is opaque.
    Unsupported {
        manifest_version: u32,
    },
}

/// A malformed `provenance_json` column, distinct from an
/// unknown-but-well-formed version.
#[derive(Debug)]
pub enum ProvenanceParseError {
    Empty,
    InvalidJson(serde_json::Error),
    MissingVersion,
    InvalidManifest(serde_json::Error),
}

impl std::fmt::Display for ProvenanceParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "provenance_json is empty"),
            Self::InvalidJson(err) => write!(f, "provenance_json is not JSON: {err}"),
            Self::MissingVersion => {
                write!(f, "provenance_json carries no numeric manifest_version")
            }
            Self::InvalidManifest(err) => {
                write!(
                    f,
                    "provenance_json does not match its declared version: {err}"
                )
            }
        }
    }
}

impl std::error::Error for ProvenanceParseError {}

impl ProvenanceManifest {
    /// Read a `provenance_json` column, gating on `manifest_version` before
    /// committing to a shape. A v3 manifest is projected to a v4 metadata
    /// value; callers that export evidence must retain the raw column
    /// alongside this normalized view.
    pub fn parse(provenance_json: &str) -> Result<ParsedProvenance, ProvenanceParseError> {
        if provenance_json.trim().is_empty() {
            return Err(ProvenanceParseError::Empty);
        }
        let value: serde_json::Value =
            serde_json::from_str(provenance_json).map_err(ProvenanceParseError::InvalidJson)?;
        let manifest_version = value
            .get("manifest_version")
            .and_then(serde_json::Value::as_u64)
            .and_then(|version| u32::try_from(version).ok())
            .ok_or(ProvenanceParseError::MissingVersion)?;
        if manifest_version == 3 {
            let legacy: LegacyProvenanceManifest =
                serde_json::from_value(value).map_err(ProvenanceParseError::InvalidManifest)?;
            return Ok(ParsedProvenance::Manifest(Box::new(legacy.into_current())));
        }
        if manifest_version != PROVENANCE_MANIFEST_VERSION {
            return Ok(ParsedProvenance::Unsupported { manifest_version });
        }
        serde_json::from_value(value)
            .map(|m| ParsedProvenance::Manifest(Box::new(m)))
            .map_err(ProvenanceParseError::InvalidManifest)
    }
}

/// Which of the owned loop's two request builders produced the captured
/// `CompletionRequest`: one of the four unrecoverable inputs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssemblyBuildPath {
    /// Ordinary assembly and per-turn compaction when the request exceeded the
    /// input budget.
    Budgeted,
    /// Invoked directly by a completion-retry repair.
    Repair,
}

/// A provider-assigned assistant message id, positioned in the effective
/// message list. One of the four unrecoverable inputs: the id exists in the
/// provider request and nowhere in the durable transcript.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssistantMessageId {
    /// Index into `AssemblyTrace::effective_messages`.
    pub message_index: usize,
    pub message_id: String,
}

/// The exact tool-result content threaded back into provider history for one
/// tool call. One of the four unrecoverable inputs: the loop and the
/// persistence path derive different text from different sources, so replaying
/// from the transcript does not reproduce the bytes the model saw. `content`
/// is the full threaded `Vec<ToolResultContent>`: the part split is part of
/// what the provider received.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ThreadedToolResult {
    /// Index into `AssemblyTrace::effective_messages`.
    pub message_index: usize,
    /// `ToolResult.id` — rig's locally minted tool-call id.
    pub tool_call_id: String,
    /// `ToolResult.call_id` — the provider-side call id when one exists.
    pub call_id: Option<String>,
    pub content: Vec<ToolResultContent>,
}

/// Why the successful provider dispatch did or did not use per-turn
/// compaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextCompactionReason {
    /// The assembled input fit below the configured threshold.
    BelowThreshold,
    /// A durable `ProviderContextReduction` was activated before dispatch.
    Compacted,
    /// A request-local, non-durable repair made the provider view fit without
    /// a reduction checkpoint.
    ProviderViewRepaired,
    /// This completion loop had no per-turn compactor.
    CompactorUnavailable,
}

/// Token-estimate components for the exact provider-wire request that was
/// captured. Their saturating sum is the estimator's own count:
/// `raw_estimated_input_tokens` from accounting version 3, before that
/// `estimated_input_tokens`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextInputComponents {
    pub messages: usize,
    pub documents: usize,
    pub tool_schemas: usize,
    pub additional_parameters: usize,
    pub output_schema: usize,
}

impl ContextInputComponents {
    pub fn estimated_input_tokens(&self) -> usize {
        self.messages
            .saturating_add(self.documents)
            .saturating_add(self.tool_schemas)
            .saturating_add(self.additional_parameters)
            .saturating_add(self.output_schema)
    }
}

/// Request-bound context accounting persisted with the exact rendered request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextAccounting {
    pub accounting_version: u32,
    #[serde(default)]
    pub turn_index: usize,
    #[serde(default)]
    pub attempt: u32,
    pub estimator: String,
    pub components: ContextInputComponents,
    /// The input the provider is expected to count: the estimator's count
    /// scaled by `input_calibration_basis_points` from accounting version 3.
    pub estimated_input_tokens: usize,
    /// The estimator's own count, before calibration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_estimated_input_tokens: Option<usize>,
    /// Provider tokens per estimated token, in basis points, observed for this
    /// session and model when the request was dispatched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_calibration_basis_points: Option<u64>,
    /// The session's latest completed call's provider count plus the
    /// calibrated growth since it, when that call anchored this request.
    /// `estimated_input_tokens` is never below it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchored_input_tokens: Option<usize>,
    pub context_window: usize,
    pub compaction_threshold_basis_points: u64,
    pub compaction_threshold_tokens: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub configured_max_output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_max_output_tokens: Option<u64>,
    pub compaction_reason: ContextCompactionReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_compaction_input_tokens: Option<usize>,
}

/// The request-local inputs that reconstruction must account for explicitly;
/// everything else that shapes a request is durable or a pure function of
/// durable data. `assistant_message_ids` and `threaded_tool_results` are
/// overlays keyed by position and call id; `effective_messages` is the oracle
/// a reconstructor checks itself against, present only when request-context
/// rendering, per-turn compaction, or a repair introduced request-local
/// content. Per-turn compaction is a sticky mutation whose exact source split
/// and checkpoint are written as a `ProviderContextReduction`; re-running the
/// summarizer is not reconstruction.
///
/// `threaded_tool_results` is carried on **every** turn, compact path
/// included, because rig joins multi-part tool-result content with `"\n"` on
/// the Chat Completions wire — the native split is unrecoverable from
/// `request_json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AssemblyTrace {
    pub trace_version: u32,
    pub build_path: AssemblyBuildPath,
    /// Number of native messages passed to assembly.
    pub effective_message_count: usize,
    /// The full effective provider message list at capture time, retained only
    /// after request-context rendering or per-turn compaction introduces
    /// request-local content.
    pub effective_messages: Option<Vec<Message>>,
    pub assistant_message_ids: Vec<AssistantMessageId>,
    pub threaded_tool_results: Vec<ThreadedToolResult>,
    /// Ordered immutable per-turn reduction facts that causally determine the
    /// sticky provider projection used for this request.
    pub reduction_keys: Vec<String>,
    pub context_accounting: Option<ContextAccounting>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AssemblyTraceMetadata {
    pub trace_version: u32,
    pub build_path: AssemblyBuildPath,
    pub effective_message_count: usize,
    pub reduction_keys: Vec<String>,
    pub context_accounting: Option<ContextAccounting>,
}

impl From<&AssemblyTrace> for AssemblyTraceMetadata {
    fn from(trace: &AssemblyTrace) -> Self {
        Self {
            trace_version: trace.trace_version,
            build_path: trace.build_path,
            effective_message_count: trace.effective_message_count,
            reduction_keys: trace.reduction_keys.clone(),
            context_accounting: trace.context_accounting.clone(),
        }
    }
}

impl AssemblyTrace {
    /// The only constructor that keeps the overlays consistent with
    /// `effective_messages`.
    pub fn from_effective_messages(
        build_path: AssemblyBuildPath,
        effective_messages: Vec<Message>,
    ) -> Self {
        Self::from_messages(build_path, effective_messages, true)
    }

    /// Build the compact common-path trace when the native list is
    /// reconstructible from durable rows plus the overlays.
    pub fn from_reconstructible_messages(
        build_path: AssemblyBuildPath,
        effective_messages: Vec<Message>,
    ) -> Self {
        Self::from_messages(build_path, effective_messages, false)
    }

    fn from_messages(
        build_path: AssemblyBuildPath,
        effective_messages: Vec<Message>,
        retain_effective_messages: bool,
    ) -> Self {
        let mut assistant_message_ids = Vec::new();
        let mut threaded_tool_results = Vec::new();

        for (message_index, message) in effective_messages.iter().enumerate() {
            match message {
                Message::Assistant {
                    id: Some(message_id),
                    ..
                } => assistant_message_ids.push(AssistantMessageId {
                    message_index,
                    message_id: message_id.clone(),
                }),
                Message::User { content } => {
                    for item in content {
                        if let UserContent::ToolResult(result) = item {
                            threaded_tool_results.push(ThreadedToolResult {
                                message_index,
                                tool_call_id: result.id.clone(),
                                call_id: result.call_id.clone(),
                                content: result.content.clone(),
                            });
                        }
                    }
                }
                _ => {}
            }
        }

        let effective_message_count = effective_messages.len();
        Self {
            trace_version: ASSEMBLY_TRACE_VERSION,
            build_path,
            effective_message_count,
            effective_messages: retain_effective_messages.then_some(effective_messages),
            assistant_message_ids,
            threaded_tool_results,
            reduction_keys: Vec::new(),
            context_accounting: None,
        }
    }

    pub fn with_reduction_keys(mut self, reduction_keys: Vec<String>) -> Self {
        self.reduction_keys = reduction_keys;
        self
    }

    pub fn with_context_accounting(mut self, context_accounting: ContextAccounting) -> Self {
        self.context_accounting = Some(context_accounting);
        self
    }
}

/// How much this capture claims about reconstructibility; explicit, never
/// inferred from an absent field.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProvenanceStatus {
    /// The rendered request is durable and exact, but no durable source
    /// versions are pinned alongside it.
    CapturedOnly,
}

/// Where in the send path the captured bytes were read, recorded positively so
/// a reader never has to infer it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureSeam {
    /// The last `HttpClientExt` before the network client; the bytes the HTTP
    /// client forwarded, after every provider-specific rewrite.
    TransportBody,
}

/// The admission identity of the provider call this capture preceded. A
/// stored key, not an ordinal guess: an admission rejection consumes no
/// `call_seq`, so counting rows on either side desynchronises. Absent for
/// one-shot runs and on v2 rows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmissionJoin {
    pub call_id: String,
    pub call_seq: i64,
}

/// Versioned provenance travelling in the `provenance_json` column. A v1 row
/// must remain readable and must still report `CapturedOnly`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProvenanceManifest {
    pub manifest_version: u32,
    pub status: ProvenanceStatus,
    /// Why this row is not `Verified`, in words.
    pub status_reason: String,
    pub capture_seam: CaptureSeam,
    /// Mirrors the `capture_scope` column.
    pub capture_scope: String,
    /// Scheme and host authority the body was actually posted to, observed at
    /// the seam; URL userinfo is excluded. Transport evidence otherwise lost
    /// in time: a configured endpoint is not proof of the route used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_endpoint: Option<String>,
    /// Canonical family of the already-built completion client, not a later
    /// configuration lookup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_family: Option<String>,
    /// SHA-256 of the transport-observed URI path; absent when the URI has a
    /// query, since query-based routing cannot be established without keeping
    /// potentially secret values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_route_path_sha256: Option<String>,
    /// The admitted call this capture preceded. Absence on a one-shot capture
    /// is a documented fact, not an error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admission: Option<AdmissionJoin>,
    pub assembly_trace: AssemblyTraceMetadata,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct LegacyProvenanceManifest {
    manifest_version: u32,
    status: ProvenanceStatus,
    status_reason: String,
    capture_seam: CaptureSeam,
    capture_scope: String,
    #[serde(default)]
    provider_endpoint: Option<String>,
    #[serde(default)]
    admission: Option<AdmissionJoin>,
    assembly_trace: AssemblyTrace,
}

impl LegacyProvenanceManifest {
    fn into_current(self) -> ProvenanceManifest {
        ProvenanceManifest {
            manifest_version: PROVENANCE_MANIFEST_VERSION,
            status: self.status,
            status_reason: self.status_reason,
            capture_seam: self.capture_seam,
            capture_scope: self.capture_scope,
            provider_endpoint: self.provider_endpoint,
            provider_family: None,
            provider_route_path_sha256: None,
            admission: self.admission,
            assembly_trace: (&self.assembly_trace).into(),
        }
    }
}

impl ProvenanceManifest {
    const CAPTURED_ONLY_REASON: &'static str =
        "this provenance manifest pins no config or transcript versions, so a \
         reconstruction cannot be verified against this capture";

    pub fn captured_only(
        capture_scope: String,
        provider_endpoint: Option<String>,
        admission: Option<AdmissionJoin>,
        assembly_trace: AssemblyTrace,
    ) -> Self {
        Self {
            manifest_version: PROVENANCE_MANIFEST_VERSION,
            status: ProvenanceStatus::CapturedOnly,
            status_reason: Self::CAPTURED_ONLY_REASON.to_string(),
            capture_seam: CaptureSeam::TransportBody,
            capture_scope,
            provider_endpoint,
            provider_family: None,
            provider_route_path_sha256: None,
            admission,
            assembly_trace: (&assembly_trace).into(),
        }
    }

    pub fn with_provider_route(
        mut self,
        family: Option<String>,
        path_sha256: Option<String>,
    ) -> Self {
        self.provider_family = family;
        self.provider_route_path_sha256 = path_sha256;
        self
    }
}
