use std::collections::HashMap;

use crate::llm::message::{
    AssistantContent, Message, Text, ToolCall, ToolResult, ToolResultContent, UserContent,
};

use super::summary::dedupe_paths;
use super::{estimate_message_tokens, FileActivity};

pub(super) fn strip_tool_results(messages: Vec<Message>) -> (Vec<Message>, FileActivity) {
    let file_activity = extract_file_activity(&messages);
    let mut stripped_messages = Vec::with_capacity(messages.len());
    let mut tool_calls = HashMap::new();

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
                        tool_calls.insert(tool_call_key(tool_call), ToolCallInfo::from(tool_call));
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

    (stripped_messages, file_activity)
}

/// Which tool results close each assistant turn.
///
/// Resolution is scoped to the *active turn*, mirroring the `pending_calls`
/// reset in [`drop_orphaned_tool_results`]. A single global set of resolved keys
/// is wrong: when a call id is reused by a later turn, the earlier turn's result
/// would "resolve" it and a dangling call would survive into provider input.
fn resolved_keys_per_turn(messages: &[Message]) -> Vec<std::collections::HashSet<String>> {
    let mut per_turn = vec![std::collections::HashSet::new(); messages.len()];
    let mut active_turn: Option<usize> = None;
    for (index, message) in messages.iter().enumerate() {
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
                // Plain content ends the turn *after* the results it rides with,
                // matching `drop_orphaned_tool_results`.
                if has_plain_content {
                    active_turn = None;
                }
            }
            _ => active_turn = None,
        }
    }
    per_turn
}

pub(super) fn drop_unpaired_tool_calls(messages: Vec<Message>) -> Vec<Message> {
    let resolved_per_turn = resolved_keys_per_turn(&messages);

    let mut kept_messages = Vec::with_capacity(messages.len());
    for (index, message) in messages.into_iter().enumerate() {
        match message {
            Message::Assistant { id, content } => {
                let resolved = &resolved_per_turn[index];
                // Duplicate call keys *within one turn* are malformed:
                // `drop_orphaned_tool_results` pairs through a set, so it closes
                // such a turn with a single result while every duplicate call
                // would survive here — again leaving a dangling call. Keep the
                // first occurrence of each key and drop the rest.
                let mut announced: std::collections::HashSet<String> =
                    std::collections::HashSet::new();
                let kept: Vec<AssistantContent> = content
                    .into_iter()
                    .filter(|item| match item {
                        AssistantContent::ToolCall(tool_call) => {
                            let key = tool_call_key(tool_call);
                            resolved.contains(&key) && announced.insert(key)
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
    let raw_split_index = split_index;
    let mut split_index = pair_safe_boundary(&messages, raw_split_index);

    // The raw budget can land inside an assistant ToolCall / user ToolResult
    // pair. Retreating keeps the pair valid, but an exceptionally large
    // reasoning-bearing assistant turn can make that atomic tail exceed the
    // retention budget by itself. In that case retaining it cannot satisfy the
    // caller's dispatch budget. Summarize the complete tail too, advancing only
    // to the next pair-safe boundary. The generated summary then becomes the
    // caller's next prompt.
    //
    // Keep a sole message intact: there is no earlier conversation to compact,
    // and silently replacing an oversized initial user prompt with a summary
    // would change the request rather than compact its history.
    let retained_tokens = estimate_message_tokens(&messages[split_index..]);
    if split_index < raw_split_index
        && retained_tokens > keep_recent_tokens
        && pair_safe_boundary(&messages, messages.len()) == messages.len()
    {
        split_index = messages.len();
    }

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

/// File activity credited only to tool calls that actually produced a result.
///
/// Classifying from the call alone recorded writes that never happened: a call
/// whose result never arrived because the turn was interrupted or the run
/// crashed still counted as a modification. These lists are persisted on
/// `AgentCompactionEntry` and rendered into later prompts, so a false "Files
/// modified" entry injects state the run never produced. Dry runs are excluded
/// at classification time — see [`ToolCallInfo::from`].
///
/// A failed call that *did* return an error result is still credited: tool
/// results carry no error flag (`gents_protocol::message::ToolResult`), so
/// there is nothing reliable to test. That is a known over-credit, narrower
/// than the previous one.
pub(super) fn extract_file_activity(messages: &[Message]) -> FileActivity {
    let mut activity = FileActivity::default();
    let mut pending: HashMap<String, ToolCallInfo> = HashMap::new();

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

pub(super) fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    if index >= text.len() {
        return text.len();
    }
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// Head and tail of every stub this module writes.
const STUB_HEAD: &str = "[tool: ";
const STUB_TAIL: &str = "see DefraDB AgentToolCall for full output]";
const STUB_JOIN: &str = " — ";
const STUB_TRUNCATED: &str = ", truncated";

/// Markers the truncation layer itself writes (`truncation::logic`,
/// `truncation::spill`). Matching these exactly replaces a `contains("truncated")`
/// sniff that fired on any tool output happening to mention the word.
const TRUNCATION_MARKERS: [&str; 2] = ["[Full output: DefraDB doc ", "[Showing lines "];

/// Facts a previously written stub carries, recovered so that re-stubbing
/// reproduces it exactly.
struct StubFacts {
    byte_count: usize,
    truncated: bool,
}

/// Recover the byte count and truncation flag from stub-shaped text.
///
/// Shape recognition is a *hint*, never a licence to skip the rewrite: tool
/// output is arbitrary text and a command or MCP server can return something
/// with this shape. Misreading such a result costs a wrong byte count in the
/// stub; skipping the rewrite would let its entire payload survive every
/// provider-view pass, which is the thing compaction exists to prevent.
fn parse_stub(tool_result: &ToolResult) -> Option<StubFacts> {
    let [ToolResultContent::Text(text)] = tool_result.content.as_slice() else {
        return None;
    };
    let body = text
        .text
        .strip_prefix(STUB_HEAD)?
        .strip_suffix(STUB_TAIL)?
        .strip_suffix(STUB_JOIN)?;
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

/// Replace a tool result's payload with a pointer stub.
///
/// Always rewrites. When the input is already a stub the facts are recovered
/// rather than re-measured, which makes the rewrite a fixed point — production's
/// half of `Compaction.strip_idempotent`. The request path really does strip
/// twice, once in `agent/daemon/request.rs` and again inside `compact()`, and
/// before this the second pass re-measured the stub and reported *its* length
/// instead of the tool's.
fn strip_tool_result(
    mut tool_result: ToolResult,
    tool_calls: &HashMap<String, ToolCallInfo>,
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
    // The primary path argument is already extracted for `FileActivity`.
    // Carrying it turns the stub from "a file was read" into "this file was
    // read", which is most of what a pointer stub exists to do.
    let argument = info
        .and_then(|info| info.file_path.as_deref())
        .map(|path| format!("({path})"))
        .unwrap_or_default();
    let truncated = if truncated { STUB_TRUNCATED } else { "" };

    let stub = format!(
        "{STUB_HEAD}{tool_name}{argument}, call_id: {call_id}, \
         {byte_count} bytes{truncated}{STUB_JOIN}{STUB_TAIL}"
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
        "read_file" | "list_files" | "glob" | "grep" | "read" | "cat" | "search" | "find" | "query"
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
        let arguments = &tool_call.function.arguments;
        let file_path = arguments
            .get("file_path")
            .or_else(|| arguments.get("path"))
            .and_then(|value| value.as_str())
            .map(ToOwned::to_owned);
        let name = tool_call.function.name.clone();
        // `edit_file` previews a diff and writes nothing under `dry_run`, so it
        // must not be reported as a modification — it did read the file, which
        // is what it gets credited for instead.
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
