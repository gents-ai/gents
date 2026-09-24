use serde::Deserialize;

use super::LeanCanonicalCoordinate;

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

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) enum LeanAssistantOrderMode {
    Grouped,
    NativePreserved,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanPromptAssemblyAssistantOrderCase {
    pub(crate) name: String,
    pub(crate) order_mode: LeanAssistantOrderMode,
    pub(crate) input: Vec<LeanPromptAssemblyItem>,
    pub(crate) expected: Vec<LeanPromptAssemblyItem>,
    pub(crate) expected_twice: Vec<LeanPromptAssemblyItem>,
}

/// One provider-bound row: the abstract transcript row plus its content.
#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub(crate) struct LeanPromptAssemblyRow {
    pub(crate) role: String,
    pub(crate) kind: String,
    pub(crate) call_ids: Vec<u64>,
    pub(crate) content: Vec<LeanPromptAssemblyItem>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanPromptAssemblyModeSanitizeCase {
    pub(crate) name: String,
    pub(crate) order_mode: LeanAssistantOrderMode,
    pub(crate) input: Vec<LeanPromptAssemblyRow>,
    pub(crate) expected: Vec<LeanPromptAssemblyRow>,
    pub(crate) expected_twice: Vec<LeanPromptAssemblyRow>,
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
pub(crate) struct LeanPromptAssemblyClaudeWireStartCase {
    pub(crate) name: String,
    pub(crate) start: LeanClaudeWireThinkingStart,
    pub(crate) later: Vec<LeanClaudeStreamEvent>,
    pub(crate) expected: LeanClaudeWireExpected,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub(crate) struct LeanClaudeWireThinkingStart {
    pub(crate) index: u64,
    pub(crate) thinking: String,
    pub(crate) signature_present: bool,
    pub(crate) signature: serde_json::Value,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub(crate) struct LeanClaudeWireExpected {
    pub(crate) kind: String,
    pub(crate) error: Option<String>,
    pub(crate) steps: Option<Vec<LeanClaudeContentStep>>,
    pub(crate) content: Option<Vec<LeanClaudeStreamBlock>>,
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
#[serde(deny_unknown_fields)]
pub(crate) struct LeanPromptAssemblyClaudeNarrowingCase {
    pub(crate) name: String,
    pub(crate) rows: Vec<LeanClaudeNarrowingInput>,
    pub(crate) carriers: Vec<String>,
    pub(crate) outcome: String,
    pub(crate) replay: Vec<Vec<LeanClaudeReplayBlock>>,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) enum LeanClaudeReplayUsage {
    Historical,
    RequiredCurrent,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) enum LeanClaudeReplayOrigin {
    ClaudeSubscription,
    Foreign,
    Missing,
    Ambiguous,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanClaudeNarrowingInput {
    pub(crate) usage: LeanClaudeReplayUsage,
    pub(crate) origin: LeanClaudeReplayOrigin,
    pub(crate) expected_reasoning: Option<Vec<LeanClaudeReasoningWitness>>,
    pub(crate) blocks: Vec<LeanClaudeReplayInputBlock>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanClaudeReasoningWitness {
    pub(crate) block_index: usize,
    pub(crate) parts: Vec<LeanClaudeReplayInputPart>,
}

/// Selected assistant occurrences, not the complete native request. The split
/// is measured in this projection; user and tool-result rows stay with their
/// native assembly owner.
#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanPromptAssemblyClaudeCheckpointCase {
    pub(crate) name: String,
    pub(crate) required: Vec<LeanCanonicalCoordinate>,
    pub(crate) rows: Vec<LeanClaudeTaggedReplayRow>,
    pub(crate) split: usize,
    pub(crate) carrier_ids: Vec<String>,
    pub(crate) resolutions: Vec<LeanClaudeCheckpointResolution>,
    pub(crate) outcome: String,
    pub(crate) prefix_rows: Vec<LeanClaudeTaggedReplayRow>,
    pub(crate) retained: Vec<LeanClaudeTaggedReplayRow>,
    pub(crate) replay: Vec<Vec<LeanClaudeReplayBlock>>,
}

/// Pair-safe protected prefix selected by the existing compaction and Claude
/// replay owners before any summary completion is attempted.
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanProtectedReplayCompactionCase {
    pub(crate) name: String,
    pub(crate) message_count: usize,
    pub(crate) raw_index: usize,
    pub(crate) max_prefix: usize,
    pub(crate) required: Vec<LeanCanonicalCoordinate>,
    pub(crate) rows: Vec<LeanClaudeTaggedReplayRow>,
    pub(crate) selected_split: Option<usize>,
    pub(crate) outcome: String,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanClaudeTaggedReplayRow {
    pub(crate) source: Option<LeanCanonicalCoordinate>,
    pub(crate) blocks: Vec<LeanClaudeReplayInputBlock>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanClaudeCheckpointResolution {
    pub(crate) tag: LeanCanonicalCoordinate,
    pub(crate) origin: LeanClaudeReplayOrigin,
    pub(crate) expected_reasoning: Vec<LeanClaudeReasoningWitness>,
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
    pub(crate) id: Option<String>,
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
