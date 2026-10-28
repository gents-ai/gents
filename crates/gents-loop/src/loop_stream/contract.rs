use super::*;

use crate::claude_messages_body::{ReplayIssuer, ReplayTag, ReplayWire, ResolvedReplayEvidence};
use crate::compaction::CompactionError;
use crate::rendered_request::RenderedRequestError;
use crate::stream_processor::StreamProcessorError;

#[derive(Clone, Debug, PartialEq)]
pub struct TaggedMessage {
    pub message: Message,
    pub source: Option<ReplayTag>,
    pub physical_header: Option<String>,
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

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ReplayEvidenceViolation(pub String);

#[derive(Debug, thiserror::Error)]
pub enum ReplayEvidenceResolveError {
    #[error(transparent)]
    Violation(#[from] ReplayEvidenceViolation),
    #[error("loading canonical Claude replay evidence failed")]
    Storage {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

pub type ReplayEvidenceResolver = Arc<
    dyn Fn(
            Vec<ReplayTag>,
        ) -> Pin<
            Box<
                dyn Future<Output = Result<Vec<ReplayEvidenceRow>, ReplayEvidenceResolveError>>
                    + Send,
            >,
        > + Send
        + Sync,
>;

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

    pub fn record(&self, rows: Vec<ReplayEvidenceRow>) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.armed {
            state.rows = Some(rows);
        }
    }

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
    pub request_doc_id: Option<String>,
    pub issuer: Option<ReplayIssuer>,
    pub wire: Option<ReplayWire>,
    pub required: Vec<ReplayTag>,
    pub resolve: Option<ReplayEvidenceResolver>,
    pub estimate_seed: Option<ReplayEstimateSeed>,
}

impl LoopReplayInput {
    pub fn arm_estimate_seed(&self) -> EstimateSeedGuard {
        let seed = self.estimate_seed.clone();
        if let Some(seed) = &seed {
            seed.arm();
        }
        EstimateSeedGuard { seed }
    }
}

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

pub type RenderedRequestSink = Arc<
    dyn Fn(
            usize,
            u32,
            CompletionRequest,
            AssemblyTrace,
        ) -> Pin<Box<dyn Future<Output = Result<(), RenderedRequestError>> + Send>>
        + Send
        + Sync,
>;

#[derive(Clone, Debug)]
pub struct TurnCompactionRequest {
    pub messages: Vec<TaggedMessage>,
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

#[derive(Debug, thiserror::Error)]
pub enum TurnCompactionError {
    #[error(transparent)]
    Compaction(#[from] CompactionError),
    #[error(transparent)]
    Compactor(Box<dyn std::error::Error + Send + Sync>),
}

impl TurnCompactionError {
    pub fn aggregate_token_budget_exhaustion(&self) -> Option<String> {
        match self {
            Self::Compaction(error) => error
                .aggregate_token_budget_exhaustion()
                .map(ToString::to_string),
            Self::Compactor(error) => aggregate_token_budget_exhaustion_message(error.as_ref()),
        }
    }
}

pub type TurnCompactor = Arc<
    dyn Fn(
            TurnCompactionRequest,
        ) -> Pin<
            Box<dyn Future<Output = Result<TurnCompactionOutcome, TurnCompactionError>> + Send>,
        > + Send
        + Sync,
>;

pub type StructuredOutputValidator = Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>;

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
    Final {
        text: String,
    },
    ProviderAudit(crate::provider_audit::ProviderAuditObservation),
    ProviderAttemptStarted {
        turn: usize,
        attempt: u32,
        capture_scope: gents_protocol::rendered_request::CaptureScope,
    },
    ProviderTurnReady {
        turn: usize,
        attempt: u32,
        message: Message,
    },
    TurnRetracted {
        turn: usize,
        attempt: u32,
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
        reply: tokio::sync::oneshot::Sender<Result<FinishOrIntake, StreamProcessorError>>,
    },
    SteeringBoundary {
        reply: tokio::sync::oneshot::Sender<Result<Vec<FoldedPrompt>, StreamProcessorError>>,
    },
    AuthoredInputReady {
        context: Option<Message>,
        prompt: Message,
        folded: Vec<(String, Message)>,
        reply: tokio::sync::oneshot::Sender<Result<Vec<FoldedPrompt>, StreamProcessorError>>,
    },
}

#[derive(Clone)]
pub struct LoopConfig {
    pub provider_input_counter: Arc<crate::provider_input::ProviderInputCounter>,
    pub replay: LoopReplayInput,
    pub preamble: Option<String>,
    pub context_message: Option<Message>,
    pub temperature: Option<f64>,
    pub max_tokens: Option<u64>,
    pub aggregate_token_budget: Option<AggregateTokenBudget>,
    pub additional_params: Option<serde_json::Value>,
    pub structured_output: Option<StructuredOutputConfig>,
    pub tool_choice: Option<ToolChoice>,
    pub on_rendered_request: Option<RenderedRequestSink>,
    pub turn_compactor: Option<TurnCompactor>,
    pub active_reduction_keys: Vec<String>,
    pub reduction_chain_keys: Vec<String>,
    pub initial_turn_index: usize,
    pub resume_from_history: bool,
    pub context_window: usize,
    pub compaction_threshold: f64,
    pub retry_policy: CompletionRetryPolicy,
    pub provider_idle_timeout: Option<std::time::Duration>,
    pub deadline: Option<DateTime<Utc>>,
    pub max_turns: usize,
    pub output_obligation_gate: Option<Arc<dyn OutputObligationCheck>>,
    pub folded_prompts: Vec<FoldedPrompt>,
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
