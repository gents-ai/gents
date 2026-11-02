use gents_protocol::message::Message;

use super::{history, CompactionError, ExactReduction, FileActivity};
use crate::provider_input::ProviderInputProfile;

pub fn strip_tool_results(messages: Vec<Message>) -> (Vec<Message>, FileActivity) {
    history::strip_tool_results(messages)
}

pub fn sanitize_history_for_provider(
    profile: ProviderInputProfile,
    messages: Vec<Message>,
) -> Vec<Message> {
    history::normalize_assistant_content_order(
        profile,
        history::drop_unpaired_tool_calls(history::drop_orphaned_tool_results(messages)),
    )
}

pub fn provider_view(
    profile: ProviderInputProfile,
    messages: Vec<Message>,
) -> (Vec<Message>, FileActivity) {
    let (stripped, activity) = strip_tool_results(messages);
    (sanitize_history_for_provider(profile, stripped), activity)
}

pub fn provider_view_with_sources(
    profile: ProviderInputProfile,
    messages: Vec<Message>,
) -> (Vec<history::SourcedMessage>, FileActivity) {
    let sourced = history::source_messages(messages);
    let (stripped, activity) = history::strip_tool_results_sourced(sourced);
    let sanitized = sanitize_sourced(profile, stripped);
    (sanitized, activity)
}

pub fn association_view_with_sources(
    messages: Vec<Message>,
) -> (Vec<history::SourcedMessage>, FileActivity) {
    let sourced = history::source_messages(messages);
    let (stripped, activity) = history::strip_tool_results_sourced(sourced);
    (pair_sourced(stripped), activity)
}

pub fn sanitize_association_with_sources(messages: Vec<Message>) -> Vec<history::SourcedMessage> {
    pair_sourced(history::source_messages(messages))
}

fn pair_sourced(sourced: Vec<history::SourcedMessage>) -> Vec<history::SourcedMessage> {
    history::drop_unpaired_tool_calls_sourced(history::drop_orphaned_tool_results_sourced(sourced))
}

fn sanitize_sourced(
    profile: ProviderInputProfile,
    sourced: Vec<history::SourcedMessage>,
) -> Vec<history::SourcedMessage> {
    history::normalize_assistant_content_order_sourced(profile, pair_sourced(sourced))
}

pub fn compacted_through_sequence(
    profile: ProviderInputProfile,
    rows: &[(u32, Message)],
    provider_prefix_len: usize,
) -> Option<u32> {
    if provider_prefix_len == 0 || rows.is_empty() {
        return None;
    }
    let messages = rows
        .iter()
        .map(|(_, message)| message.clone())
        .collect::<Vec<_>>();
    let (sourced_view, _) = provider_view_with_sources(profile, messages.clone());
    if provider_prefix_len > sourced_view.len() {
        return None;
    }

    let split = sourced_view
        .get(provider_prefix_len)
        .map_or(rows.len(), |item| item.source_index);
    if split == 0 {
        return None;
    }
    let (prefix_view, _) = provider_view(profile, messages[..split].to_vec());
    if prefix_view.len() != provider_prefix_len {
        return None;
    }
    let (suffix_view, _) = provider_view(profile, messages[split..].to_vec());
    let full_view = sourced_view
        .iter()
        .map(|item| &item.message)
        .collect::<Vec<_>>();
    if prefix_view.iter().chain(suffix_view.iter()).eq(full_view) {
        Some(rows[split - 1].0)
    } else {
        None
    }
}

pub fn session_cursor_for_reduction(
    profile: ProviderInputProfile,
    rows: &[(u32, Message)],
    prior_provider_prefix: usize,
    reduction: ExactReduction<'_>,
) -> Result<Option<u32>, CompactionError> {
    let durable_messages = rows
        .iter()
        .map(|(_, message)| message.clone())
        .collect::<Vec<_>>();
    let mut full_provider_view = provider_view(profile, durable_messages).0;
    if prior_provider_prefix > full_provider_view.len() {
        return Err(CompactionError::PrefixExceedsHistory);
    }
    let active =
        sanitize_history_for_provider(profile, full_provider_view.split_off(prior_provider_prefix));
    if !reduction
        .compacted_prefix
        .iter()
        .chain(reduction.retained_suffix.iter())
        .eq(active.iter())
    {
        return Err(CompactionError::ExactPartition);
    }

    let cumulative_prefix = prior_provider_prefix
        .checked_add(reduction.compacted_prefix.len())
        .ok_or(CompactionError::PrefixOverflow)?;
    let candidate = compacted_through_sequence(profile, rows, cumulative_prefix);
    Ok(candidate.filter(|cursor| {
        let suffix = rows
            .iter()
            .filter(|(sequence, _)| sequence > cursor)
            .map(|(_, message)| message.clone())
            .collect::<Vec<_>>();
        provider_view(profile, suffix).0 == reduction.retained_suffix
    }))
}

pub fn pair_safe_boundary(messages: &[Message], limit: usize) -> usize {
    history::pair_safe_boundary(messages, limit)
}

pub fn split_for_summary(
    messages: Vec<Message>,
    keep_recent_tokens: usize,
    provider: crate::backend_provider::BackendProviderKind,
    wire: crate::openai_wire::OpenAiWireApi,
    model: &str,
) -> Result<(Vec<Message>, Vec<Message>), CompactionError> {
    let counter = crate::provider_input::ProviderInputCounter::new(provider, wire, model);
    history::split_messages_for_summary_with_counter(messages, keep_recent_tokens, &counter)
}

/// Requires published canonical messages from the session reconstruction owner.
/// `Compaction.admitted_projection_prefix_stable` proves that a closed raw turn
/// preserves its provider projection under later appends. Persisting a reduction
/// additionally requires `session_cursor_for_reduction` to resolve its exact split.
pub fn session_prefix_is_stable(messages: &[Message]) -> bool {
    messages.last().is_some_and(is_ordinary_message)
}

pub fn safe_to_reduce(messages: &[Message]) -> bool {
    session_prefix_is_stable(messages)
        && sanitize_association_with_sources(messages.to_vec())
            .into_iter()
            .map(|sourced| sourced.message)
            .eq(messages.iter().cloned())
}

fn is_ordinary_message(message: &Message) -> bool {
    match message {
        Message::Assistant { content, .. } => !content
            .iter()
            .any(|item| matches!(item, gents_protocol::message::AssistantContent::ToolCall(_))),
        Message::User { content } => !content
            .iter()
            .any(|item| matches!(item, gents_protocol::message::UserContent::ToolResult(_))),
        Message::System { .. } => false,
    }
}
