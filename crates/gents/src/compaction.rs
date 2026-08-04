use std::sync::Arc;

use crate::llm::message::Message;
use anyhow::Result;
use rig::completion::CompletionModel;

mod history;
mod summary;
#[cfg(test)]
mod tests;

use history::{extract_file_activity, pretruncate_tool_results, split_messages_for_summary};
use summary::{
    bounded_error_preview, compaction_prompt, compaction_request_prompt, dedupe_paths,
    format_summary, parse_summary_response,
};

#[derive(Debug, Clone)]
pub struct CompactionOptions {
    pub threshold: f64,
    pub tool_result_max_chars: usize,
    pub keep_recent_tokens: usize,
    pub strategy: CompactionStrategy,
    /// The caller has already established that the complete provider input is
    /// over budget. Skip the history-only threshold recheck and summarize.
    pub force_summarize: bool,
    /// The claimed deadline of the request this compaction serves. The
    /// compactor's stored config is daemon-lifetime and carries no deadline,
    /// so this is the only path by which the internal retry ladder's
    /// deadline fail-fast can engage (#1016); `None` leaves recovery bounded
    /// only by the ladder itself.
    pub deadline: Option<chrono::DateTime<chrono::Utc>>,
}

impl Default for CompactionOptions {
    fn default() -> Self {
        Self {
            threshold: 0.75,
            tool_result_max_chars: 2000,
            keep_recent_tokens: 20000,
            strategy: CompactionStrategy::StripThenSummarize,
            force_summarize: false,
            deadline: None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum CompactionStrategy {
    StripToolResults,
    Summarize,
    StripThenSummarize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileActivity {
    pub files_read: Vec<String>,
    pub files_modified: Vec<String>,
}

impl FileActivity {
    pub fn is_empty(&self) -> bool {
        self.files_read.is_empty() && self.files_modified.is_empty()
    }
}

#[derive(Debug, Clone)]
pub struct CompactionResult {
    pub messages: Vec<Message>,
    pub summary: Option<String>,
    pub original_token_estimate: usize,
    pub compacted_token_estimate: usize,
    pub files_read: Vec<String>,
    pub files_modified: Vec<String>,
    pub messages_compacted: u32,
    pub compaction_count: u32,
}

pub trait Compactor: Send + Sync {
    fn compact(
        &self,
        messages: Vec<Message>,
        context_window: usize,
        options: &CompactionOptions,
    ) -> impl std::future::Future<Output = Result<CompactionResult>> + Send;
}

#[derive(Clone)]
pub struct DefraCompactor<M: CompletionModel> {
    model: Arc<M>,
    config: crate::agent::loop_stream::LoopConfig,
}

impl<M: CompletionModel> DefraCompactor<M> {
    pub(crate) fn new(model: Arc<M>, mut config: crate::agent::loop_stream::LoopConfig) -> Self {
        // Compaction is an internal, non-persisting sub-completion, not a user
        // execution origin; it must not inherit the parent's retry ladder (which
        // for scheduled origins is a deadline-less 5s/30s/120s backoff that would
        // block inline compaction for minutes) (#648). But it has no caller-level
        // retry either, so zero recovery made one empty provider turn abort the
        // whole user request: use the bounded immediate internal budget (#1016).
        config.retry_policy =
            crate::agent::completion_retry::CompletionRetryPolicy::internal_immediate();
        Self { model, config }
    }
}

impl<M: CompletionModel + 'static> Compactor for DefraCompactor<M> {
    async fn compact(
        &self,
        messages: Vec<Message>,
        context_window: usize,
        options: &CompactionOptions,
    ) -> Result<CompactionResult> {
        let original_token_estimate = estimate_message_tokens(&messages);

        // Normalize to the canonical provider view so `messages_compacted`
        // indexes the same list `drop_compacted_prefix` will later index,
        // whoever the caller is. Idempotent, so this is a no-op when the caller
        // already passed a provider view — which the daemon always does.
        //
        // This makes `CompactionStrategy::Summarize` and `StripThenSummarize`
        // behave identically. They already did on the daemon path, which strips
        // unconditionally before calling here; the variant is retained for
        // config compatibility.
        let row_count = messages.len();
        let (stripped_messages, stripped_activity) = provider_view(messages);

        // `messages_compacted` is an *index* into this list, so normalization
        // removing rows means the caller did not hand us a provider view and any
        // count taken here would be measured in a shifted space — silently
        // dropping rows the reader never summarized. Refuse loudly instead.
        //
        // For a provider view this is unreachable
        // (`Compaction.providerView_idempotent`). It is a real guard for legacy
        // compaction entries whose boundary predates the pair-safe splitter, and
        // for any future caller passing a raw transcript.
        let normalization_removed_rows = stripped_messages.len() != row_count;
        if normalization_removed_rows {
            tracing::warn!(
                row_count,
                normalized_count = stripped_messages.len(),
                "compaction skipped: input was not a provider view, so a compacted-prefix \
                 count taken here would not name the rows the next request drops"
            );
        }

        let stripped_token_estimate = estimate_message_tokens(&stripped_messages);
        // `normalization_removed_rows` stays the outermost refusal: it means any
        // count taken here would be measured in a shifted space, which
        // `force_summarize` must not override — the caller established that the
        // input is over budget, not that the row indices are trustworthy.
        if normalization_removed_rows
            || matches!(options.strategy, CompactionStrategy::StripToolResults)
            || (!options.force_summarize
                && !needs_compaction(&stripped_messages, context_window, options.threshold))
        {
            return Ok(CompactionResult {
                messages: stripped_messages,
                summary: None,
                original_token_estimate,
                compacted_token_estimate: stripped_token_estimate,
                files_read: stripped_activity.files_read,
                files_modified: stripped_activity.files_modified,
                messages_compacted: 0,
                compaction_count: 0,
            });
        }

        let (old_messages, recent_messages) =
            split_messages_for_summary(stripped_messages.clone(), options.keep_recent_tokens);
        if old_messages.is_empty() {
            return Ok(CompactionResult {
                messages: stripped_messages,
                summary: None,
                original_token_estimate,
                compacted_token_estimate: stripped_token_estimate,
                files_read: stripped_activity.files_read,
                files_modified: stripped_activity.files_modified,
                messages_compacted: 0,
                compaction_count: 0,
            });
        }

        let old_activity = extract_file_activity(&old_messages);
        let prepared_history =
            pretruncate_tool_results(old_messages.clone(), options.tool_result_max_chars);
        // The transcript is untrusted source material. Put the summarization
        // contract in the system layer so an unfinished user/tool workflow in
        // `prepared_history` cannot outrank it and turn the internal completion
        // into a continuation of the old task. The final user message is
        // deliberately neutral; it carries no executable transcript content.
        let mut summary_config = self.config.clone();
        summary_config.preamble = Some(compaction_prompt().to_string());
        summary_config.context_message = None;
        summary_config.tool_choice = None;
        summary_config.turn_compactor = None;
        summary_config.max_turns = 0;
        // Both deadlines are hard stops when present; recovery must respect
        // the earlier one.
        summary_config.deadline = match (options.deadline, summary_config.deadline) {
            (Some(from_options), Some(from_config)) => Some(from_options.min(from_config)),
            (from_options, from_config) => from_options.or(from_config),
        };
        let raw_summary = crate::agent::loop_stream::run_loop_to_text(
            (*self.model).clone(),
            None,
            crate::llm::message::Message::user(compaction_request_prompt()),
            prepared_history.clone(),
            std::sync::Arc::new(Vec::new()),
            summary_config,
        )
        .await
        .map_err(|error| {
            anyhow::anyhow!(
                "compaction summary inference failed: {}",
                bounded_error_preview(&format!("{error}"))
            )
        })?;
        let parsed_summary = parse_summary_response(&raw_summary)?;

        let mut files_read = old_activity.files_read;
        files_read.extend(parsed_summary.files_read);
        dedupe_paths(&mut files_read);

        let mut files_modified = old_activity.files_modified;
        files_modified.extend(parsed_summary.files_modified);
        dedupe_paths(&mut files_modified);

        let summary = format_summary(
            &parsed_summary.summary,
            &files_read,
            &files_modified,
            &parsed_summary.key_decisions,
            &parsed_summary.pending_questions,
        );
        let compacted_token_estimate =
            estimate_message_tokens(&recent_messages) + estimate_tokens(&summary);

        Ok(CompactionResult {
            messages: recent_messages,
            summary: Some(summary),
            original_token_estimate,
            compacted_token_estimate,
            files_read,
            files_modified,
            messages_compacted: old_messages.len() as u32,
            compaction_count: 1,
        })
    }
}

pub fn estimate_tokens(text: &str) -> usize {
    text.len() / 4
}

pub fn estimate_message_tokens(messages: &[Message]) -> usize {
    let serialized = serde_json::to_string(messages).unwrap_or_default();
    estimate_tokens(&serialized)
}

pub fn strip_tool_results(messages: Vec<Message>) -> (Vec<Message>, FileActivity) {
    history::strip_tool_results(messages)
}

pub fn sanitize_history_for_provider(messages: Vec<Message>) -> Vec<Message> {
    history::normalize_assistant_content_order(history::drop_unpaired_tool_calls(
        history::drop_orphaned_tool_results(messages),
    ))
}

/// The single canonical narrowing from the durable transcript to the provider
/// view: stub tool-result payloads, then drop unpaired calls and orphaned
/// results and normalize assistant content order.
///
/// Both sides of compaction's prefix accounting index *this* list. The
/// compaction writer records `messages_compacted` against it and the request
/// reader drops that many rows from it; measuring in one space and dropping in
/// another was defect 3 of #993.
///
/// Modelled as `Compaction.providerView`, proven idempotent by
/// `Compaction.providerView_idempotent` — which is what lets [`Compactor::compact`]
/// re-normalize its own input for free.
pub fn provider_view(messages: Vec<Message>) -> (Vec<Message>, FileActivity) {
    let (stripped, activity) = strip_tool_results(messages);
    (sanitize_history_for_provider(stripped), activity)
}

/// Greatest `j <= limit` at which no tool call is awaiting its result — the
/// index [`Compactor::compact`] retreats its token-budget split to.
///
/// Re-exported so the generated conformance cases can check the production
/// boundary against `Compaction.pairSafeBoundary`.
pub fn pair_safe_boundary(messages: &[Message], limit: usize) -> usize {
    history::pair_safe_boundary(messages, limit)
}

/// The real split [`Compactor::compact`] performs: everything before the
/// boundary is summarized, everything from it is retained.
///
/// Exported so the generated conformance cases can sweep budgets against the
/// live splitter rather than only against [`pair_safe_boundary`] — otherwise a
/// change that stopped *calling* the boundary would slip through.
pub fn split_for_summary(
    messages: Vec<Message>,
    keep_recent_tokens: usize,
) -> (Vec<Message>, Vec<Message>) {
    history::split_messages_for_summary(messages, keep_recent_tokens)
}

/// Mirror of Lean `StreamingResponse.Status`, with the same terminal partition,
/// so generated conformance cases can be fed straight in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseStatus {
    Streaming,
    Complete,
    Error,
}

impl ResponseStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Complete | Self::Error)
    }

    pub fn from_defra(value: &str) -> Option<Self> {
        match value {
            "streaming" => Some(Self::Streaming),
            "complete" => Some(Self::Complete),
            "error" => Some(Self::Error),
            _ => None,
        }
    }
}

/// Resolves the streaming status of the response that produced a message.
pub trait ResponseStatusIndex {
    fn status_of(&self, message: &Message) -> Option<ResponseStatus>;
}

/// Every response in scope is terminal.
pub struct AllTerminal;

impl ResponseStatusIndex for AllTerminal {
    fn status_of(&self, _message: &Message) -> Option<ResponseStatus> {
        Some(ResponseStatus::Complete)
    }
}

/// No status is known — the conservative resolution when anything in scope is
/// still streaming.
pub struct NoneKnown;

impl ResponseStatusIndex for NoneKnown {
    fn status_of(&self, _message: &Message) -> Option<ResponseStatus> {
        None
    }
}

/// Runtime counterpart of Lean `PromptView.safeToReduce`: a transcript may only
/// be reduced when every tool result it retains belongs to a response whose
/// status is known and terminal. Reducing under a live response can summarize
/// away a turn that is still being written.
///
/// See `boundary.compaction.safe-to-reduce-session-scope` for how the daemon
/// resolves statuses at session scope rather than per message.
pub fn safe_to_reduce(messages: &[Message], statuses: &impl ResponseStatusIndex) -> bool {
    messages.iter().all(|message| {
        if !carries_tool_result(message) {
            return true;
        }
        statuses
            .status_of(message)
            .is_some_and(ResponseStatus::is_terminal)
    })
}

/// Runtime counterpart of Lean `PromptAssembly.UniqueCallIds`: no tool-call id
/// is announced by more than one assistant message.
///
/// This is a *hypothesis* of the prefix-stability theorem, not a structural
/// guarantee — call ids come from the provider, and nothing in the ingestion
/// path enforces uniqueness across a session. `sanitize_history_for_provider`
/// credits an announcement from the globally resolved set, so a later turn that
/// reuses an id resurrects an earlier announcement the shorter view had dropped
/// as unpaired. The prefix then changes under append and a stored
/// `messages_compacted` no longer names the rows it was measured against.
/// `Compaction.reused_call_id_breaks_prefix_stability` exhibits exactly that.
///
/// Checking it here turns the theorem's hypothesis into a precondition the
/// runtime verifies before recording a count. See
/// `boundary.compaction.unique-call-ids-checked` for the residual gap and the
/// follow-up that would remove the need for the check.
pub fn has_unique_call_ids(messages: &[Message]) -> bool {
    let mut announced: std::collections::HashSet<String> = std::collections::HashSet::new();
    for message in messages {
        let Message::Assistant { content, .. } = message else {
            continue;
        };
        // Per-message first: Lean models a turn's ids as a `Finset`, so a repeat
        // *within* one announcement collapses rather than conflicting.
        let this_turn: std::collections::HashSet<String> = content
            .iter()
            .filter_map(|item| match item {
                crate::llm::message::AssistantContent::ToolCall(tool_call) => Some(
                    tool_call
                        .call_id
                        .clone()
                        .unwrap_or_else(|| tool_call.id.clone()),
                ),
                _ => None,
            })
            .collect();
        for call_id in this_turn {
            if !announced.insert(call_id) {
                return false;
            }
        }
    }
    true
}

fn carries_tool_result(message: &Message) -> bool {
    let Message::User { content } = message else {
        return false;
    };
    content
        .iter()
        .any(|item| matches!(item, crate::llm::message::UserContent::ToolResult(_)))
}

pub fn bounded_summary(summary: String) -> String {
    let (bounded, _, _) = crate::truncation::truncate_text(
        &summary,
        crate::truncation::TruncationMode::Head,
        &crate::truncation::TruncationLimits::default(),
    );
    bounded
}

/// A fractional threshold as integer basis points.
///
/// The configuration surface carries the threshold as `f64` (CLI, desired
/// state, schema), but the *budget* must not be computed in floating point:
/// `(context_window as f64 * threshold) as usize` truncates, and disagrees with
/// exact integer division for thresholds that are not exactly representable in
/// binary. 57% of 10,000 yields 5,699 that way rather than 5,700 (#1008).
///
/// Rounding recovers the intended basis points — `0.57 * 10_000` is
/// `5699.999999999999`, which rounds back to `5700` — so this is exact for any
/// threshold that originated as a percentage or basis-point value.
pub fn threshold_basis_points(threshold: f64) -> u64 {
    if !threshold.is_finite() || threshold <= 0.0 {
        return 0;
    }
    (threshold * 10_000.0).round().min(10_000.0) as u64
}

/// Tokens a fractional threshold allows within a context window, in exact
/// integer arithmetic. Mirrors Lean `PromptAssembly.Budget.configuredThresholdBudget`.
pub fn threshold_budget(context_window: usize, threshold: f64) -> usize {
    let basis_points = u128::from(threshold_basis_points(threshold));
    ((context_window as u128 * basis_points) / 10_000) as usize
}

/// Provider input available after both the configured compaction threshold and
/// the requested output reservation are applied. Mirrors Lean
/// `PromptAssembly.Budget.effectiveInputBudget`.
pub fn effective_input_budget(
    context_window: usize,
    max_output_tokens: usize,
    threshold: f64,
) -> usize {
    threshold_budget(context_window, threshold)
        .min(context_window.saturating_sub(max_output_tokens))
}

/// The shared provider-dispatch gate. It is deliberately expressed over an
/// already-assembled input estimate so callers at request entry and inside the
/// owned completion loop apply exactly the same output-reserved rule.
pub fn input_exceeds_budget(
    input_tokens: usize,
    context_window: usize,
    max_output_tokens: usize,
    threshold: f64,
) -> bool {
    input_tokens > effective_input_budget(context_window, max_output_tokens, threshold)
}

pub fn needs_compaction(messages: &[Message], context_window: usize, threshold: f64) -> bool {
    let tokens = estimate_message_tokens(messages);
    tokens > threshold_budget(context_window, threshold)
}
