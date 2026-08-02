use std::collections::HashMap;

use crate::llm::message::{
    AssistantContent, Message, Text, ToolCall, ToolResult, ToolResultContent, UserContent,
};

use super::summary::dedupe_paths;
use super::{estimate_message_tokens, FileActivity};

pub(super) fn strip_tool_results(messages: Vec<Message>) -> (Vec<Message>, FileActivity) {
    let mut stripped_messages = Vec::with_capacity(messages.len());
    let mut tool_calls = HashMap::new();
    let mut file_activity = FileActivity::default();

    for message in messages {
        match message {
            Message::Assistant { id, content } => {
                // Scope the lookup to the turn being opened. Call ids are
                // provider-generated and can repeat across turns, so a stale
                // entry could label a later stub with the wrong tool. Only an
                // assistant message that actually announces calls opens a new
                // turn — a text-only one must not orphan the pending lookups.
                let opens_turn = content
                    .iter()
                    .any(|item| matches!(item, AssistantContent::ToolCall(_)));
                if opens_turn {
                    tool_calls.clear();
                }

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

                stripped_messages.push(Message::User { content: items });
            }
            Message::System { content } => {
                stripped_messages.push(Message::System { content });
            }
        }
    }

    dedupe_paths(&mut file_activity.files_read);
    dedupe_paths(&mut file_activity.files_modified);
    (stripped_messages, file_activity)
}

pub(super) fn drop_unpaired_tool_calls(messages: Vec<Message>) -> Vec<Message> {
    let mut resolved: std::collections::HashSet<String> = std::collections::HashSet::new();
    for message in &messages {
        if let Message::User { content } = message {
            for item in content.iter() {
                if let UserContent::ToolResult(tool_result) = item {
                    resolved.insert(tool_result_key(tool_result));
                }
            }
        }
    }

    let mut kept_messages = Vec::with_capacity(messages.len());
    for message in messages {
        match message {
            Message::Assistant { id, content } => {
                let kept: Vec<AssistantContent> = content
                    .into_iter()
                    .filter(|item| match item {
                        AssistantContent::ToolCall(tool_call) => {
                            resolved.contains(&tool_call_key(tool_call))
                        }
                        _ => true,
                    })
                    .collect();
                if !kept.is_empty() {
                    kept_messages.push(Message::Assistant { id, content: kept });
                }
            }
            other => kept_messages.push(other),
        }
    }
    kept_messages
}

pub(super) fn drop_orphaned_tool_results(messages: Vec<Message>) -> Vec<Message> {
    let mut pending_calls: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut kept_messages = Vec::with_capacity(messages.len());
    for message in messages {
        match message {
            Message::Assistant { id, content } => {
                pending_calls.clear();
                for item in content.iter() {
                    if let AssistantContent::ToolCall(tool_call) = item {
                        pending_calls.insert(tool_call_key(tool_call));
                    }
                }
                kept_messages.push(Message::Assistant { id, content });
            }
            Message::User { content } => {
                let has_plain_content = content
                    .iter()
                    .any(|item| !matches!(item, UserContent::ToolResult(_)));
                let kept: Vec<UserContent> = content
                    .into_iter()
                    .filter(|item| match item {
                        UserContent::ToolResult(tool_result) => {
                            pending_calls.remove(&tool_result_key(tool_result))
                        }
                        _ => true,
                    })
                    .collect();
                if has_plain_content {
                    pending_calls.clear();
                }
                if !kept.is_empty() {
                    kept_messages.push(Message::User { content: kept });
                }
            }
            other => {
                pending_calls.clear();
                kept_messages.push(other);
            }
        }
    }
    kept_messages
}

/// Normalize assistant content to the canonical provider order — text, then
/// reasoning (and any other non-call content), then tool calls — at the
/// provider-send boundary.
///
/// `AssistantTurnAccumulator::build_message` writes this order for newly
/// persisted turns, but transcripts persisted before the ordering fix can carry
/// text *after* tool calls, which strict providers reject on reload. Like
/// `drop_unpaired_tool_calls`, this narrows the durable transcript to the
/// provider format at the request-build boundary; the stored messages and the
/// conformance-fenced reducers are untouched. Relative order within each
/// category is preserved.
pub(super) fn normalize_assistant_content_order(messages: Vec<Message>) -> Vec<Message> {
    messages
        .into_iter()
        .map(|message| match message {
            Message::Assistant { id, content } => {
                let mut text = Vec::new();
                let mut middle = Vec::new();
                let mut calls = Vec::new();
                for item in content.into_iter() {
                    match item {
                        AssistantContent::Text(_) => text.push(item),
                        AssistantContent::ToolCall(_) => calls.push(item),
                        other => middle.push(other),
                    }
                }
                let ordered: Vec<AssistantContent> =
                    text.into_iter().chain(middle).chain(calls).collect();
                Message::Assistant {
                    id,
                    content: ordered,
                }
            }
            other => other,
        })
        .collect()
}

pub(super) fn pretruncate_tool_results(messages: Vec<Message>, max_chars: usize) -> Vec<Message> {
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
                            tool_result.content = truncated_contents;
                            UserContent::ToolResult(tool_result)
                        }
                        other => other,
                    })
                    .collect();

                Message::User { content: items }
            }
            other => other,
        })
        .collect()
}

pub(super) fn split_messages_for_summary(
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

    // The token budget can land the boundary between an assistant message
    // carrying a ToolCall and the user message carrying its ToolResult. Left
    // alone, the call is summarized away while the result stays in the retained
    // tail, and `sanitize_history_for_provider` then drops the orphaned result
    // at loop entry — the tool's output is lost from the provider view entirely
    // while the summary describes only the call.
    //
    // Retreat to the nearest turn boundary. Moving *earlier* over-retains by at
    // most one turn and never loses context; moving later would summarize a turn
    // the budget wanted kept. For provider-input assembly, over-retaining is the
    // correct failure direction.
    //
    // Modelled as `Compaction.pairSafeBoundary`, with
    // `Compaction.raw_split_can_orphan` witnessing that the unadjusted index is
    // unsound.
    let split_index = pair_safe_boundary(&messages, split_index);

    if split_index == 0 {
        return (Vec::new(), messages);
    }

    let old_messages = messages[..split_index].to_vec();
    let recent_messages = messages[split_index..].to_vec();
    (old_messages, recent_messages)
}

/// Greatest `j <= limit` at which no tool call is awaiting its result.
///
/// Mirrors `Compaction.pairSafeBoundary` and the pending-set discipline in
/// [`drop_orphaned_tool_results`]: an assistant message replaces the pending set
/// with its own call ids, a tool result erases one, and anything else clears it.
pub(super) fn pair_safe_boundary(messages: &[Message], limit: usize) -> usize {
    let limit = limit.min(messages.len());
    let mut pending: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut boundary = 0usize;

    for (index, message) in messages.iter().take(limit).enumerate() {
        if pending.is_empty() {
            boundary = index;
        }
        match message {
            Message::Assistant { content, .. } => {
                pending = content
                    .iter()
                    .filter_map(|item| match item {
                        AssistantContent::ToolCall(tool_call) => Some(tool_call_key(tool_call)),
                        _ => None,
                    })
                    .collect();
            }
            Message::User { content } => {
                let has_plain_content = content
                    .iter()
                    .any(|item| !matches!(item, UserContent::ToolResult(_)));
                for item in content.iter() {
                    if let UserContent::ToolResult(tool_result) = item {
                        pending.remove(&tool_result_key(tool_result));
                    }
                }
                if has_plain_content {
                    pending.clear();
                }
            }
            Message::System { .. } => pending.clear(),
        }
    }

    if pending.is_empty() {
        limit
    } else {
        boundary
    }
}

pub(super) fn extract_file_activity(messages: &[Message]) -> FileActivity {
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

fn truncate_tool_result_content(content: ToolResultContent, max_chars: usize) -> ToolResultContent {
    match content {
        ToolResultContent::Text(text) if text.text.len() > max_chars => {
            // `max_chars` is a byte budget but tool output is arbitrary UTF-8,
            // so slicing at that index panics whenever it lands inside a
            // codepoint. Floor to the nearest boundary.
            let cut = floor_char_boundary(&text.text, max_chars);
            let truncated = format!(
                "{}… [pre-truncated {}/{} chars for compaction]",
                &text.text[..cut],
                cut,
                text.text.len()
            );
            ToolResultContent::Text(Text { text: truncated })
        }
        other => other,
    }
}

fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    if index >= text.len() {
        return text.len();
    }
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// Head and tail of every stub this module writes.
///
/// Recognizing an existing stub is what makes `strip_tool_results` idempotent.
/// Without it a second pass re-measures the stub and reports *its* length
/// instead of the tool's — and the request path really does strip twice, once
/// in `agent/daemon/request.rs` and again inside `compact()`, so after a
/// compaction the model was being told the wrong byte count.
const STUB_HEAD: &str = "[tool: ";
const STUB_TAIL: &str = "see DefraDB AgentToolCall for full output]";

/// Markers the truncation layer itself writes (`truncation::logic`,
/// `truncation::spill`). Matching these exactly replaces a `contains("truncated")`
/// sniff that fired on any tool output happening to mention the word.
const TRUNCATION_MARKERS: [&str; 2] = ["[Full output: DefraDB doc ", "[Showing lines "];

fn tool_result_is_stub(tool_result: &ToolResult) -> bool {
    matches!(
        tool_result.content.as_slice(),
        [ToolResultContent::Text(text)]
            if text.text.starts_with(STUB_HEAD) && text.text.ends_with(STUB_TAIL)
    )
}

fn strip_tool_result(
    mut tool_result: ToolResult,
    tool_calls: &HashMap<String, ToolCallInfo>,
) -> ToolResult {
    if tool_result_is_stub(&tool_result) {
        return tool_result;
    }

    let call_id = tool_result_key(&tool_result);
    let info = tool_calls.get(&call_id);
    let tool_name = info.map_or("unknown", |info| info.name.as_str());
    // The primary path argument is already extracted for `FileActivity`.
    // Carrying it turns the stub from "a file was read" into "this file was
    // read", which is most of what a pointer stub exists to do.
    let argument = info
        .and_then(|info| info.file_path.as_deref())
        .map(|path| format!("({path})"))
        .unwrap_or_default();
    let byte_count = tool_result_byte_count(&tool_result);
    let truncated = if tool_result_was_truncated(&tool_result) {
        ", truncated"
    } else {
        ""
    };

    let stub = format!(
        "[tool: {tool_name}{argument}, call_id: {call_id}, {byte_count} bytes{truncated} — {STUB_TAIL}"
    );
    tool_result.content = vec![ToolResultContent::Text(Text { text: stub })];
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
        ToolResultContent::Text(text) => TRUNCATION_MARKERS
            .iter()
            .any(|marker| text.text.contains(marker)),
        _ => false,
    })
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

fn tool_result_key(tool_result: &ToolResult) -> String {
    tool_result
        .call_id
        .clone()
        .unwrap_or_else(|| tool_result.id.clone())
}

#[derive(Debug, Clone)]
struct ToolCallInfo {
    name: String,
    file_path: Option<String>,
    is_read: bool,
    is_write: bool,
}

/// Tools whose calls mean "this path was read".
///
/// The first group is the registered native file tools (`toolset/file_tools.rs`);
/// the second is the generic names MCP servers commonly use. Keep this in sync
/// with the tool registry — `every_registered_file_tool_is_classified` fails if
/// a file tool is added without a classification, which would silently empty the
/// compaction summary's file lists.
pub(super) fn is_read_tool(name: &str) -> bool {
    matches!(
        name,
        "read_file"
            | "list_files"
            | "glob"
            | "grep"
            | "read"
            | "cat"
            | "search"
            | "find"
            | "query"
    )
}

/// Tools whose calls mean "this path was modified".
pub(super) fn is_write_tool(name: &str) -> bool {
    matches!(
        name,
        "write_file" | "edit_file" | "write" | "edit" | "replace" | "apply_patch"
    )
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
            is_read: is_read_tool(&name),
            is_write: is_write_tool(&name),
            name,
            file_path,
        }
    }
}
