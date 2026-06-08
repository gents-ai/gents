use anyhow::Result;
use rig::agent::Agent;
use rig::completion::message::Message;
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
    agent: Agent<M>,
}

impl<M: CompletionModel> DefraCompactor<M> {
    pub fn new(agent: Agent<M>) -> Self {
        Self { agent }
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
        // Summarize via the owned loop (#400) instead of rig's `Agent::prompt`.
        // This is a non-persisting, tool-free single completion: no hook (no
        // session/persistence), empty tool surface, `max_turns: 0`.
        let model = (*self.agent.model).clone();
        let loop_config = crate::agent::loop_stream::LoopConfig {
            preamble: self.agent.preamble.clone(),
            temperature: self.agent.temperature,
            max_tokens: self.agent.max_tokens,
            additional_params: self.agent.additional_params.clone(),
            tool_choice: None,
            max_turns: 0,
        };
        let raw_summary = crate::agent::loop_stream::run_loop_to_text(
            model,
            None,
            rig::completion::Message::user(compaction_prompt()),
            prepared_history.clone(),
            std::sync::Arc::new(Vec::new()),
            loop_config,
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

pub fn needs_compaction(messages: &[Message], context_window: usize, threshold: f64) -> bool {
    let tokens = estimate_message_tokens(messages);
    let budget = (context_window as f64 * threshold) as usize;
    tokens > budget
}
