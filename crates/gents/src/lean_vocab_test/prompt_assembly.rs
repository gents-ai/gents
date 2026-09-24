use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanCurrentInputHeader {
    pub(crate) request: String,
    pub(crate) kind: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanCurrentInputCase {
    pub(crate) name: String,
    pub(crate) current_request: String,
    pub(crate) headers: Vec<LeanCurrentInputHeader>,
    pub(crate) retained_indices: Vec<usize>,
}

/// One item inside a message, as emitted by `PromptAssembly.Content.Item`.
/// `value` is the text/reasoning index for `text`/`other`, and the tool-call id
/// for `call`.
#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub(crate) struct LeanPromptAssemblyItem {
    pub(crate) item: String,
    pub(crate) value: u64,
}

/// One provider-bound row: the abstract transcript row plus its content.
#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub(crate) struct LeanPromptAssemblyRow {
    pub(crate) role: String,
    pub(crate) kind: String,
    pub(crate) call_ids: Vec<u64>,
    pub(crate) content: Vec<LeanPromptAssemblyItem>,
}

/// `sanitize` applied to a suffix of the input, for split-stability.
#[derive(Debug, Deserialize, Clone)]
pub(crate) struct LeanPromptAssemblySplit {
    pub(crate) index: usize,
    pub(crate) expected: Vec<LeanPromptAssemblyRow>,
}

/// A sanitize witness from `PromptAssembly.Provider.sanitizeForProviderGlobal`.
/// `expected`, `expected_twice`, and every `splits` entry are computed by
/// running the Lean model, never written by hand.
#[derive(Debug, Deserialize, Clone)]
pub(crate) struct LeanPromptAssemblySanitizeCase {
    pub(crate) name: String,
    pub(crate) input: Vec<LeanPromptAssemblyRow>,
    pub(crate) expected: Vec<LeanPromptAssemblyRow>,
    pub(crate) expected_twice: Vec<LeanPromptAssemblyRow>,
    pub(crate) splits: Vec<LeanPromptAssemblySplit>,
}

/// The assembled layer order emitted from `PromptAssembly.assemble` (see
/// `Proofs/Conformance/ContractCases/PromptAssembly.lean`); `skill_count`
/// counts the actual skill selection, never raw references.
#[derive(Debug, Deserialize, Clone)]
pub(crate) struct LeanPromptAssemblyLayerCase {
    pub(crate) name: String,
    pub(crate) skill_count: usize,
    pub(crate) summary_count: usize,
    pub(crate) conversation_len: usize,
    pub(crate) slots: Vec<String>,
}

/// A tool-argument repair witness from `PromptAssembly.ToolArgs.repairArgs`.
#[derive(Debug, Deserialize, Clone)]
pub(crate) struct LeanPromptAssemblyRepairCase {
    pub(crate) name: String,
    pub(crate) input: String,
    pub(crate) expected: String,
    pub(crate) expected_twice: String,
    pub(crate) payload_only: bool,
}

/// A provider-input budget witness computed by `PromptAssembly.Budget`.
#[derive(Debug, Deserialize, Clone)]
pub(crate) struct LeanPromptAssemblyBudgetCase {
    pub(crate) name: String,
    pub(crate) context_window: usize,
    pub(crate) max_output_tokens: usize,
    pub(crate) threshold_basis_points: usize,
    pub(crate) configured_threshold_budget: usize,
    pub(crate) prompt_tokens: usize,
    pub(crate) request_tokens: usize,
    pub(crate) effective_input_budget: usize,
    pub(crate) effective_output_tokens: usize,
    pub(crate) should_compact: bool,
    pub(crate) provider_safe: bool,
    pub(crate) can_dispatch: bool,
}

/// A multi-turn provider-input budget trace computed by
/// `PromptAssembly.Budget`.
#[derive(Debug, Deserialize, Clone)]
pub(crate) struct LeanPromptAssemblyTurnBudgetCase {
    pub(crate) name: String,
    pub(crate) context_window: usize,
    pub(crate) max_output_tokens: usize,
    pub(crate) threshold_basis_points: usize,
    pub(crate) configured_threshold_budget: usize,
    pub(crate) effective_input_budget: usize,
    pub(crate) turn_input_tokens: Vec<usize>,
    pub(crate) turn_output_tokens: Vec<usize>,
    pub(crate) turn_should_compact: Vec<bool>,
    pub(crate) turn_can_dispatch: Vec<bool>,
}

/// A static-overhead-aware compaction retention target computed by
/// `PromptAssembly.Budget.compactionRetentionTarget`.
#[derive(Debug, Deserialize, Clone)]
pub(crate) struct LeanPromptAssemblyRetentionCase {
    pub(crate) name: String,
    pub(crate) configured_keep_recent: usize,
    pub(crate) effective_input_budget: usize,
    pub(crate) fixed_input: usize,
    pub(crate) available_input: usize,
    pub(crate) retention_target: usize,
    pub(crate) summary_max_output: usize,
    pub(crate) effective_summary_output: usize,
    pub(crate) rolling_summary_input_budget: usize,
}

/// A Claude `tool_use` map witness computed by
/// `PromptAssembly.ClaudeMap.mapTurn`.
#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub(crate) struct LeanPromptAssemblyClaudeMapCase {
    pub(crate) name: String,
    pub(crate) surface: Vec<String>,
    pub(crate) blocks: Vec<String>,
    pub(crate) outcome: String,
    pub(crate) ids: Vec<u64>,
}

/// A Claude Messages body witness computed by `ClaudeMap.systemBlocks` /
/// `splitSystem` / `toolsField`.
#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub(crate) struct LeanPromptAssemblyClaudeBodyCase {
    pub(crate) name: String,
    pub(crate) preamble: Option<String>,
    pub(crate) rows: Vec<String>,
    pub(crate) tools: Vec<String>,
    pub(crate) system: Vec<String>,
    pub(crate) tools_present: bool,
    pub(crate) supported_efforts: Option<Vec<String>>,
    pub(crate) effort: Option<String>,
    pub(crate) selected_effort: Option<String>,
}

/// A Claude Messages SSE witness computed by `ClaudeMap.runStream`.
#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub(crate) struct LeanPromptAssemblyClaudeStreamCase {
    pub(crate) name: String,
    pub(crate) surface: Vec<String>,
    pub(crate) events: Vec<String>,
    pub(crate) outcome: String,
    pub(crate) calls: Vec<String>,
}

/// Model-only Claude thinking SSE witness; both previews and sealed content
/// come from `ClaudeMap.runContentTrace`, not from a native parser oracle.
#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub(crate) struct LeanPromptAssemblyClaudeThinkingStreamCase {
    pub(crate) name: String,
    pub(crate) surface: Vec<String>,
    pub(crate) events: Vec<LeanClaudeStreamEvent>,
    pub(crate) outcome: String,
    pub(crate) steps: Vec<LeanClaudeContentStep>,
    pub(crate) content: Vec<LeanClaudeStreamBlock>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub(crate) struct LeanClaudeStreamEvent {
    pub(crate) kind: String,
    pub(crate) value: Option<String>,
    pub(crate) id: Option<u64>,
    pub(crate) name: Option<String>,
    pub(crate) input: Option<String>,
    pub(crate) index: Option<u64>,
    pub(crate) fragment: Option<String>,
    pub(crate) data: Option<String>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub(crate) struct LeanClaudeReasoningPart {
    pub(crate) kind: String,
    pub(crate) payload: String,
    pub(crate) signature: Option<String>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub(crate) struct LeanClaudeStreamBlock {
    pub(crate) kind: String,
    pub(crate) value: Option<String>,
    pub(crate) parts: Option<Vec<LeanClaudeReasoningPart>>,
    pub(crate) id: Option<u64>,
    pub(crate) name: Option<String>,
    pub(crate) arguments: Option<String>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub(crate) struct LeanClaudeContentStep {
    pub(crate) provisional_thinking: Option<String>,
    pub(crate) sealed: Vec<LeanClaudeStreamBlock>,
}

/// Model-only continuation replay witness from reconstructed canonical native blocks.
#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub(crate) struct LeanPromptAssemblyClaudeReplayCase {
    pub(crate) name: String,
    pub(crate) blocks: Vec<LeanClaudeReplayInputBlock>,
    pub(crate) outcome: String,
    pub(crate) replay: Vec<LeanClaudeReplayBlock>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub(crate) struct LeanClaudeReplayInputBlock {
    pub(crate) kind: String,
    pub(crate) payload: Option<Vec<u8>>,
    pub(crate) parts: Option<Vec<LeanClaudeReplayInputPart>>,
    pub(crate) id: Option<String>,
    pub(crate) doc_id: Option<u64>,
    pub(crate) call_id: Option<String>,
    pub(crate) name: Option<String>,
    pub(crate) arguments: Option<Vec<u8>>,
    pub(crate) signature: Option<String>,
    pub(crate) additional_params: Option<String>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub(crate) struct LeanClaudeReplayInputPart {
    pub(crate) kind: String,
    pub(crate) payload: Vec<u8>,
    pub(crate) signature: Option<String>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub(crate) struct LeanClaudeReplayBlock {
    pub(crate) kind: String,
    pub(crate) payload: Option<Vec<u8>>,
    pub(crate) signature: Option<String>,
    pub(crate) call_id: Option<String>,
    pub(crate) name: Option<String>,
    pub(crate) arguments: Option<Vec<u8>>,
}

/// A request-wide token-ledger witness computed by
/// `PromptAssembly.AggregateBudget`.
#[derive(Debug, Deserialize, Clone)]
pub(crate) struct LeanAggregateTokenBudgetCase {
    pub(crate) request_doc_id: String,
    pub(crate) prior_request_doc_ids: Vec<String>,
    pub(crate) prior_call_kinds: Vec<String>,
    pub(crate) name: String,
    pub(crate) limit: u64,
    pub(crate) used: u64,
    pub(crate) prior_prompt_tokens: Vec<u64>,
    pub(crate) prior_completion_tokens: Vec<u64>,
    pub(crate) input_tokens: u64,
    pub(crate) configured_max_output_tokens: u64,
    pub(crate) reported_input_tokens: u64,
    pub(crate) reported_output_tokens: u64,
    pub(crate) reported_total_tokens: u64,
    pub(crate) usage_present: bool,
    pub(crate) terminal_valid: bool,
    pub(crate) effective_output_tokens: u64,
    pub(crate) can_dispatch: bool,
    pub(crate) charged_tokens: u64,
    pub(crate) charge_result: String,
    pub(crate) next_used: Option<u64>,
    pub(crate) post_charge_action: String,
}
