use std::sync::Arc;

use crate::llm::message::Message;
use anyhow::Result;
use rig::completion::CompletionModel;

mod history;
mod summary;
#[cfg(test)]
mod tests;

use history::{extract_file_activity, pretruncate_tool_results, split_messages_for_summary};
use summary::{compaction_prompt, dedupe_paths, format_summary, parse_summary_response};

#[derive(Debug, Clone)]
pub struct CompactionOptions {
    pub threshold: f64,
    pub tool_result_max_chars: usize,
    pub keep_recent_tokens: usize,
    pub strategy: CompactionStrategy,
}

impl Default for CompactionOptions {
    fn default() -> Self {
        Self {
            threshold: 0.75,
            tool_result_max_chars: 2000,
            keep_recent_tokens: 20000,
            strategy: CompactionStrategy::StripThenSummarize,
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
        // block inline compaction for minutes). Fail fast instead (#648).
        config.retry_policy = crate::agent::completion_retry::CompletionRetryPolicy::no_retry();
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
        let (stripped_messages, stripped_activity) = provider_view(messages);

        let stripped_token_estimate = estimate_message_tokens(&stripped_messages);
        if matches!(options.strategy, CompactionStrategy::StripToolResults)
            || !needs_compaction(&stripped_messages, context_window, options.threshold)
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
        let raw_summary = crate::agent::loop_stream::run_loop_to_text(
            (*self.model).clone(),
            None,
            crate::llm::message::Message::user(compaction_prompt()),
            prepared_history.clone(),
            std::sync::Arc::new(Vec::new()),
            self.config.clone(),
        )
        .await
        .map_err(|error| anyhow::anyhow!("compaction summary inference failed: {error}"))?;
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

pub fn needs_compaction(messages: &[Message], context_window: usize, threshold: f64) -> bool {
    let tokens = estimate_message_tokens(messages);
    let budget = (context_window as f64 * threshold) as usize;
    tokens > budget
}
