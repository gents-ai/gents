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
    pub(crate) fn new(model: Arc<M>, config: crate::agent::loop_stream::LoopConfig) -> Self {
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

        let (stripped_messages, stripped_activity) = match options.strategy {
            CompactionStrategy::StripToolResults | CompactionStrategy::StripThenSummarize => {
                strip_tool_results(messages)
            }
            CompactionStrategy::Summarize => {
                let activity = extract_file_activity(&messages);
                (messages, activity)
            }
        };

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
        // Summarize via the owned loop (#400): a non-persisting, tool-free single
        // completion (no hook, empty tool surface, `max_turns: 0`).
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

/// The full provider-send boundary sanitization for loaded history: drop
/// orphaned tool results, then drop unpaired tool calls (#445), then
/// normalize assistant content order. New sanitizers that narrow the
/// permissive durable transcript to the stricter provider format belong here
/// (see the `history` components), NOT in the conformance-fenced reducers.
/// Runs on the loaded transcript AND on the compaction output (the recent
/// window can begin mid-exchange).
///
/// ORDER MATTERS (PromptAssembly model, P1 soundness): orphan-drop must run
/// FIRST. A result that precedes its call is orphaned; if unpaired-drop ran
/// first it would keep that call on the strength of the about-to-be-dropped
/// result, and an unpaired call would reach the provider. Orphan-drop also
/// treats normal conversation as closing the active tool-call block, so late
/// results do not survive on the strength of stale earlier calls. In this
/// order, unpaired-drop only removes calls NO surviving result references (so
/// it can never create a new orphan).
pub fn sanitize_history_for_provider(messages: Vec<Message>) -> Vec<Message> {
    history::normalize_assistant_content_order(history::drop_unpaired_tool_calls(
        history::drop_orphaned_tool_results(messages),
    ))
}

/// Bound a compaction summary on its way into the prompt. The summary is
/// model-emitted free text injected into every subsequent request's system
/// reminder; bounding at the consumption point covers oversized entries
/// already persisted as well as new ones.
pub fn bounded_summary(summary: String) -> String {
    // Head mode: the narrative leads the summary; bulleted file/decision
    // lists trail and are the right part to lose.
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
