use super::*;

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
    pub messages: Vec<Message>,
    pub admission: crate::compaction::ReductionAdmission,
    pub turn_index: usize,
    pub prior_reduction_keys: Vec<String>,
}

#[derive(Clone, Debug)]
pub enum TurnCompactionOutcome {
    ProviderViewRepaired {
        messages: Vec<Message>,
    },
    Reduced {
        messages: Vec<Message>,
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
}

#[derive(Clone)]
pub struct LoopConfig {
    /// One backend/wire-selected provider projection shared by every budget
    /// decision in this completion loop and its nested compactor.
    pub provider_input_counter: Arc<crate::provider_input::ProviderInputCounter>,
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
    pub deadline: Option<DateTime<Utc>>,
    pub max_turns: usize,
    pub output_obligation_gate: Option<Arc<dyn OutputObligationCheck>>,
}
