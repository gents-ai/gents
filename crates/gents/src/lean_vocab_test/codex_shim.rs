use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanCodexShimProjectionCase {
    pub(crate) witness: String,
    pub(crate) lean_theorems: Vec<String>,
    pub(crate) request_state: String,
    pub(crate) response_status: Option<String>,
    pub(crate) local_interrupt_acked: bool,
    pub(crate) projected_phase: String,
    pub(crate) terminal: bool,
    pub(crate) effectively_terminal: bool,
    pub(crate) interruptible_request_state: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanCodexShimSubagentToolCase {
    pub(crate) witness: String,
    pub(crate) lean_theorems: Vec<String>,
    pub(crate) tool_name: String,
    pub(crate) projected_item_kind: String,
    pub(crate) collab_tool: Option<String>,
    pub(crate) reciprocal_link: bool,
    pub(crate) projection_settled: bool,
    pub(crate) link_settle_expired: bool,
    pub(crate) runtime_tool_status: Option<String>,
    pub(crate) projected_collab_status: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanCodexShimSubagentStatusCase {
    pub(crate) witness: String,
    pub(crate) lean_theorems: Vec<String>,
    pub(crate) request_state: String,
    pub(crate) response_status: Option<String>,
    pub(crate) projected_agent_status: String,
    pub(crate) terminal: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanCodexShimSubagentVisibilityCase {
    pub(crate) witness: String,
    pub(crate) lean_theorems: Vec<String>,
    pub(crate) authorized: bool,
    pub(crate) loaded: bool,
    pub(crate) projection_mode: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanCodexShimSubagentMetadataCase {
    pub(crate) witness: String,
    pub(crate) lean_theorems: Vec<String>,
    pub(crate) runtime_model: Option<String>,
    pub(crate) runtime_reasoning_effort: Option<String>,
    pub(crate) projected_model: Option<String>,
    pub(crate) projected_reasoning_effort: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanCodexShimSubagentListingCase {
    pub(crate) witness: String,
    pub(crate) lean_theorems: Vec<String>,
    pub(crate) source_kind: String,
    pub(crate) authorized: bool,
    pub(crate) listed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanCodexShimSubagentThreadShapeCase {
    pub(crate) witness: String,
    pub(crate) lean_theorems: Vec<String>,
    pub(crate) parent_thread_id: String,
    pub(crate) native_source_parent: Option<String>,
    pub(crate) legacy_top_level_parent: Option<String>,
    pub(crate) replay_stages: Vec<String>,
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
    pub(crate) response_status: Option<String>,
    pub(crate) conversation_status: String,
    pub(crate) projected_status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanCodexShimBehaviorSelectionCase {
    pub(crate) witness: String,
    pub(crate) lean_theorems: Vec<String>,
    pub(crate) root_behavior_id: String,
    pub(crate) thread_behavior_id: Option<String>,
    pub(crate) projected_behavior_id: String,
    pub(crate) root_model: String,
    pub(crate) projected_child_model: Option<String>,
    pub(crate) resolved_child_model: Option<String>,
    pub(crate) projected_model: String,
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
