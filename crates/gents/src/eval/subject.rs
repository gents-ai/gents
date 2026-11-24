use serde::{Deserialize, Serialize};

/// The subject agent's own work in one trial (Lean
/// `Runtime.EvalUsage.subjectUsage`), beside the home-wide [`super::TrialUsage`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubjectUsage {
    pub requests: u64,
    pub first_request: Option<FirstRequest>,
    pub turns: CallTotals,
    pub compaction: CallTotals,
    pub tool_calls: ToolCallSplit,
    pub wall_ms: Option<u64>,
    /// From each call's `started_at`, which is its slot admission, not its
    /// dispatch to the provider.
    pub model_ms: Option<u64>,
    pub tool_ms: Option<u64>,
    /// Over the turns that streamed a token (Lean `Runtime.EvalUsage.firstTokenMs`).
    pub median_first_token_ms: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirstRequest {
    pub prompt_tokens: Option<u64>,
    pub estimate: Option<PromptEstimate>,
    pub first_token_ms: Option<u64>,
}

/// A rendered request's `context_accounting_json` components: the
/// estimator's own counts before calibration. The system prompt is part of
/// `messages`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptEstimate {
    pub estimator: String,
    pub messages: u64,
    pub documents: u64,
    pub tool_schemas: u64,
    pub additional_parameters: u64,
    pub output_schema: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallTotals {
    pub calls: u64,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub unreported: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallSplit {
    pub direct: ToolCount,
    pub script: ToolCount,
    pub inside_script: ToolCount,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCount {
    pub calls: u64,
    pub failed: u64,
}
