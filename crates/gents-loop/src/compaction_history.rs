use rapidhash::{HashMapExt, RapidHashMap};

use gents_protocol::message::{
    AssistantContent, Message, Text, ToolCall, ToolResult, ToolResultContent, UserContent,
};

use super::summary::dedupe_paths;
use super::FileActivity;
use crate::provider_input::ProviderInputProfile;

#[derive(Debug)]
pub struct SourcedMessage {
    pub source_index: usize,
    pub block_indices: Vec<usize>,
    pub message: Message,
}

pub(crate) fn source_messages(messages: Vec<Message>) -> Vec<SourcedMessage> {
    messages
        .into_iter()
        .enumerate()
        .map(|(source_index, message)| SourcedMessage {
            source_index,
            block_indices: match &message {
                Message::Assistant { content, .. } => (0..content.len()).collect(),
                _ => Vec::new(),
            },
            message,
        })
        .collect()
}

fn messages_only(messages: Vec<SourcedMessage>) -> Vec<Message> {
    messages.into_iter().map(|item| item.message).collect()
}

pub fn strip_tool_results(messages: Vec<Message>) -> (Vec<Message>, FileActivity) {
    let (messages, file_activity) = strip_tool_results_sourced(source_messages(messages));
    (messages_only(messages), file_activity)
}

pub fn strip_tool_results_sourced(
    messages: Vec<SourcedMessage>,
) -> (Vec<SourcedMessage>, FileActivity) {
    let file_activity = extract_file_activity_iter(messages.iter().map(|item| &item.message));
    let mut stripped_messages = Vec::with_capacity(messages.len());
    let mut tool_calls = RapidHashMap::new();

    for SourcedMessage {
        source_index,
        block_indices,
        message,
    } in messages
    {
        match message {
            Message::Assistant { id, content } => {
                let opens_turn = content
                    .iter()
                    .any(|item| matches!(item, AssistantContent::ToolCall(_)));
                if opens_turn {
                    tool_calls.clear();
                }

                for item in content.iter() {
                    if let AssistantContent::ToolCall(tool_call) = item {
                        tool_calls.insert(tool_call_key(tool_call), ToolCallInfo::from(tool_call));
                    }
                }

                stripped_messages.push(SourcedMessage {
                    source_index,
                    block_indices,
                    message: Message::Assistant { id, content },
                });
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

                stripped_messages.push(SourcedMessage {
                    source_index,
                    block_indices,
                    message: Message::User { content: items },
                });
            }
            Message::System { content } => {
                stripped_messages.push(SourcedMessage {
                    source_index,
                    block_indices,
                    message: Message::System { content },
                });
            }
        }
    }

    (stripped_messages, file_activity)
}

fn resolved_keys_per_turn<'a>(
    len: usize,
    messages: impl IntoIterator<Item = &'a Message>,
) -> Vec<rapidhash::RapidHashSet<String>> {
    let mut per_turn = vec![rapidhash::RapidHashSet::default(); len];
    let mut active_turn: Option<usize> = None;
    for (index, message) in messages.into_iter().enumerate() {
        match message {
            Message::Assistant { .. } => active_turn = Some(index),
            Message::User { content } => {
                let mut has_plain_content = false;
                for item in content.iter() {
                    match item {
                        UserContent::ToolResult(tool_result) => {
                            if let Some(turn) = active_turn {
                                per_turn[turn].insert(tool_result_key(tool_result));
                            }
                        }
                        _ => has_plain_content = true,
                    }
                }
                if has_plain_content {
                    active_turn = None;
                }
            }
            _ => active_turn = None,
        }
    }
    per_turn
}

pub fn drop_unpaired_tool_calls(messages: Vec<Message>) -> Vec<Message> {
    messages_only(drop_unpaired_tool_calls_sourced(source_messages(messages)))
}

pub fn drop_unpaired_tool_calls_sourced(messages: Vec<SourcedMessage>) -> Vec<SourcedMessage> {
    let resolved_per_turn =
        resolved_keys_per_turn(messages.len(), messages.iter().map(|item| &item.message));

    let mut kept_messages = Vec::with_capacity(messages.len());
    for (index, sourced) in messages.into_iter().enumerate() {
        let SourcedMessage {
            source_index,
            block_indices,
            message,
        } = sourced;
        match message {
            Message::Assistant { id, content } => {
                let resolved = &resolved_per_turn[index];
                let mut announced: rapidhash::RapidHashSet<String> =
                    rapidhash::RapidHashSet::default();
                let mut kept_indices = Vec::new();
                let kept: Vec<AssistantContent> = content
                    .into_iter()
                    .enumerate()
                    .filter_map(|(index, item)| {
                        let keep = match &item {
                            AssistantContent::ToolCall(tool_call) => {
                                let key = tool_call_key(tool_call);
                                resolved.contains(&key) && announced.insert(key)
                            }
                            _ => true,
                        };
                        if keep {
                            kept_indices.extend(block_indices.get(index).copied());
                            Some(item)
                        } else {
                            None
                        }
                    })
                    .collect();
                if !kept.is_empty() {
                    kept_messages.push(SourcedMessage {
                        source_index,
                        block_indices: kept_indices,
                        message: Message::Assistant { id, content: kept },
                    });
                }
            }
            other => kept_messages.push(SourcedMessage {
                source_index,
                block_indices,
                message: other,
            }),
        }
    }
    kept_messages
}

pub fn drop_orphaned_tool_results(messages: Vec<Message>) -> Vec<Message> {
    messages_only(drop_orphaned_tool_results_sourced(source_messages(
        messages,
    )))
}

pub fn drop_orphaned_tool_results_sourced(messages: Vec<SourcedMessage>) -> Vec<SourcedMessage> {
    let mut pending_calls: rapidhash::RapidHashSet<String> = rapidhash::RapidHashSet::default();
    let mut kept_messages = Vec::with_capacity(messages.len());
    for SourcedMessage {
        source_index,
        block_indices,
        message,
    } in messages
    {
        match message {
            Message::Assistant { id, content } => {
                pending_calls.clear();
                for item in content.iter() {
                    if let AssistantContent::ToolCall(tool_call) = item {
                        pending_calls.insert(tool_call_key(tool_call));
                    }
                }
                kept_messages.push(SourcedMessage {
                    source_index,
                    block_indices,
                    message: Message::Assistant { id, content },
                });
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
                    kept_messages.push(SourcedMessage {
                        source_index,
                        block_indices,
                        message: Message::User { content: kept },
                    });
                }
            }
            other => {
                pending_calls.clear();
                kept_messages.push(SourcedMessage {
                    source_index,
                    block_indices,
                    message: other,
                });
            }
        }
    }
    kept_messages
}

pub fn normalize_assistant_content_order(
    profile: ProviderInputProfile,
    messages: Vec<Message>,
) -> Vec<Message> {
    messages_only(normalize_assistant_content_order_sourced(
        profile,
        source_messages(messages),
    ))
}

pub fn normalize_assistant_content_order_sourced(
    profile: ProviderInputProfile,
    messages: Vec<SourcedMessage>,
) -> Vec<SourcedMessage> {
    if profile == ProviderInputProfile::ClaudeMessages {
        return messages;
    }
    messages
        .into_iter()
        .map(|sourced| {
            let SourcedMessage {
                source_index,
                block_indices,
                message,
            } = sourced;
            let mut block_indices = block_indices;
            let message = match message {
                Message::Assistant { id, content } => {
                    let mut text = Vec::new();
                    let mut middle = Vec::new();
                    let mut calls = Vec::new();
                    for (index, item) in content.into_iter().enumerate() {
                        let position = block_indices.get(index).copied();
                        match item {
                            item @ AssistantContent::Text(_) => text.push((position, item)),
                            item @ AssistantContent::ToolCall(_) => calls.push((position, item)),
                            other => middle.push((position, other)),
                        }
                    }
                    let ordered: Vec<_> = text.into_iter().chain(middle).chain(calls).collect();
                    block_indices = ordered.iter().filter_map(|(index, _)| *index).collect();
                    let ordered = ordered.into_iter().map(|(_, item)| item).collect();
                    Message::Assistant {
                        id,
                        content: ordered,
                    }
                }
                other => other,
            };
            SourcedMessage {
                source_index,
                block_indices,
                message,
            }
        })
        .collect()
}

pub fn pretruncate_tool_results(messages: Vec<Message>, max_chars: usize) -> Vec<Message> {
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

pub fn split_messages_for_summary_with_counter(
    messages: Vec<Message>,
    keep_recent_tokens: usize,
    counter: &crate::provider_input::ProviderInputCounter,
) -> anyhow::Result<(Vec<Message>, Vec<Message>)> {
    split_messages_for_summary_with_counter_bounded(messages, keep_recent_tokens, counter, None)
}

pub fn split_messages_for_summary_with_counter_bounded(
    messages: Vec<Message>,
    keep_recent_tokens: usize,
    counter: &crate::provider_input::ProviderInputCounter,
    max_compacted_prefix_messages: Option<usize>,
) -> anyhow::Result<(Vec<Message>, Vec<Message>)> {
    if messages.len() <= 1 {
        if max_compacted_prefix_messages.is_some() {
            return Err(super::ReductionError::CannotFit.into());
        }
        return Ok((Vec::new(), messages));
    }

    let mut low = 0usize;
    let mut high = messages.len() - 1;
    let mut split_index = messages.len() - 1;
    while low <= high {
        let index = low + (high - low) / 2;
        let fits = counter.estimate_message_request(&messages[index..])? <= keep_recent_tokens;
        if fits {
            split_index = index;
            if index == 0 {
                break;
            }
            high = index - 1;
        } else {
            low = index + 1;
        }
    }

    let raw_split_index = split_index;
    let mut split_index = pair_safe_boundary(&messages, raw_split_index);

    if let Some(max_prefix) = max_compacted_prefix_messages {
        split_index = protected_pair_safe_split_index(&messages, raw_split_index, max_prefix)
            .ok_or(super::ReductionError::CannotFit)?;
    }

    let retained_tokens = counter.estimate_message_request(&messages[split_index..])?;
    if max_compacted_prefix_messages.is_none()
        && split_index < raw_split_index
        && retained_tokens > keep_recent_tokens
        && pair_safe_boundary(&messages, messages.len()) == messages.len()
    {
        split_index = messages.len();
    }

    if split_index == 0 {
        return Ok((Vec::new(), messages));
    }

    let old_messages = messages[..split_index].to_vec();
    let recent_messages = messages[split_index..].to_vec();
    Ok((old_messages, recent_messages))
}

pub fn protected_pair_safe_split_index(
    messages: &[Message],
    raw_split_index: usize,
    max_prefix: usize,
) -> Option<usize> {
    let split =
        pair_safe_boundary(messages, raw_split_index).min(pair_safe_boundary(messages, max_prefix));
    (split > 0).then_some(split)
}

pub fn split_messages_for_summary(
    messages: Vec<Message>,
    keep_recent_tokens: usize,
) -> (Vec<Message>, Vec<Message>) {
    let counter = crate::provider_input::ProviderInputCounter::new(
        crate::backend_provider::BackendProviderKind::OpenAiCompatible,
        crate::openai_wire::OpenAiWireApi::ChatCompletions,
        "test-model",
    );
    split_messages_for_summary_with_counter(messages, keep_recent_tokens, &counter)
        .expect("test provider projection should succeed")
}

pub fn pair_safe_boundary(messages: &[Message], limit: usize) -> usize {
    let limit = limit.min(messages.len());
    pair_safe_boundaries(&messages[..limit])
        .last()
        .copied()
        .unwrap_or(0)
}

pub fn pair_safe_boundaries(messages: &[Message]) -> Vec<usize> {
    let mut pending: rapidhash::RapidHashSet<String> = rapidhash::RapidHashSet::default();
    let mut boundaries = Vec::new();

    for (index, message) in messages.iter().enumerate() {
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
                for item in content {
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
        if pending.is_empty() {
            boundaries.push(index + 1);
        }
    }

    boundaries
}

pub fn extract_file_activity(messages: &[Message]) -> FileActivity {
    extract_file_activity_iter(messages.iter())
}

fn extract_file_activity_iter<'a>(messages: impl IntoIterator<Item = &'a Message>) -> FileActivity {
    let mut activity = FileActivity::default();
    let mut pending: RapidHashMap<String, ToolCallInfo> = RapidHashMap::new();

    for message in messages {
        match message {
            Message::Assistant { content, .. } => {
                let opens_turn = content
                    .iter()
                    .any(|item| matches!(item, AssistantContent::ToolCall(_)));
                if opens_turn {
                    pending.clear();
                }
                for item in content.iter() {
                    if let AssistantContent::ToolCall(tool_call) = item {
                        pending.insert(tool_call_key(tool_call), ToolCallInfo::from(tool_call));
                    }
                }
            }
            Message::User { content } => {
                for item in content.iter() {
                    if let UserContent::ToolResult(tool_result) = item {
                        if let Some(info) = pending.get(&tool_result_key(tool_result)) {
                            push_file_activity(&mut activity, info);
                        }
                    }
                }
            }
            Message::System { .. } => {}
        }
    }

    dedupe_paths(&mut activity.files_read);
    dedupe_paths(&mut activity.files_modified);
    activity
}

fn truncate_tool_result_content(content: ToolResultContent, max_chars: usize) -> ToolResultContent {
    match content {
        ToolResultContent::Text(text) if text.text.len() > max_chars => {
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

pub fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    if index >= text.len() {
        return text.len();
    }
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

const STUB_HEAD: &str = "[tool: ";
const STUB_RECOVERY: &str = "full output: sessions ";
const STUB_TAIL: &str = "]";
const STUB_JOIN: &str = " — ";
const STUB_TRUNCATED: &str = ", truncated";

const TRUNCATION_MARKERS: [&str; 4] = crate::truncation::TRUNCATION_NOTICE_PREFIXES;

struct StubFacts {
    byte_count: usize,
    truncated: bool,
}

fn parse_stub(tool_result: &ToolResult) -> Option<StubFacts> {
    let [ToolResultContent::Text(text)] = tool_result.content.as_slice() else {
        return None;
    };
    let (body, _recovery) = text
        .text
        .strip_prefix(STUB_HEAD)?
        .strip_suffix(STUB_TAIL)?
        .rsplit_once(&format!("{STUB_JOIN}{STUB_RECOVERY}"))?;
    let (body, truncated) = match body.strip_suffix(STUB_TRUNCATED) {
        Some(head) => (head, true),
        None => (body, false),
    };
    let byte_count = body
        .rsplit_once(", ")?
        .1
        .strip_suffix(" bytes")?
        .parse()
        .ok()?;
    Some(StubFacts {
        byte_count,
        truncated,
    })
}

fn strip_tool_result(
    mut tool_result: ToolResult,
    tool_calls: &RapidHashMap<String, ToolCallInfo>,
) -> ToolResult {
    let existing = parse_stub(&tool_result);
    let byte_count = existing
        .as_ref()
        .map_or_else(|| tool_result_byte_count(&tool_result), |it| it.byte_count);
    let truncated = existing.as_ref().map_or_else(
        || tool_result_was_truncated(&tool_result),
        |it| it.truncated,
    );

    let call_id = tool_result_key(&tool_result);
    let info = tool_calls.get(&call_id);
    let tool_name = info.map_or("unknown", |info| info.name.as_str());
    let argument = info
        .and_then(|info| info.file_path.as_deref())
        .map(|path| format!("({path})"))
        .unwrap_or_default();
    let truncated = if truncated { STUB_TRUNCATED } else { "" };

    let recovery = serde_json::json!({"action": "output", "call_id": call_id});
    let stub = format!(
        "{STUB_HEAD}{tool_name}{argument}, call_id: {call_id}, \
         {byte_count} bytes{truncated}{STUB_JOIN}{STUB_RECOVERY}{recovery}{STUB_TAIL}"
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

pub fn is_read_tool(name: &str) -> bool {
    matches!(
        name,
        "read_file" | "list_files" | "glob" | "grep" | "read" | "cat" | "search" | "find" | "query"
    )
}

pub fn is_write_tool(name: &str) -> bool {
    matches!(
        name,
        "write_file" | "edit_file" | "write" | "edit" | "replace" | "apply_patch"
    )
}

impl From<&ToolCall> for ToolCallInfo {
    fn from(tool_call: &ToolCall) -> Self {
        let arguments = &tool_call.function.arguments;
        let file_path = arguments
            .get("file_path")
            .or_else(|| arguments.get("path"))
            .and_then(|value| value.as_str())
            .map(ToOwned::to_owned);
        let name = tool_call.function.name.clone();
        // A dry-run preview writes nothing; credit only the read.
        let dry_run = arguments
            .get("dry_run")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        let writes = is_write_tool(&name);

        Self {
            is_read: is_read_tool(&name) || (writes && dry_run),
            is_write: writes && !dry_run,
            name,
            file_path,
        }
    }
}
