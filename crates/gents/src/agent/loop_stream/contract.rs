use super::*;

/// `(turn_index, attempt, request, assembly_trace)`.
///
/// The trace rides alongside the request because the assembled
/// `CompletionRequest` is the *output* of prompt assembly and cannot explain
/// its own inputs: the provider-assigned assistant message ids, the exact
/// threaded tool-result content, the post-compaction message list, and which
/// builder produced it are all in-memory facts that die with the loop. See
/// `crate::rendered_request::AssemblyTrace`.
pub(crate) type RenderedRequestSink = Arc<
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
pub(crate) struct TurnCompactionRequest {
    pub(crate) messages: Vec<Message>,
    pub(crate) estimated_input_tokens: usize,
    pub(crate) turn_index: usize,
    pub(crate) prior_reduction_keys: Vec<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct TurnCompactionOutcome {
    pub(crate) messages: Vec<Message>,
    pub(crate) reduction_key: String,
}

pub(crate) type TurnCompactor = Arc<
    dyn Fn(
            TurnCompactionRequest,
        ) -> Pin<Box<dyn Future<Output = anyhow::Result<TurnCompactionOutcome>> + Send>>
        + Send
        + Sync,
>;

/// A typed-output contract carried through the owned completion loop.
///
/// Rig owns the provider schema transport. Gents keeps ownership of the loop
/// so schema validation participates in its deadline-aware, formally modelled
/// retract-and-resample lifecycle instead of bypassing persistence and hooks
/// through `rig::Agent::prompt_typed`.
#[derive(Clone)]
pub(crate) struct StructuredOutputConfig {
    pub(super) schema: schemars::Schema,
    pub(super) validate: Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>,
}

impl StructuredOutputConfig {
    pub(crate) fn for_type<T>() -> Self
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
pub(crate) enum LoopStreamItem<R> {
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
pub(crate) struct LoopConfig {
    /// One backend/wire-selected provider projection shared by every budget
    /// decision in this completion loop and its nested compactor.
    pub(crate) provider_input_counter: Arc<crate::provider_input::ProviderInputCounter>,
    pub(crate) preamble: Option<String>,
    pub(crate) context_message: Option<Message>,
    pub(crate) temperature: Option<f64>,
    pub(crate) max_tokens: Option<u64>,
    /// One request-scoped ledger shared by the owned inference loop and every
    /// nested provider call it admits (notably compaction). `None` preserves
    /// the unbounded interactive behavior.
    pub(crate) aggregate_token_budget: Option<AggregateTokenBudget>,
    pub(crate) additional_params: Option<serde_json::Value>,
    pub(crate) structured_output: Option<StructuredOutputConfig>,
    pub(crate) tool_choice: Option<ToolChoice>,
    pub(crate) on_rendered_request: Option<RenderedRequestSink>,
    /// Provider-view compaction used between completion turns. The callback
    /// must durably create or verify its reduction fact before returning.
    pub(crate) turn_compactor: Option<TurnCompactor>,
    /// The one newest reduction fact that shapes the sticky provider
    /// projection. Empty on a fresh request.
    pub(crate) active_reduction_keys: Vec<String>,
    /// Every durable reduction for this request, including consumed facts that
    /// order the next identity but no longer shape the active provider view.
    pub(crate) reduction_chain_keys: Vec<String>,
    /// Turn index to resume at an unconsumed durable checkpoint.
    pub(crate) initial_turn_index: usize,
    pub(crate) context_window: usize,
    pub(crate) compaction_threshold: f64,
    pub(crate) retry_policy: CompletionRetryPolicy,
    pub(crate) deadline: Option<DateTime<Utc>>,
    pub(crate) max_turns: usize,
    pub(crate) output_obligation_gate: Option<OutputObligationGate>,
}
