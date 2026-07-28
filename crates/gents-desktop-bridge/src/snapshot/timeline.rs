use serde_json::Value;

use std::collections::BTreeMap;

use gents_protocol::timeline::{
    build_timeline_order, OverlayInput, TimelineMessageInput, TimelineRole, TimelineSlot,
};

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
        "failed" | "error" | "cancelled" | "timedout" => "error".to_string(),
        "awaitingapproval" => "awaitingApproval".to_string(),
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
        partial_output_tail: tool.partial_output_tail.clone(),
        partial_output_seq: tool.partial_output_seq,
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
                && !message.runtime_control
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

fn tool_is_nonterminal(tool: &ToolCallView) -> bool {
    matches!(
        tool_status_kind(tool.lifecycle_state.as_deref().or(tool.status.as_deref())).as_str(),
        "running" | "awaitingApproval"
    )
}

fn render_timeline_order(
    order: Vec<TimelineSlot>,
    rendered_messages: &BTreeMap<String, RenderedTimelineItem>,
    tool_groups: &BTreeMap<Option<i64>, Vec<ToolCallView>>,
    pending_turn: Option<&PendingTurnView>,
    overlay_content: &Option<String>,
    overlay_reasoning: &Option<String>,
) -> Vec<RenderedTimelineItem> {
    let mut timeline = Vec::with_capacity(order.len());
    for slot in order {
        match slot {
            TimelineSlot::Message { key, .. } => {
                if let Some(item) = rendered_messages.get(&key) {
                    timeline.push(item.clone());
                }
            }
            TimelineSlot::ToolGroup { message_sequence } => {
                let tools = tool_groups
                    .get(&message_sequence)
                    .cloned()
                    .unwrap_or_default();
                timeline.push(RenderedTimelineItem::ToolGroup {
                    item_key: format!("tools-{}", message_sequence.unwrap_or(-1)),
                    message_sequence,
                    tools: tools.into_iter().map(render_tool_call).collect(),
                });
            }
            TimelineSlot::Pending => {
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
            }
            TimelineSlot::Overlay => {
                if overlay_content.is_some() || overlay_reasoning.is_some() {
                    timeline.push(RenderedTimelineItem::LiveAssistant {
                        item_key: "live-assistant".to_string(),
                        content: overlay_content.clone(),
                        reasoning: overlay_reasoning.clone(),
                    });
                }
            }
        }
    }
    timeline
}

pub(super) fn build_rendered_timeline(
    messages: &[MessageView],
    tool_calls: &[ToolCallView],
    pending_turn: Option<&PendingTurnView>,
    active_response_overlay: Option<&ResponseView>,
    active_response_request_id: Option<&str>,
) -> Vec<RenderedTimelineItem> {
    // Group tool calls by their owning message sequence (rich lookup for the
    // mapping-back step); the presentation-neutral ORDER is decided by the
    // shared, Lean-fenced skeleton, not here.
    let mut tool_groups: BTreeMap<Option<i64>, Vec<ToolCallView>> = BTreeMap::new();
    for tool in tool_calls.iter().cloned() {
        tool_groups
            .entry(tool.message_sequence)
            .or_default()
            .push(tool);
    }
    let group_sequences: Vec<Option<i64>> = tool_groups.keys().copied().collect();

    // Candidate messages (step-2 filter: drop tool-result rows and rows with no
    // rendered content/reasoning/tool-calls — a presentation decision). For each
    // candidate, project the ordering-relevant fields the skeleton consumes, and
    // remember the rich content by key for mapping the slots back.
    let mut inputs: Vec<TimelineMessageInput> = Vec::new();
    let mut rendered_message: BTreeMap<String, RenderedTimelineItem> = BTreeMap::new();
    for message in messages.iter() {
        let role = message
            .display_role
            .as_deref()
            .or(message.role.as_deref())
            .unwrap_or("assistant");
        let is_user = role.eq_ignore_ascii_case("user");
        let is_background_control = is_user && message.runtime_control;
        let keep = !message.has_tool_results
            && !is_background_control
            && (!normalize_timeline_text(message.display_content.as_deref()).is_empty()
                || !normalize_timeline_text(message.reasoning.as_deref()).is_empty()
                || message.has_tool_calls);
        if !keep {
            continue;
        }
        let normalized_content = normalize_optional(message.display_content.as_deref());
        let normalized_reasoning = normalize_optional(message.reasoning.as_deref());
        let (emits_item, item) = if is_user {
            match normalized_content.clone() {
                Some(content) => (
                    true,
                    Some(RenderedTimelineItem::UserMessage {
                        item_key: message.message_key.clone(),
                        sequence: message.sequence,
                        content,
                        timestamp: normalize_optional(message.timestamp.as_deref()),
                    }),
                ),
                None => (false, None),
            }
        } else if normalized_content.is_some() || normalized_reasoning.is_some() {
            (
                true,
                Some(RenderedTimelineItem::AssistantMessage {
                    item_key: message.message_key.clone(),
                    sequence: message.sequence,
                    content: normalized_content.clone(),
                    reasoning: normalized_reasoning.clone(),
                    timestamp: normalize_optional(message.timestamp.as_deref()),
                }),
            )
        } else {
            (false, None)
        };
        if let Some(item) = item {
            rendered_message
                .entry(message.message_key.clone())
                .or_insert(item);
        }
        // Presentation dedup token: the desktop only dedups by presentation when
        // the message carries a sequence (None opts out). Serialize the same
        // tuple the old `message_presentation_key` used, as an opaque token.
        let dedup_token =
            message_presentation_key(message, role, &normalized_content, &normalized_reasoning)
                .map(|key| format!("{key:?}"));
        inputs.push(TimelineMessageInput {
            key: message.message_key.clone(),
            sequence: message.sequence,
            role: if is_user {
                TimelineRole::User
            } else {
                TimelineRole::Assistant
            },
            emits_item,
            dedup_token,
        });
    }

    let overlay_content =
        active_response_overlay.and_then(|overlay| normalize_optional(overlay.content.as_deref()));
    let overlay_reasoning = active_response_overlay
        .and_then(|overlay| normalize_optional(overlay.reasoning.as_deref()));

    // First assemble the durable timeline so the adapter can reduce the rich
    // content comparison to the neutral duplicate bit consumed by the shared
    // ordering contract.
    let base_order = build_timeline_order(&inputs, &group_sequences, pending_turn.is_some(), None);
    let base_timeline = render_timeline_order(
        base_order,
        &rendered_message,
        &tool_groups,
        pending_turn,
        &overlay_content,
        &overlay_reasoning,
    );
    let matches_trailing_assistant =
        overlay_matches_latest_assistant(&base_timeline, &overlay_content, &overlay_reasoning);

    // A tool belonging to this active, unmaterialized response is an orphan
    // until its owning assistant message is persisted. Keep the live reasoning
    // immediately before that running tool instead of letting it jump upward
    // only after materialization.
    let before_orphans = active_response_request_id.is_some_and(|request_id| {
        tool_calls.iter().any(|tool| {
            tool.request_id.as_deref() == Some(request_id)
                && tool_is_nonterminal(tool)
                && !inputs
                    .iter()
                    .any(|message| message.sequence == tool.message_sequence)
        })
    });
    let overlay =
        (overlay_content.is_some() || overlay_reasoning.is_some()).then_some(OverlayInput {
            matches_trailing_assistant,
            before_orphans,
        });
    let order = build_timeline_order(&inputs, &group_sequences, pending_turn.is_some(), overlay);
    render_timeline_order(
        order,
        &rendered_message,
        &tool_groups,
        pending_turn,
        &overlay_content,
        &overlay_reasoning,
    )
}

#[cfg(test)]
mod tests {
    use super::{
        build_rendered_timeline, materialized_user_turn_count, tool_status_kind, MessageView,
        RenderedTimelineItem,
    };

    fn user_message(key: &str, sequence: i64, content: &str) -> MessageView {
        MessageView {
            message_key: key.to_string(),
            sequence: Some(sequence),
            role: Some("user".to_string()),
            content: Some(content.to_string()),
            display_role: Some("user".to_string()),
            display_content: Some(content.to_string()),
            reasoning: None,
            has_tool_calls: false,
            has_tool_results: false,
            runtime_control: false,
            timestamp: None,
        }
    }

    #[test]
    fn status_kind_maps_awaiting_approval_and_terminals() {
        assert_eq!(
            tool_status_kind(Some("awaitingApproval")),
            "awaitingApproval"
        );
        assert_eq!(tool_status_kind(Some("completed")), "success");
        assert_eq!(tool_status_kind(Some("failed")), "error");
        assert_eq!(tool_status_kind(Some("cancelled")), "error");
        assert_eq!(tool_status_kind(Some("timedOut")), "error");
        assert_eq!(tool_status_kind(Some("running")), "running");
        assert_eq!(tool_status_kind(None), "running");
    }

    #[test]
    fn background_completion_controls_do_not_render_as_user_turns() {
        let messages = vec![
            user_message("user-1", 1, "Please classify these sessions."),
            MessageView {
                runtime_control: true,
                ..user_message(
                    "notification",
                    2,
                    r#"<subagent-notification child_request_id="child-1">
<summary>classification complete</summary>
</subagent-notification>"#,
                )
            },
            MessageView {
                runtime_control: true,
                ..user_message(
                    "wake",
                    3,
                    gents::background_completion::BACKGROUND_COMPLETION_WAKE_PROMPT,
                )
            },
        ];

        let timeline = build_rendered_timeline(&messages, &[], None, None, None);

        assert_eq!(materialized_user_turn_count(&messages), 1);
        assert_eq!(timeline.len(), 1);
        assert!(matches!(
            &timeline[0],
            RenderedTimelineItem::UserMessage { content, .. }
                if content == "Please classify these sessions."
        ));
    }

    #[test]
    fn user_text_that_looks_like_a_control_message_still_renders() {
        let content = gents::background_completion::BACKGROUND_COMPLETION_WAKE_PROMPT;
        let messages = vec![user_message("literal-user-text", 1, content)];

        let timeline = build_rendered_timeline(&messages, &[], None, None, None);

        assert_eq!(materialized_user_turn_count(&messages), 1);
        assert!(matches!(
            &timeline[0],
            RenderedTimelineItem::UserMessage {
                content: rendered,
                ..
            } if rendered == content
        ));
    }
}
