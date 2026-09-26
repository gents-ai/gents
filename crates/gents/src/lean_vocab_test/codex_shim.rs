use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanCodexShimProjectionCase {
    pub(crate) witness: String,
    pub(crate) lean_theorems: Vec<String>,
    pub(crate) request_state: String,
    pub(crate) is_superseded: bool,
    pub(crate) local_interrupt_acked: bool,
    pub(crate) projected_phase: String,
    pub(crate) terminal: bool,
    pub(crate) effectively_terminal: bool,
    pub(crate) interruptible_request_state: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanCodexShimReasoningProjectionCase {
    pub(crate) witness: String,
    pub(crate) lean_theorems: Vec<String>,
    pub(crate) item_open: bool,
    pub(crate) item_completed: bool,
    pub(crate) cursor_primed: bool,
    pub(crate) streamed_text: Option<String>,
    pub(crate) live_delta: Option<String>,
    pub(crate) durable_text: Option<String>,
    pub(crate) terminal: bool,
    pub(crate) projected_events: Vec<String>,
    pub(crate) projected_delta: Option<String>,
    pub(crate) completed_text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanCodexShimThreadStatusCase {
    pub(crate) witness: String,
    pub(crate) lean_theorems: Vec<String>,
    pub(crate) request_state: Option<String>,
    pub(crate) projected_status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanCodexShimBehaviorSelectionCase {
    pub(crate) witness: String,
    pub(crate) lean_theorems: Vec<String>,
    pub(crate) root_behavior_id: String,
    pub(crate) thread_behavior_id: Option<String>,
    pub(crate) projected_behavior_id: String,
    pub(crate) selected_owner: String,
    pub(crate) actual_owner: String,
    pub(crate) actual_behavior: String,
    pub(crate) resolved_model: Option<String>,
    pub(crate) projected_model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanCodexShimToolMetadataCase {
    pub(crate) witness: String,
    pub(crate) lean_theorems: Vec<String>,
    pub(crate) fallback_server: String,
    pub(crate) selected_server: Option<String>,
    pub(crate) fallback_tool: String,
    pub(crate) selected_tool: Option<String>,
    pub(crate) denial_reason: Option<String>,
    pub(crate) cancel_cause: Option<String>,
    pub(crate) failure_class: Option<String>,
    pub(crate) result_fallback: Option<String>,
    pub(crate) latency_ms: Option<usize>,
    pub(crate) started_at_ms: Option<usize>,
    pub(crate) completed_at_ms: Option<usize>,
    pub(crate) persisted_event_at_ms: Option<usize>,
    pub(crate) observed_at_ms: usize,
    pub(crate) projected_server: String,
    pub(crate) projected_tool: String,
    pub(crate) projected_failure: Option<String>,
    pub(crate) projected_duration_ms: Option<usize>,
    pub(crate) projected_event_at_ms: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanCodexShimContextUsageCase {
    pub(crate) witness: String,
    pub(crate) lean_theorems: Vec<String>,
    pub(crate) cumulative_input: usize,
    pub(crate) cumulative_output: usize,
    pub(crate) latest_prompt: usize,
    pub(crate) latest_completion: usize,
    pub(crate) model_window: usize,
    pub(crate) total_tokens: usize,
    pub(crate) current_context_tokens: usize,
    pub(crate) remaining_tokens: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanCodexShimCompactionProjectionCase {
    pub(crate) witness: String,
    pub(crate) lean_theorems: Vec<String>,
    pub(crate) previous_call_state: Option<String>,
    pub(crate) call_state: String,
    pub(crate) projected_events: Vec<String>,
    pub(crate) claims_compacted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanCodexShimTurnLifecycleCase {
    pub(crate) witness: String,
    pub(crate) lean_theorems: Vec<String>,
    pub(crate) action: String,
    pub(crate) pre_phase: String,
    pub(crate) post_phase: String,
    pub(crate) pre_lex_ord: usize,
    pub(crate) post_lex_ord: usize,
    pub(crate) monotonic: bool,
}

/// Binding vectors for the runnable-gated Codex shim (#699).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanCodexShimBindingCase {
    pub(crate) witness: String,
    pub(crate) lean_theorems: Vec<String>,
    pub(crate) pre_state: String,
    pub(crate) unbound_reason: Option<String>,
    pub(crate) bound_behavior_runnable: bool,
    pub(crate) host_can_listen: bool,
    pub(crate) post_state: String,
    pub(crate) post_unbound_reason: Option<String>,
    pub(crate) requires_restart: bool,
}
