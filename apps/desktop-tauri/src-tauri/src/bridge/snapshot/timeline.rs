use serde_json::Value;

use super::super::types::{
    normalize_optional, MessageView, PendingTurnView, RenderedTimelineItem, RenderedToolCallView,
    ResponseView, ToolCallView, ToolDetailFieldView, ToolDetailValueView,
};

pub(super) fn normalize_timeline_text(value: Option<&str>) -> String {
    value.map(str::trim).unwrap_or_default().to_string()
}

fn render_json_value(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(value) => value.clone(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        _ => serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string()),
    }
}

fn parse_tool_detail_value(value: Option<&str>) -> Option<ToolDetailValueView> {
    let raw_text = normalize_optional(value)?;
    let parsed = serde_json::from_str::<Value>(&raw_text).ok();
    let fields = match parsed {
        Some(Value::Object(map)) => map
            .into_iter()
            .map(|(key, value)| ToolDetailFieldView {
                key,
                value: render_json_value(&value),
            })
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    };

    Some(ToolDetailValueView { raw_text, fields })
}

fn tool_status_kind(status: Option<&str>) -> String {
    match status.unwrap_or_default().to_ascii_lowercase().as_str() {
        "completed" | "complete" | "success" => "success".to_string(),
        "failed" | "error" | "cancelled" => "error".to_string(),
        _ => "running".to_string(),
    }
}

fn render_tool_call(tool: ToolCallView) -> RenderedToolCallView {
    RenderedToolCallView {
        item_key: tool.tool_call_key.clone(),
        tool_name: tool.tool_name.clone().unwrap_or_else(|| "tool".to_string()),
        status_kind: tool_status_kind(tool.status.as_deref()),
        status: tool.status.clone(),
        args: parse_tool_detail_value(tool.args.as_deref()),
        result: parse_tool_detail_value(tool.result.as_deref()),
    }
}

fn live_overlay_suffix(
    committed_assistant_texts: &[String],
    overlay_text: Option<&str>,
) -> Option<String> {
    let mut remaining = normalize_timeline_text(overlay_text);
    if remaining.is_empty() {
        return None;
    }

    for committed_text in committed_assistant_texts {
        let normalized = committed_text.trim();
        if normalized.is_empty() {
            continue;
        }

        if remaining.starts_with(normalized) {
            remaining = remaining[normalized.len()..].trim_start().to_string();
        }
    }

    (!remaining.is_empty()).then_some(remaining)
}

pub(super) fn materialized_user_turn_count(messages: &[MessageView]) -> usize {
    let mut ordered = messages.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|message| message.sequence.unwrap_or_default());
    ordered
        .into_iter()
        .filter(|message| {
            let role = message
                .display_role
                .as_deref()
                .or(message.role.as_deref())
                .unwrap_or_default();
            role.eq_ignore_ascii_case("user")
                && normalize_optional(message.display_content.as_deref()).is_some()
        })
        .count()
}

fn active_turn_committed_assistant_texts(
    messages: &[MessageView],
    active_turn_index: Option<usize>,
) -> Vec<String> {
    let Some(active_turn_index) = active_turn_index else {
        return Vec::new();
    };

    let mut ordered = messages.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|message| message.sequence.unwrap_or_default());

    let mut current_turn_index = None;
    let mut next_turn_index = 0usize;
    let mut assistant_texts = Vec::new();

    for message in ordered {
        let role = message
            .display_role
            .as_deref()
            .or(message.role.as_deref())
            .unwrap_or_default();
        let content = normalize_optional(message.display_content.as_deref());

        if role.eq_ignore_ascii_case("user") && content.is_some() {
            current_turn_index = Some(next_turn_index);
            next_turn_index += 1;
            continue;
        }

        if role.eq_ignore_ascii_case("assistant") && current_turn_index == Some(active_turn_index) {
            if let Some(content) = content {
                assistant_texts.push(content);
            }
        }
    }

    assistant_texts
}

pub(super) fn build_rendered_timeline(
    messages: &[MessageView],
    tool_calls: &[ToolCallView],
    pending_turn: Option<&PendingTurnView>,
    active_response_overlay: Option<&ResponseView>,
    active_turn_index: Option<usize>,
) -> Vec<RenderedTimelineItem> {
    let mut timeline = Vec::new();
    let mut tool_groups: std::collections::BTreeMap<Option<i64>, Vec<ToolCallView>> =
        std::collections::BTreeMap::new();

    for tool in tool_calls.iter().cloned() {
        tool_groups
            .entry(tool.message_sequence)
            .or_default()
            .push(tool);
    }

    let mut used_group_keys = std::collections::BTreeSet::new();

    let mut timeline_messages = messages
        .iter()
        .filter(|message| {
            !message.has_tool_results
                && (!normalize_timeline_text(message.display_content.as_deref()).is_empty()
                    || !normalize_timeline_text(message.reasoning.as_deref()).is_empty()
                    || message.has_tool_calls)
        })
        .cloned()
        .collect::<Vec<_>>();
    timeline_messages.sort_by_key(|message| message.sequence.unwrap_or_default());

    for message in timeline_messages {
        let role = message
            .display_role
            .as_deref()
            .or(message.role.as_deref())
            .unwrap_or("assistant");
        let normalized_content = normalize_optional(message.display_content.as_deref());
        let normalized_reasoning = normalize_optional(message.reasoning.as_deref());

        match role {
            "user" => {
                if let Some(content) = normalized_content.clone() {
                    timeline.push(RenderedTimelineItem::UserMessage {
                        item_key: message.message_key.clone(),
                        sequence: message.sequence,
                        content,
                    });
                }
            }
            _ => {
                if normalized_content.is_some() || normalized_reasoning.is_some() {
                    timeline.push(RenderedTimelineItem::AssistantMessage {
                        item_key: message.message_key.clone(),
                        sequence: message.sequence,
                        content: normalized_content,
                        reasoning: normalized_reasoning,
                    });
                }
            }
        }

        let key = message.sequence;
        if let Some(grouped_tools) = tool_groups.get(&key).cloned() {
            used_group_keys.insert(key);
            timeline.push(RenderedTimelineItem::ToolGroup {
                item_key: format!("tools-{}", key.unwrap_or(-1)),
                message_sequence: key,
                tools: grouped_tools.into_iter().map(render_tool_call).collect(),
            });
        }
    }

    if let Some(pending_turn) = pending_turn {
        timeline.push(RenderedTimelineItem::PendingUserTurn {
            item_key: format!("pending-{}", pending_turn.request_id),
            request_id: pending_turn.request_id.clone(),
            content: pending_turn.content.clone(),
            lifecycle_state: pending_turn.lifecycle_state.clone(),
            created_at: pending_turn.created_at.clone(),
        });
    }

    for (key, grouped_tools) in tool_groups {
        if used_group_keys.contains(&key) {
            continue;
        }

        timeline.push(RenderedTimelineItem::ToolGroup {
            item_key: format!("tools-{}", key.unwrap_or(-1)),
            message_sequence: key,
            tools: grouped_tools.into_iter().map(render_tool_call).collect(),
        });
    }

    let committed_assistant_texts =
        active_turn_committed_assistant_texts(messages, active_turn_index);
    let overlay_content = live_overlay_suffix(
        &committed_assistant_texts,
        active_response_overlay.and_then(|overlay| overlay.content.as_deref()),
    );
    let overlay_reasoning = active_response_overlay
        .and_then(|overlay| normalize_optional(overlay.reasoning.as_deref()));
    if overlay_content.is_some() || overlay_reasoning.is_some() {
        timeline.push(RenderedTimelineItem::LiveAssistant {
            item_key: "live-assistant".to_string(),
            content: overlay_content,
            reasoning: overlay_reasoning,
        });
    }

    timeline
}
