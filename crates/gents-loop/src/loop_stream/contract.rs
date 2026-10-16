use super::*;

use crate::claude_messages_body::{ReplayIssuer, ReplayTag, ReplayWire, ResolvedReplayEvidence};

/// One native row and its independently established canonical provider source.
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

/// A deterministic violation of canonical replay provenance; storage failures
/// must not be relabeled as this.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ReplayEvidenceViolation(pub String);

pub type ReplayEvidenceResolver = Arc<
    dyn Fn(
            Vec<ReplayTag>,
        ) -> Pin<Box<dyn Future<Output = anyhow::Result<Vec<ReplayEvidenceRow>>> + Send>>
        + Send
        + Sync,
>;

/// One-shot carrier from an admission estimate's replay evidence resolution to
/// the immediately following dispatch lookup.
///
/// Native premise the model does not cover: the slot holds ONLY the admission
/// estimate's resolution, is consumed exactly once by the immediately
/// following dispatch whose tags it covers, and every later assembly re-reads
/// the store. A capture replicating between the estimate and that first
/// dispatch is observed no earlier than the next resolution; the estimate's
/// token count already races this way.
#[derive(Clone, Default)]
pub struct ReplayEstimateSeed {
    state: std::sync::Arc<std::sync::Mutex<ReplayEstimateSeedState>>,
}

#[derive(Default)]
struct ReplayEstimateSeedState {
    armed: bool,
    rows: Option<Vec<ReplayEvidenceRow>>,
}

impl ReplayEstimateSeed {
    pub(crate) fn arm(&self) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .armed = true;
    }

    pub(crate) fn disarm(&self) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .armed = false;
    }

    /// Record one resolution while an admission estimate holds the seed armed;
    /// dispatch and later resolutions record nothing.
    pub fn record(&self, rows: Vec<ReplayEvidenceRow>) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.armed {
            state.rows = Some(rows);
        }
    }

    /// Take the recorded rows. Taken once: the next lookup re-reads the store.
    pub(crate) fn take(&self) -> Option<Vec<ReplayEvidenceRow>> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .rows
            .take()
    }
}

#[derive(Clone, Default)]
pub struct LoopReplayInput {
    /// Present only for a persisted request with a real canonical owner.
    pub request_doc_id: Option<String>,
    /// Exact built-client route, absent when endpoint identity is unproven.
    pub issuer: Option<ReplayIssuer>,
    /// Wire selected by the running loop's actual provider profile.
    pub wire: Option<ReplayWire>,
    /// The pending tool round's sources; a reduction must not summarize them.
    pub required: Vec<ReplayTag>,
    pub resolve: Option<ReplayEvidenceResolver>,
    /// Carries the admission estimate's resolution to the first dispatch.
    pub estimate_seed: Option<ReplayEstimateSeed>,
}

impl LoopReplayInput {
    /// Arm [`Self::estimate_seed`] so the resolutions run while the returned
    /// guard is held are recorded for the immediately following dispatch.
    /// Dropping the guard ends the bracket on every exit path, including
    /// unwinds and cancellation.
    pub fn arm_estimate_seed(&self) -> EstimateSeedGuard {
        let seed = self.estimate_seed.clone();
        if let Some(seed) = &seed {
            seed.arm();
        }
        EstimateSeedGuard { seed }
    }
}

/// Ends the estimate-seed bracket opened by
/// [`LoopReplayInput::arm_estimate_seed`] when dropped.
pub struct EstimateSeedGuard {
    seed: Option<ReplayEstimateSeed>,
}

impl Drop for EstimateSeedGuard {
    fn drop(&mut self) {
        if let Some(seed) = &self.seed {
            seed.disarm();
        }
    }
}

/// `(turn_index, attempt, request, assembly_trace)`. The trace rides alongside
/// the request because the assembled `CompletionRequest` is the *output* of
/// prompt assembly and cannot explain its own inputs; see
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

/// A typed-output contract carried through the owned completion loop. Rig owns
/// the provider schema transport; gents keeps loop ownership so schema
/// validation participates in its modelled retract-and-resample lifecycle
/// instead of bypassing persistence and hooks through `rig::Agent::prompt_typed`.
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
pub enum LoopStreamItem {
    Text(String),
    Reasoning(gents_protocol::message::Reasoning),
    ReasoningDelta {
        id: Option<String>,
        reasoning: String,
    },
    ToolCall {
        tool_call: ToolCall,
        internal_call_id: String,
    },
    ToolResult {
        tool_result: ToolResult,
        internal_call_id: String,
    },
    /// The final turn's text; the loop ends after it.
    Final {
        text: String,
    },
    ProviderAudit(crate::provider_audit::ProviderAuditObservation),
    ProviderAttemptStarted {
        turn: usize,
        attempt: u32,
        capture_scope: gents_protocol::rendered_request::CaptureScope,
    },
    /// A complete provider turn ready for atomic durable acceptance; tool
    /// dispatch resumes only after the consumer returns.
    ProviderTurnReady {
        turn: usize,
        attempt: u32,
        message: Message,
    },
    TurnRetracted {
        turn: usize,
        attempt: u32,
        /// Carried so the daemon can extend the next poll's liveness budget by
        /// it, as for `AttemptFailed` (#648).
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
    FinishOrIntake {
        text: String,
        reply: tokio::sync::oneshot::Sender<anyhow::Result<FinishOrIntake>>,
    },
    SteeringBoundary {
        reply: tokio::sync::oneshot::Sender<anyhow::Result<Vec<FoldedPrompt>>>,
    },
    /// Owned execution publishes admitted input before the first provider
    /// invocation.
    AuthoredInputReady {
        context: Option<Message>,
        prompt: Message,
        /// Folded messages after the prompt, each with its authored key.
        folded: Vec<(String, Message)>,
        reply: tokio::sync::oneshot::Sender<anyhow::Result<Vec<FoldedPrompt>>>,
    },
}

#[derive(Clone)]
pub struct LoopConfig {
    /// One backend/wire-selected provider projection shared by every budget
    /// decision in this loop and its nested compactor.
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
    /// The entry prompt is already durable; do not publish it as new authored input.
    pub resume_from_history: bool,
    pub context_window: usize,
    pub compaction_threshold: f64,
    pub retry_policy: CompletionRetryPolicy,
    /// Longest silence tolerated from an open provider stream, before its
    /// first item and between items. Runs only while the loop awaits provider
    /// output, never across admission queueing, tool dispatch or retry
    /// backoff. `None` waits for the request deadline.
    pub provider_idle_timeout: Option<std::time::Duration>,
    pub deadline: Option<DateTime<Utc>>,
    pub max_turns: usize,
    pub output_obligation_gate: Option<Arc<dyn OutputObligationCheck>>,
    /// Admitted messages folded into this request's turn, in queue order
    /// (Lean `SessionQueue.TurnInput`). Each is a distinct user message sent
    /// after the prompt and published under its own authored key.
    pub folded_prompts: Vec<FoldedPrompt>,
    /// The request's admitted input, published before the first provider
    /// projection is built. A restored checkpoint carries its own provider
    /// view, so the authored input travels independently of it.
    pub authored_input: Option<AuthoredInput>,
}

#[derive(Clone, Debug)]
pub struct AuthoredInput {
    pub context: Option<Message>,
    pub prompt: Message,
    pub folded: Vec<FoldedPrompt>,
}

#[derive(Clone, Debug)]
pub struct FoldedPrompt {
    pub key: String,
    pub message: Message,
}

#[derive(Debug)]
pub enum FinishOrIntake {
    Finished,
    Continue(Vec<FoldedPrompt>),
}

pub struct SteeringSnapshot {
    pub pending_request_doc_ids: Vec<String>,
    pub inputs: Vec<FoldedPrompt>,
}
