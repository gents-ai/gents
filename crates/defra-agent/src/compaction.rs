//! Text compaction — shrink conversation history to fit within context windows.
//!
//! Compaction is a library function, not a policy. Callers decide when to compact.
//! Two-pass strategy: first strip tool call results into stubs, then summarize
//! older messages via the same Rig agent/model if the stripped history is still
//! over budget.

use std::collections::HashMap;

use anyhow::{Context, Result};
use rig::agent::Agent;
use rig::completion::message::{
    AssistantContent, Message, Text, ToolCall, ToolResult, ToolResultContent, UserContent,
};
use rig::completion::{CompletionModel, Prompt};
use rig::one_or_many::OneOrMany;
use serde::Deserialize;

/// Options controlling compaction behavior.
#[derive(Debug, Clone)]
pub struct CompactionOptions {
    /// Target size as percentage of context window (e.g., 0.75 = 75%).
    pub threshold: f64,
    /// Maximum characters to retain per tool result when preparing a summary request.
    pub tool_result_max_chars: usize,
    /// Number of recent tokens to keep untouched during compaction.
    pub keep_recent_tokens: usize,
    /// Strategy to apply.
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

/// Which compaction passes to apply.
#[derive(Debug, Clone)]
pub enum CompactionStrategy {
    /// Strip tool results only.
    StripToolResults,
    /// Summarize via LLM only.
    Summarize,
    /// Strip tool results first, then summarize if still over budget.
    StripThenSummarize,
}

/// File activity discovered while walking tool calls.
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

/// Metadata about a compaction operation.
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

/// Compacts conversation history to fit within context budget.
pub trait Compactor: Send + Sync {
    /// Compact the given messages according to the options.
    /// Returns the compacted messages and metadata.
    fn compact(
        &self,
        messages: Vec<Message>,
        context_window: usize,
        options: &CompactionOptions,
    ) -> impl std::future::Future<Output = Result<CompactionResult>> + Send;
}

/// Rig-backed compactor that reuses the daemon's preamble and tools for cache-safe
/// summarization calls.
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
        let mut prepared_history =
            pretruncate_tool_results(old_messages.clone(), options.tool_result_max_chars);
        let raw_summary = self
            .agent
            .prompt(compaction_prompt())
            .with_history(&mut prepared_history)
            .await?;
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

/// Rough token estimate: ~4 chars per token.
pub fn estimate_tokens(text: &str) -> usize {
    text.len() / 4
}

/// Estimate tokens for an entire message sequence.
pub fn estimate_message_tokens(messages: &[Message]) -> usize {
    let serialized = serde_json::to_string(messages).unwrap_or_default();
    estimate_tokens(&serialized)
}

/// Strip tool results from messages into compact stubs and extract file activity.
pub fn strip_tool_results(messages: Vec<Message>) -> (Vec<Message>, FileActivity) {
    let mut stripped_messages = Vec::with_capacity(messages.len());
    let mut tool_calls = HashMap::new();
    let mut file_activity = FileActivity::default();

    for message in messages {
        match message {
            Message::Assistant { id, content } => {
                for item in content.iter() {
                    if let AssistantContent::ToolCall(tool_call) = item {
                        let key = tool_call_key(tool_call);
                        let info = ToolCallInfo::from(tool_call);
                        push_file_activity(&mut file_activity, &info);
                        tool_calls.insert(key, info);
                    }
                }

                stripped_messages.push(Message::Assistant { id, content });
            }
            Message::User { content } => {
                let items: Vec<UserContent> = content
                    .into_iter()
                    .map(|item| match item {
                        UserContent::ToolResult(tool_result) => {
                            UserContent::ToolResult(strip_tool_result(tool_result, &tool_calls))
                        }
                        other => other,
                    })
                    .collect();

                let content = OneOrMany::many(items).unwrap_or_else(|_| {
                    OneOrMany::one(UserContent::Text(Text {
                        text: String::new(),
                    }))
                });
                stripped_messages.push(Message::User { content });
            }
        }
    }

    dedupe_paths(&mut file_activity.files_read);
    dedupe_paths(&mut file_activity.files_modified);
    (stripped_messages, file_activity)
}

/// Check whether compaction is needed based on the token estimate and threshold.
pub fn needs_compaction(messages: &[Message], context_window: usize, threshold: f64) -> bool {
    let tokens = estimate_message_tokens(messages);
    let budget = (context_window as f64 * threshold) as usize;
    tokens > budget
}

fn pretruncate_tool_results(messages: Vec<Message>, max_chars: usize) -> Vec<Message> {
    messages
        .into_iter()
        .map(|message| match message {
            Message::User { content } => {
                let items: Vec<UserContent> = content
                    .into_iter()
                    .map(|item| match item {
                        UserContent::ToolResult(mut tool_result) => {
                            let truncated_contents: Vec<ToolResultContent> = tool_result
                                .content
                                .into_iter()
                                .map(|content| truncate_tool_result_content(content, max_chars))
                                .collect();
                            tool_result.content = OneOrMany::many(truncated_contents)
                                .unwrap_or_else(|_| {
                                    OneOrMany::one(ToolResultContent::Text(Text {
                                        text: "[empty tool result]".to_string(),
                                    }))
                                });
                            UserContent::ToolResult(tool_result)
                        }
                        other => other,
                    })
                    .collect();

                Message::User {
                    content: OneOrMany::many(items).unwrap_or_else(|_| {
                        OneOrMany::one(UserContent::Text(Text {
                            text: String::new(),
                        }))
                    }),
                }
            }
            other => other,
        })
        .collect()
}

fn truncate_tool_result_content(content: ToolResultContent, max_chars: usize) -> ToolResultContent {
    match content {
        ToolResultContent::Text(text) if text.text.len() > max_chars => {
            let truncated = format!(
                "{}… [pre-truncated {}/{} chars for compaction]",
                &text.text[..max_chars],
                max_chars,
                text.text.len()
            );
            ToolResultContent::Text(Text { text: truncated })
        }
        other => other,
    }
}

fn strip_tool_result(
    mut tool_result: ToolResult,
    tool_calls: &HashMap<String, ToolCallInfo>,
) -> ToolResult {
    let call_id = tool_result
        .call_id
        .clone()
        .unwrap_or_else(|| tool_result.id.clone());
    let tool_name = tool_calls
        .get(&call_id)
        .map(|info| info.name.as_str())
        .unwrap_or("unknown");
    let byte_count = tool_result_byte_count(&tool_result);
    let truncated = tool_result_was_truncated(&tool_result);
    let stub = if truncated {
        format!(
            "[tool: {tool_name}, call_id: {call_id}, {byte_count} bytes, truncated — see DefraDB AgentToolCall for full output]"
        )
    } else {
        format!(
            "[tool: {tool_name}, call_id: {call_id}, {byte_count} bytes — see DefraDB AgentToolCall for full output]"
        )
    };

    tool_result.content = OneOrMany::one(ToolResultContent::Text(Text { text: stub }));
    tool_result
}

fn tool_result_byte_count(tool_result: &ToolResult) -> usize {
    tool_result
        .content
        .iter()
        .map(|content| match content {
            ToolResultContent::Text(text) => text.text.len(),
            other => serde_json::to_string(other).map_or(0, |value| value.len()),
        })
        .sum()
}

fn tool_result_was_truncated(tool_result: &ToolResult) -> bool {
    tool_result.content.iter().any(|content| match content {
        ToolResultContent::Text(text) => {
            text.text.contains("[Full output: DefraDB doc")
                || text.text.contains("Showing lines")
                || text.text.contains("truncated")
        }
        _ => false,
    })
}

fn split_messages_for_summary(
    messages: Vec<Message>,
    keep_recent_tokens: usize,
) -> (Vec<Message>, Vec<Message>) {
    if messages.len() <= 1 {
        return (Vec::new(), messages);
    }

    let mut split_index = messages.len();
    let mut recent_tokens = 0usize;

    for index in (0..messages.len()).rev() {
        let token_cost = estimate_message_tokens(std::slice::from_ref(&messages[index]));
        if split_index == messages.len() || recent_tokens + token_cost <= keep_recent_tokens {
            recent_tokens += token_cost;
            split_index = index;
            continue;
        }

        break;
    }

    if split_index == 0 {
        return (Vec::new(), messages);
    }

    let old_messages = messages[..split_index].to_vec();
    let recent_messages = messages[split_index..].to_vec();
    (old_messages, recent_messages)
}

fn extract_file_activity(messages: &[Message]) -> FileActivity {
    let mut activity = FileActivity::default();
    for message in messages {
        if let Message::Assistant { content, .. } = message {
            for item in content.iter() {
                if let AssistantContent::ToolCall(tool_call) = item {
                    let info = ToolCallInfo::from(tool_call);
                    push_file_activity(&mut activity, &info);
                }
            }
        }
    }

    dedupe_paths(&mut activity.files_read);
    dedupe_paths(&mut activity.files_modified);
    activity
}

fn push_file_activity(activity: &mut FileActivity, info: &ToolCallInfo) {
    if let Some(path) = &info.file_path {
        if info.is_write {
            activity.files_modified.push(path.clone());
        } else if info.is_read {
            activity.files_read.push(path.clone());
        }
    }
}

fn tool_call_key(tool_call: &ToolCall) -> String {
    tool_call
        .call_id
        .clone()
        .unwrap_or_else(|| tool_call.id.clone())
}

fn compaction_prompt() -> &'static str {
    "Summarize the earlier conversation turns immediately before this message. \
Return JSON only with keys: summary (string), files_read (array of strings), \
files_modified (array of strings), key_decisions (array of strings), \
pending_questions (array of strings). Preserve concrete facts, file paths, \
unfinished work, and major findings. Do not invent tool results."
}

fn parse_summary_response(raw_summary: &str) -> Result<SummaryResponse> {
    let trimmed = raw_summary.trim();
    let json = trimmed
        .strip_prefix("```json")
        .and_then(|value| value.strip_suffix("```"))
        .map(str::trim)
        .or_else(|| {
            trimmed
                .strip_prefix("```")
                .and_then(|value| value.strip_suffix("```"))
                .map(str::trim)
        })
        .unwrap_or(trimmed);

    let mut summary: SummaryResponse = serde_json::from_str(json)
        .with_context(|| format!("parsing compaction summary response: {json}"))?;
    dedupe_paths(&mut summary.files_read);
    dedupe_paths(&mut summary.files_modified);
    Ok(summary)
}

fn format_summary(
    narrative: &str,
    files_read: &[String],
    files_modified: &[String],
    key_decisions: &[String],
    pending_questions: &[String],
) -> String {
    let mut sections = vec![narrative.trim().to_string()];

    if !files_read.is_empty() {
        sections.push(format!(
            "Files read:\n{}",
            files_read
                .iter()
                .map(|item| format!("- {}", item.trim()))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }

    if !files_modified.is_empty() {
        sections.push(format!(
            "Files modified:\n{}",
            files_modified
                .iter()
                .map(|item| format!("- {}", item.trim()))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }

    if !key_decisions.is_empty() {
        sections.push(format!(
            "Key decisions and findings:\n{}",
            key_decisions
                .iter()
                .map(|item| format!("- {}", item.trim()))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }

    if !pending_questions.is_empty() {
        sections.push(format!(
            "Pending questions:\n{}",
            pending_questions
                .iter()
                .map(|item| format!("- {}", item.trim()))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }

    sections
        .into_iter()
        .filter(|section| !section.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn dedupe_paths(paths: &mut Vec<String>) {
    paths.sort();
    paths.dedup();
}

#[derive(Debug, Clone)]
struct ToolCallInfo {
    name: String,
    file_path: Option<String>,
    is_read: bool,
    is_write: bool,
}

impl From<&ToolCall> for ToolCallInfo {
    fn from(tool_call: &ToolCall) -> Self {
        let file_path = tool_call
            .function
            .arguments
            .get("file_path")
            .or_else(|| tool_call.function.arguments.get("path"))
            .and_then(|value| value.as_str())
            .map(ToOwned::to_owned);
        let name = tool_call.function.name.clone();

        Self {
            is_read: matches!(
                name.as_str(),
                "read" | "cat" | "grep" | "search" | "find" | "query"
            ),
            is_write: matches!(name.as_str(), "write" | "edit" | "replace" | "apply_patch"),
            name,
            file_path,
        }
    }
}

#[derive(Debug, Deserialize)]
struct SummaryResponse {
    summary: String,
    #[serde(default)]
    files_read: Vec<String>,
    #[serde(default)]
    files_modified: Vec<String>,
    #[serde(default)]
    key_decisions: Vec<String>,
    #[serde(default)]
    pending_questions: Vec<String>,
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::ensure_schemas;
    use crate::prompt::{LayeredPromptBuilder, PromptBuilder};
    use crate::session;
    use rig::agent::AgentBuilder;
    use rig::completion::{CompletionError, CompletionRequest, CompletionResponse, Usage};
    use rig::streaming::StreamingCompletionResponse;

    fn text_msg(role: &str, text: &str) -> Message {
        match role {
            "user" => Message::User {
                content: OneOrMany::one(UserContent::Text(Text {
                    text: text.to_string(),
                })),
            },
            "assistant" => Message::Assistant {
                id: None,
                content: OneOrMany::one(AssistantContent::Text(Text {
                    text: text.to_string(),
                })),
            },
            _ => panic!("unknown role"),
        }
    }

    fn tool_call_msg(name: &str, args: &str) -> Message {
        Message::Assistant {
            id: None,
            content: OneOrMany::one(AssistantContent::ToolCall(ToolCall {
                id: "call-1".to_string(),
                call_id: Some("call-1".to_string()),
                function: rig::completion::message::ToolFunction {
                    name: name.to_string(),
                    arguments: serde_json::from_str(args).unwrap_or_default(),
                },
                signature: None,
                additional_params: None,
            })),
        }
    }

    fn tool_result_msg(call_id: &str, result_text: &str) -> Message {
        Message::User {
            content: OneOrMany::one(UserContent::ToolResult(ToolResult {
                id: call_id.to_string(),
                call_id: Some(call_id.to_string()),
                content: OneOrMany::one(ToolResultContent::Text(Text {
                    text: result_text.to_string(),
                })),
            })),
        }
    }

    #[derive(Clone, Default)]
    struct MockSummaryModel {
        response: String,
        last_request: Arc<Mutex<Option<CompletionRequest>>>,
    }

    impl MockSummaryModel {
        fn new(response: &str) -> Self {
            Self {
                response: response.to_string(),
                last_request: Arc::new(Mutex::new(None)),
            }
        }
    }

    #[allow(refining_impl_trait)]
    impl CompletionModel for MockSummaryModel {
        type Response = ();
        type StreamingResponse = ();
        type Client = ();

        fn make(_: &Self::Client, _: impl Into<String>) -> Self {
            Self::default()
        }

        async fn completion(
            &self,
            request: CompletionRequest,
        ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
            *self.last_request.lock().unwrap() = Some(request);
            Ok(CompletionResponse {
                choice: OneOrMany::one(AssistantContent::Text(Text {
                    text: self.response.clone(),
                })),
                usage: Usage::new(),
                raw_response: (),
                message_id: None,
            })
        }

        async fn stream(
            &self,
            _request: CompletionRequest,
        ) -> Result<StreamingCompletionResponse<Self::StreamingResponse>, CompletionError> {
            Err(CompletionError::ProviderError(
                "streaming is unused in compaction tests".to_string(),
            ))
        }
    }

    #[test]
    fn strip_preserves_text_messages() {
        let messages = vec![text_msg("user", "hello"), text_msg("assistant", "hi there")];
        let (stripped, files) = strip_tool_results(messages);
        assert_eq!(stripped.len(), 2);
        assert!(files.files_read.is_empty());
        assert!(files.files_modified.is_empty());
    }

    #[test]
    fn strip_rewrites_tool_results_into_stubs() {
        let long_result = "x".repeat(5000);
        let messages = vec![
            text_msg("user", "read this file"),
            tool_call_msg("read", r#"{"file_path": "/tmp/test.rs"}"#),
            tool_result_msg("call-1", &long_result),
            text_msg("assistant", "I saw the file"),
        ];

        let (stripped, files) = strip_tool_results(messages);
        assert_eq!(stripped.len(), 4);
        assert_eq!(files.files_read, vec!["/tmp/test.rs"]);
        assert!(files.files_modified.is_empty());

        if let Message::User { content } = &stripped[2] {
            if let UserContent::ToolResult(tr) = content.first_ref() {
                if let ToolResultContent::Text(text) = tr.content.first_ref() {
                    assert_eq!(
                        text.text,
                        "[tool: read, call_id: call-1, 5000 bytes — see DefraDB AgentToolCall for full output]"
                    );
                } else {
                    panic!("expected text content");
                }
            } else {
                panic!("expected tool result");
            }
        } else {
            panic!("expected user message");
        }
    }

    #[test]
    fn strip_extracts_read_and_modified_files() {
        let messages = vec![
            tool_call_msg("read", r#"{"file_path": "/src/main.rs"}"#),
            tool_result_msg("call-1", "fn main() {}"),
            tool_call_msg("write", r#"{"file_path": "/src/lib.rs"}"#),
            tool_result_msg("call-1", "ok"),
        ];

        let (_, files) = strip_tool_results(messages);
        assert_eq!(files.files_read, vec!["/src/main.rs"]);
        assert_eq!(files.files_modified, vec!["/src/lib.rs"]);
    }

    #[test]
    fn needs_compaction_under_threshold() {
        let messages = vec![text_msg("user", "hi")];
        assert!(!needs_compaction(&messages, 100000, 0.75));
    }

    #[test]
    fn needs_compaction_over_threshold() {
        let big = "x".repeat(10000);
        let messages = vec![text_msg("user", &big)];
        assert!(needs_compaction(&messages, 1000, 0.75));
    }

    #[test]
    fn estimate_tokens_rough() {
        assert_eq!(estimate_tokens("hello world!"), 3);
        assert_eq!(estimate_tokens(""), 0);
    }

    #[tokio::test]
    async fn defra_compactor_uses_mock_summary_response() {
        let response = serde_json::json!({
            "summary": "The agent investigated the build failure.",
            "files_read": ["/tmp/test.rs"],
            "files_modified": ["/tmp/lib.rs"],
            "key_decisions": ["Keep the parser small"],
            "pending_questions": ["Should we split the module?"]
        })
        .to_string();
        let model = MockSummaryModel::new(&response);
        let agent = AgentBuilder::new(model.clone())
            .preamble("You are a coding agent.")
            .build();
        let compactor = DefraCompactor::new(agent);
        let messages = vec![
            text_msg("user", &"x".repeat(12000)),
            tool_call_msg("read", r#"{"file_path": "/tmp/test.rs"}"#),
            tool_result_msg("call-1", &"y".repeat(6000)),
            text_msg("assistant", "I found the failing parser."),
            text_msg("user", "Please summarize the older work."),
        ];

        let result = compactor
            .compact(
                messages,
                1000,
                &CompactionOptions {
                    threshold: 0.75,
                    keep_recent_tokens: 10,
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        assert_eq!(result.compaction_count, 1);
        assert!(result.summary.as_ref().unwrap().contains("build failure"));
        assert_eq!(result.files_read, vec!["/tmp/test.rs"]);
        assert_eq!(result.files_modified, vec!["/tmp/lib.rs"]);
        assert!(result.messages_compacted > 0);

        let request = model.last_request.lock().unwrap().clone().unwrap();
        assert!(request
            .preamble
            .as_deref()
            .unwrap()
            .contains("coding agent"));
        let last_message = request.chat_history.last();
        match last_message {
            Message::User { content } => match content.first_ref() {
                UserContent::Text(text) => {
                    assert_eq!(text.text, compaction_prompt());
                }
                other => panic!("expected text prompt, got {other:?}"),
            },
            other => panic!("expected user prompt, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn integration_compaction_persists_entry_and_prompt_builder_uses_it() {
        let data_path =
            std::env::temp_dir().join(format!("agent-daemon-compactor-{}", uuid::Uuid::new_v4()));
        let node = defra_node::EmbeddedNode::builder()
            .data_path(&data_path)
            .build()
            .await
            .unwrap();
        ensure_schemas(&node).await.unwrap();
        session::create_session_with_id(&node, "session-1", "general")
            .await
            .unwrap();

        let model = MockSummaryModel::new(
            &serde_json::json!({
                "summary": "The agent repeatedly inspected the source files.",
                "files_read": ["/workspace/main.rs"],
                "files_modified": [],
                "key_decisions": ["Use compaction to collapse older turns"],
                "pending_questions": []
            })
            .to_string(),
        );
        let agent = AgentBuilder::new(model)
            .preamble("You are a helpful coding agent.")
            .build();
        let compactor = DefraCompactor::new(agent);

        let mut sequence = 1;
        for turn in 0..55 {
            let user = Message::User {
                content: OneOrMany::one(UserContent::Text(Text {
                    text: format!("Request {turn}: {}", "x".repeat(800)),
                })),
            };
            let assistant_tool_call =
                tool_call_msg("read", r#"{"file_path": "/workspace/main.rs"}"#);
            let tool_result = tool_result_msg("call-1", &"file contents\n".repeat(50));
            let assistant = text_msg(
                "assistant",
                &format!("Response {turn}: {}", "y".repeat(500)),
            );

            session::save_message(
                &node,
                "session-1",
                sequence,
                "user",
                &serde_json::to_string(&user).unwrap(),
            )
            .await
            .unwrap();
            sequence += 1;

            session::save_message(
                &node,
                "session-1",
                sequence,
                "assistant",
                &serde_json::to_string(&assistant_tool_call).unwrap(),
            )
            .await
            .unwrap();
            sequence += 1;

            session::save_message(
                &node,
                "session-1",
                sequence,
                "user",
                &serde_json::to_string(&tool_result).unwrap(),
            )
            .await
            .unwrap();
            sequence += 1;

            session::save_message(
                &node,
                "session-1",
                sequence,
                "assistant",
                &serde_json::to_string(&assistant).unwrap(),
            )
            .await
            .unwrap();
            sequence += 1;
        }

        let history = session::load_history(&node, "session-1").await.unwrap();
        let (stripped_history, _) = strip_tool_results(history);
        let result = compactor
            .compact(
                stripped_history,
                2000,
                &CompactionOptions {
                    threshold: 0.50,
                    keep_recent_tokens: 200,
                    strategy: CompactionStrategy::Summarize,
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let summary = result.summary.clone().unwrap();
        session::save_compaction_entry(
            &node,
            "session-1",
            &summary,
            &result.files_read,
            &result.files_modified,
            result.messages_compacted,
            result.original_token_estimate,
            result.compacted_token_estimate,
        )
        .await
        .unwrap();

        let entries = session::load_compaction_entries(&node, "session-1")
            .await
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].summary.contains("inspected the source files"));

        let resumed_history = session::load_history(&node, "session-1").await.unwrap();
        let (resumed_history, _) = strip_tool_results(resumed_history);
        let compacted_count = entries
            .iter()
            .map(|entry| entry.messages_compacted as usize)
            .sum::<usize>();
        let resumed_history = resumed_history
            .into_iter()
            .skip(compacted_count)
            .collect::<Vec<_>>();
        assert_eq!(resumed_history, result.messages);

        let config = crate::config::DaemonConfig {
            system_prompt: "Be helpful.".to_string(),
            data_room: "general".to_string(),
            ..Default::default()
        };
        let prompt_builder = LayeredPromptBuilder::new(&config);
        let summaries = entries
            .iter()
            .map(|entry| entry.summary.clone())
            .collect::<Vec<_>>();
        let built = prompt_builder
            .build(&resumed_history, &summaries)
            .await
            .unwrap();

        if let Message::User { content } = &built.messages[0] {
            if let UserContent::Text(text) = content.first_ref() {
                assert!(text.text.contains("inspected the source files"));
                assert!(text
                    .text
                    .contains("Previous conversation summary (compacted):"));
            } else {
                panic!("expected summary reminder text");
            }
        } else {
            panic!("expected summary reminder");
        }

        assert_eq!(built.messages[1..], resumed_history[..]);

        let _ = std::fs::remove_dir_all(&data_path);
    }
}
