use std::collections::BTreeMap;

use gents_protocol::timeline::{
    build_timeline_order, PendingInput, PendingPlacement, TimelineMessageInput, TimelineRole,
    TimelineSlot,
};

use super::super::types::{
    normalize_optional, MessageReconstructionView, MessageView, PendingTurnView,
    ReconstructionState, RenderedTimelineItem, RenderedToolCallView, ToolCallView,
};
use super::tool_presentation::project_tool_presentation;

pub(super) fn normalize_timeline_text(value: Option<&str>) -> String {
    value.map(str::trim).unwrap_or_default().to_string()
}

fn tool_status_kind(status: Option<&str>) -> String {
    match status.unwrap_or_default().to_ascii_lowercase().as_str() {
        "completed" | "complete" | "success" => "success".to_string(),
        "failed" | "error" | "cancelled" | "timedout" => "error".to_string(),
        "pending" | "claimed" | "processing" | "running" => "running".to_string(),
        _ => "unknown".to_string(),
    }
}

fn render_tool_call(tool: ToolCallView) -> RenderedToolCallView {
    let presentation = if tool.reconstruction.state == ReconstructionState::Ready {
        project_tool_presentation(&tool)
    } else {
        let mut unavailable = tool.clone();
        unavailable.args = None;
        unavailable.result = None;
        project_tool_presentation(&unavailable)
    };
    RenderedToolCallView {
        item_key: tool.tool_call_key.clone(),
        tool_name: tool.tool_name.clone().unwrap_or_else(|| "tool".to_string()),
        status_kind: tool_status_kind(tool.lifecycle_state.as_deref()),
        child_request_id: tool.child_request_id.clone(),
        await_mode: tool.await_mode.clone(),
        cancel_policy: tool.cancel_policy.clone(),
        started_at: tool.started_at.clone(),
        deadline_at: tool.deadline_at.clone(),
        completed_at: tool.completed_at.clone(),
        presentation,
        reconstruction: tool.reconstruction.clone(),
        partial_output_tail: tool.partial_output_tail.clone(),
        partial_output_seq: tool.partial_output_seq,
        denial: tool.denial.clone(),
        cancel_cause: tool.cancel_cause.clone(),
    }
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
        let unresolved = message.reconstruction_state != ReconstructionState::Ready;
        let keep = !is_background_control
            && (unresolved
                || (!message.has_tool_results
                    && (!normalize_timeline_text(message.display_content.as_deref()).is_empty()
                        || !normalize_timeline_text(message.reasoning.as_deref()).is_empty()
                        || message.has_tool_calls)));
        if !keep {
            continue;
        }
        let normalized_content = normalize_optional(message.display_content.as_deref());
        let normalized_reasoning = normalize_optional(message.reasoning.as_deref());
        let reconstruction = MessageReconstructionView {
            state: message.reconstruction_state.clone(),
            error: message.reconstruction_error.clone(),
            denied_dependency_doc_id: message.denied_dependency_doc_id.clone(),
        };
        let (emits_item, item) = if is_user {
            match (normalized_content.clone(), unresolved) {
                (Some(_), _) | (None, true) => (
                    true,
                    Some(RenderedTimelineItem::UserMessage {
                        item_key: message.message_key.clone(),
                        request_id: message.request_id.clone(),
                        sequence: message.sequence,
                        content: normalized_content.clone(),
                        timestamp: normalize_optional(message.timestamp.as_deref()),
                        reconstruction,
                    }),
                ),
                (None, false) => (false, None),
            }
        } else if normalized_content.is_some() || normalized_reasoning.is_some() || unresolved {
            (
                true,
                Some(RenderedTimelineItem::AssistantMessage {
                    item_key: message.message_key.clone(),
                    sequence: message.sequence,
                    content: normalized_content.clone(),
                    reasoning: normalized_reasoning.clone(),
                    timestamp: normalize_optional(message.timestamp.as_deref()),
                    reconstruction,
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

    let pending = pending_turn.map(|pending_turn| {
        let first_same_request_assistant = messages
            .iter()
            .filter(|message| {
                pending_turn.request_doc_id.as_deref().is_some()
                    && message.request_id.as_deref() == pending_turn.request_doc_id.as_deref()
                    && message.sequence.is_some()
                    && message
                        .display_role
                        .as_deref()
                        .or(message.role.as_deref())
                        .is_some_and(|role| role.eq_ignore_ascii_case("assistant"))
                    && !message.has_tool_results
                    && !message.runtime_control
                    && (normalize_optional(message.display_content.as_deref()).is_some()
                        || normalize_optional(message.reasoning.as_deref()).is_some()
                        || message.has_tool_calls)
            })
            .min_by_key(|message| {
                message
                    .sequence
                    .map_or((0_i8, 0_i64), |sequence| (1, sequence))
            });
        PendingInput {
            placement: first_same_request_assistant
                .and_then(|message| message.sequence)
                .map_or(PendingPlacement::Tail, |message_sequence| {
                    PendingPlacement::BeforeMessage { message_sequence }
                }),
        }
    });
    let order = build_timeline_order(&inputs, &group_sequences, pending, None);
    render_timeline_order(
        order,
        &rendered_message,
        &tool_groups,
        pending_turn,
        &None,
        &None,
    )
}

#[cfg(test)]
mod tests {
    use super::{
        build_rendered_timeline, render_tool_call, tool_status_kind, MessageReconstructionView,
        MessageView, ReconstructionState, RenderedTimelineItem, ToolCallView,
    };

    fn user_message(key: &str, sequence: i64, content: &str) -> MessageView {
        MessageView {
            message_key: key.to_string(),
            request_id: Some("request-1".to_string()),
            sequence: Some(sequence),
            role: Some("user".to_string()),
            display_role: Some("user".to_string()),
            display_content: Some(content.to_string()),
            reasoning: None,
            has_tool_calls: false,
            has_tool_results: false,
            reconstruction_state: ReconstructionState::Ready,
            reconstruction_error: None,
            denied_dependency_doc_id: None,
            runtime_control: false,
            timestamp: None,
        }
    }

    #[test]
    fn status_kind_maps_active_and_terminal_states() {
        assert_eq!(tool_status_kind(Some("completed")), "success");
        assert_eq!(tool_status_kind(Some("failed")), "error");
        assert_eq!(tool_status_kind(Some("cancelled")), "error");
        assert_eq!(tool_status_kind(Some("timedOut")), "error");
        assert_eq!(tool_status_kind(Some("running")), "running");
        assert_eq!(tool_status_kind(None), "unknown");
    }

    #[test]
    fn subagent_identity_and_await_mode_reach_rendered_tool() {
        let tool = ToolCallView {
            tool_call_key: "spawn-1".to_string(),
            request_id: Some("parent-1".to_string()),
            message_sequence: Some(2),
            tool_name: Some("spawn_subagent".to_string()),
            tool_call_id: Some("spawn-1".to_string()),
            args: Some(
                r#"{"name":"researcher","prompt":"trace the request flow","await_mode":"background"}"#
                    .to_string(),
            ),
            partial_output_tail: Some("reading watcher.rs".to_string()),
            partial_output_seq: Some(18),
            result: None,
            reconstruction: MessageReconstructionView {
                state: ReconstructionState::Ready,
                error: None,
                denied_dependency_doc_id: None,
            },
            status: Some("running".to_string()),
            lifecycle_state: Some("running".to_string()),
            child_request_id: Some("child-request-1".to_string()),
            await_mode: Some("background".to_string()),
            cancel_policy: Some("detach".to_string()),
            started_at: None,
            deadline_at: None,
            completed_at: None,
            denial: None,
            cancel_cause: None,
        };
        let rendered = render_tool_call(tool.clone());

        assert_eq!(rendered.tool_name, "spawn_subagent");
        assert_eq!(
            rendered.child_request_id.as_deref(),
            Some("child-request-1")
        );
        assert_eq!(rendered.await_mode.as_deref(), Some("background"));
        assert_eq!(rendered.status_kind, "running");

        let unresolved = render_tool_call(ToolCallView {
            tool_name: Some("custom".to_string()),
            args: Some("private arguments".to_string()),
            result: Some("partial result".to_string()),
            reconstruction: MessageReconstructionView {
                state: ReconstructionState::Denied,
                error: None,
                denied_dependency_doc_id: Some("denied-segment".to_string()),
            },
            ..tool
        });
        assert_eq!(unresolved.status_kind, "running");
        assert_eq!(unresolved.reconstruction.state, ReconstructionState::Denied);
        assert!(matches!(
            unresolved.presentation,
            crate::types::ToolPresentationView::Generic {
                input: None,
                output: None,
                ..
            }
        ));
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

        let timeline = build_rendered_timeline(&messages, &[], None);

        assert_eq!(timeline.len(), 1);
        assert!(matches!(
            &timeline[0],
            RenderedTimelineItem::UserMessage { content, .. }
                if content.as_deref() == Some("Please classify these sessions.")
        ));
    }

    #[test]
    fn user_text_that_looks_like_a_control_message_still_renders() {
        let content = gents::background_completion::BACKGROUND_COMPLETION_WAKE_PROMPT;
        let messages = vec![user_message("literal-user-text", 1, content)];

        let timeline = build_rendered_timeline(&messages, &[], None);

        assert!(matches!(
            &timeline[0],
            RenderedTimelineItem::UserMessage {
                content: rendered,
                ..
            } if rendered.as_deref() == Some(content)
        ));
    }
}
