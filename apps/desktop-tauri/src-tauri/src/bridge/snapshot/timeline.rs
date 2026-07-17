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
        status_kind: tool_status_kind(tool.lifecycle_state.as_deref().or(tool.status.as_deref())),
        status: tool.status.clone(),
        args: parse_tool_detail_value(tool.args.as_deref()),
        result: parse_tool_detail_value(tool.result.as_deref()),
        denial: tool.denial.clone(),
        cancel_cause: tool.cancel_cause.clone(),
    }
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

fn message_presentation_key(
    message: &MessageView,
    role: &str,
    content: &Option<String>,
    reasoning: &Option<String>,
) -> Option<(i64, String, Option<String>, Option<String>, bool, bool)> {
    message.sequence.map(|sequence| {
        (
            sequence,
            role.to_ascii_lowercase(),
            content.clone(),
            reasoning.clone(),
            message.has_tool_calls,
            message.has_tool_results,
        )
    })
}

fn overlay_matches_latest_assistant(
    timeline: &[RenderedTimelineItem],
    overlay_content: &Option<String>,
    overlay_reasoning: &Option<String>,
) -> bool {
    for item in timeline.iter().rev() {
        match item {
            RenderedTimelineItem::AssistantMessage {
                content, reasoning, ..
            } => return content == overlay_content && reasoning == overlay_reasoning,
            RenderedTimelineItem::ToolGroup { .. } => continue,
            _ => return false,
        }
    }
    false
}

pub(super) fn build_rendered_timeline(
    messages: &[MessageView],
    tool_calls: &[ToolCallView],
    pending_turn: Option<&PendingTurnView>,
    active_response_overlay: Option<&ResponseView>,
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

    let mut seen_message_keys = std::collections::BTreeSet::new();
    let mut seen_presentations = std::collections::BTreeSet::new();

    for message in timeline_messages {
        let role = message
            .display_role
            .as_deref()
            .or(message.role.as_deref())
            .unwrap_or("assistant");
        let normalized_content = normalize_optional(message.display_content.as_deref());
        let normalized_reasoning = normalize_optional(message.reasoning.as_deref());
        if !seen_message_keys.insert(message.message_key.clone()) {
            continue;
        }
        if let Some(presentation_key) =
            message_presentation_key(&message, role, &normalized_content, &normalized_reasoning)
        {
            if !seen_presentations.insert(presentation_key) {
                continue;
            }
        }

        match role {
            "user" => {
                if let Some(content) = normalized_content.clone() {
                    timeline.push(RenderedTimelineItem::UserMessage {
                        item_key: message.message_key.clone(),
                        sequence: message.sequence,
                        content,
                        timestamp: normalize_optional(message.timestamp.as_deref()),
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
                        timestamp: normalize_optional(message.timestamp.as_deref()),
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
            selected_skill_ids: pending_turn.selected_skill_ids.clone(),
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

    let overlay_content =
        active_response_overlay.and_then(|overlay| normalize_optional(overlay.content.as_deref()));
    let overlay_reasoning = active_response_overlay
        .and_then(|overlay| normalize_optional(overlay.reasoning.as_deref()));
    if (overlay_content.is_some() || overlay_reasoning.is_some())
        && !overlay_matches_latest_assistant(&timeline, &overlay_content, &overlay_reasoning)
    {
        timeline.push(RenderedTimelineItem::LiveAssistant {
            item_key: "live-assistant".to_string(),
            content: overlay_content,
            reasoning: overlay_reasoning,
        });
    }

    timeline
}
