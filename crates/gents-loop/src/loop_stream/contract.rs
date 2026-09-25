use super::*;

use crate::claude_messages_body::{ReplayIssuer, ReplayTag, ReplayWire, ResolvedReplayEvidence};

/// One native row and its independently established canonical provider source.
/// The tag follows this row through provider-view projection by the projection's
/// emitted source index, never by provider-generated IDs or equal content.
#[derive(Clone, Debug, PartialEq)]
pub struct TaggedMessage {
    pub message: Message,
    pub source: Option<ReplayTag>,
    /// Canonical physical header, carried independently of native content.
    pub physical_header: Option<String>,
    /// Original physical assistant block positions after provider-view shaping.
    pub block_indices: Vec<usize>,
}

impl TaggedMessage {
    pub fn unassociated(message: Message) -> Self {
        Self {
            message,
            source: None,
            physical_header: None,
            block_indices: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReplayEvidenceRow {
    pub tag: ReplayTag,
    pub evidence: ResolvedReplayEvidence,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReplayProjectionContext {
    pub issuer: ReplayIssuer,
    pub wire: ReplayWire,
    pub body: serde_json::Value,
}

/// A deterministic violation of canonical replay provenance. Native owners
/// preserve this type through contextual errors; storage failures must not be
/// relabeled as invalid provider input.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ReplayEvidenceViolation(pub String);

/// A native canonical lookup. The returned list deliberately retains zero or
/// multiple matches so the replay owner can reject missing/ambiguous evidence.
pub type ReplayEvidenceResolver = Arc<
    dyn Fn(
            Vec<ReplayTag>,
            ReplayProjectionContext,
        ) -> Pin<Box<dyn Future<Output = anyhow::Result<Vec<ReplayEvidenceRow>>> + Send>>
        + Send
        + Sync,
>;

#[derive(Clone, Default)]
pub struct LoopReplayInput {
    /// Present only for a persisted request with a real canonical owner.
    pub request_doc_id: Option<String>,
    /// Exact built-client route, absent when endpoint identity is unproven.
    pub issuer: Option<ReplayIssuer>,
    /// Wire selected by the running loop's actual provider profile.
    pub wire: Option<ReplayWire>,
    /// Required-current coordinates are independent of the surviving rows.
    pub required: Vec<ReplayTag>,
    /// Current signed rows awaiting canonical-origin selection; a foreign
    /// producer stays historical rather than becoming claimed Claude replay.
    pub candidates: Vec<ReplayTag>,
    /// Signed coordinates permanently omitted after a client prefix rewrite.
    pub retired: Vec<ReplayTag>,
    pub resolve: Option<ReplayEvidenceResolver>,
}

/// `(turn_index, attempt, request, assembly_trace)`.
///
/// The trace rides alongside the request because the assembled
/// `CompletionRequest` is the *output* of prompt assembly and cannot explain
/// its own inputs: the provider-assigned assistant message ids, the exact
/// threaded tool-result content, the post-compaction message list, and which
/// builder produced it are all in-memory facts that die with the loop. See
/// `crate::rendered_request::AssemblyTrace`.
pub type RenderedRequestSink = Arc<
    dyn Fn(
            usize,
            u32,
            CompletionRequest,
            AssemblyTrace,
        ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>>
        + Send
        + Sync,
>;

#[derive(Clone, Debug)]
pub struct TurnCompactionRequest {
    pub messages: Vec<TaggedMessage>,
    /// Independent required-current coordinates, checked before persisting
    /// any reduced provider checkpoint.
    pub required: Vec<ReplayTag>,
    pub admission: crate::compaction::ReductionAdmission,
    pub turn_index: usize,
    pub prior_reduction_keys: Vec<String>,
}

#[derive(Clone, Debug)]
pub enum TurnCompactionOutcome {
    ProviderViewRepaired {
        messages: Vec<TaggedMessage>,
    },
    Reduced {
        messages: Vec<TaggedMessage>,
        reduction_key: String,
    },
    CannotFit,
}

pub type TurnCompactor = Arc<
    dyn Fn(
            TurnCompactionRequest,
        ) -> Pin<Box<dyn Future<Output = anyhow::Result<TurnCompactionOutcome>> + Send>>
        + Send
        + Sync,
>;

pub type StructuredOutputValidator = Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>;

/// A typed-output contract carried through the owned completion loop.
///
/// Rig owns the provider schema transport. Gents keeps ownership of the loop
/// so schema validation participates in its deadline-aware, formally modelled
/// retract-and-resample lifecycle instead of bypassing persistence and hooks
/// through `rig::Agent::prompt_typed`.
#[derive(Clone)]
pub struct StructuredOutputConfig {
    pub(super) schema: schemars::Schema,
    pub(super) validate: StructuredOutputValidator,
}

impl StructuredOutputConfig {
    pub fn for_type<T>() -> Self
    where
        T: DeserializeOwned + schemars::JsonSchema + 'static,
    {
        Self {
            schema: schemars::schema_for!(T),
            validate: Arc::new(|raw| {
                serde_json::from_str::<T>(raw)
                    .map(|_| ())
                    .map_err(|error| {
                        format!(
                            "{error}; raw_output_preview={}; finish_metadata=unavailable_at_rig_streaming_boundary",
                            bounded_structured_output_preview(raw)
                        )
                    })
            }),
        }
    }
}

fn bounded_structured_output_preview(raw: &str) -> String {
    const MAX_PREVIEW_BYTES: usize = 192;
    let mut cut = raw.len().min(MAX_PREVIEW_BYTES);
    while !raw.is_char_boundary(cut) {
        cut -= 1;
    }
    let suffix = if cut < raw.len() { "…" } else { "" };
    serde_json::to_string(&format!("{}{suffix}", &raw[..cut]))
        .unwrap_or_else(|_| "\"<unavailable>\"".to_string())
}

#[derive(Debug)]
#[allow(dead_code)]
pub enum LoopStreamItem<R> {
    Item(MultiTurnStreamItem<R>),
    ProviderAudit(crate::provider_audit::ProviderAuditObservation),
    ProviderAttemptStarted {
        turn: usize,
        attempt: u32,
        capture_scope: gents_protocol::rendered_request::CaptureScope,
    },
    /// A complete provider turn ready for atomic durable acceptance. The loop
    /// resumes into tool dispatch only after the consumer returns.
    ProviderTurnReady {
        turn: usize,
        attempt: u32,
        message: Message,
    },
    TurnRetracted {
        turn: usize,
        attempt: u32,
        /// Backoff the generator sleeps before the resample. Carried so the
        /// daemon can extend the next poll's liveness budget by it, exactly as
        /// for `AttemptFailed` — otherwise a retract backoff longer than the
        /// liveness timeout is misread as a dead stream (#648).
        backoff: std::time::Duration,
    },
    AttemptFailed {
        turn: usize,
        attempt: u32,
        error: InferenceError,
        will_retry: bool,
        backoff: std::time::Duration,
    },
    OutputObligationPending {
        reminder: Message,
    },
    /// Owned execution publishes admitted input before the first provider
    /// invocation. Tool continuation messages already have delivery owners.
    AuthoredInputReady {
        context: Option<Message>,
        prompt: Message,
    },
}

#[derive(Clone)]
pub struct LoopConfig {
    /// One backend/wire-selected provider projection shared by every budget
    /// decision in this completion loop and its nested compactor.
    pub provider_input_counter: Arc<crate::provider_input::ProviderInputCounter>,
    pub replay: LoopReplayInput,
    pub preamble: Option<String>,
    pub context_message: Option<Message>,
    pub temperature: Option<f64>,
    pub max_tokens: Option<u64>,
    /// One request-scoped ledger shared by the owned inference loop and every
    /// nested provider call it admits (notably compaction). `None` preserves
    /// the unbounded interactive behavior.
    pub aggregate_token_budget: Option<AggregateTokenBudget>,
    pub additional_params: Option<serde_json::Value>,
    pub structured_output: Option<StructuredOutputConfig>,
    pub tool_choice: Option<ToolChoice>,
    pub on_rendered_request: Option<RenderedRequestSink>,
    /// Provider-view compaction used between completion turns. The callback
    /// must durably create or verify its reduction fact before returning.
    pub turn_compactor: Option<TurnCompactor>,
    /// The one newest reduction fact that shapes the sticky provider
    /// projection. Empty on a fresh request.
    pub active_reduction_keys: Vec<String>,
    /// Every durable reduction for this request, including consumed facts that
    /// order the next identity but no longer shape the active provider view.
    pub reduction_chain_keys: Vec<String>,
    /// Turn index to resume at an unconsumed durable checkpoint.
    pub initial_turn_index: usize,
    pub context_window: usize,
    pub compaction_threshold: f64,
    pub retry_policy: CompletionRetryPolicy,
    /// Longest silence tolerated from an open provider stream, before its
    /// first item and between items. Expiry fails the attempt as transport
    /// through `retry_policy`. It runs only while the loop awaits provider
    /// output, never across admission queueing, tool dispatch or retry
    /// backoff. `None` waits for the request deadline.
    pub provider_idle_timeout: Option<std::time::Duration>,
    pub deadline: Option<DateTime<Utc>>,
    pub max_turns: usize,
    pub output_obligation_gate: Option<Arc<dyn OutputObligationCheck>>,
}
