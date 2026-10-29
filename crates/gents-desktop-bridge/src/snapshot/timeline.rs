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
        node_did: tool.node_did.clone(),
        request_turn: tool.request_turn.clone(),
        background_available: tool.background_available,
        item_key: tool.tool_call_key.clone(),
        tool_call_doc_id: tool.tool_call_doc_id.clone(),
        tool_name: tool.tool_name.clone().unwrap_or_else(|| "tool".to_string()),
        status_kind: tool_status_kind(tool.lifecycle_state.as_deref()),
        request_id: tool.request_id.clone(),
        tool_call_id: tool.tool_call_id.clone(),
        await_mode: tool.await_mode.clone(),
        started_at: tool.started_at.clone(),
        deadline_at: tool.deadline_at.clone(),
        completed_at: tool.completed_at.clone(),
        presentation,
        reconstruction: tool.reconstruction.clone(),
        partial_output_tail: tool.partial_output_tail.clone(),
        denial: tool.denial.clone(),
        cancel_cause: tool.cancel_cause.clone(),
        job_progress: tool.job_progress.clone(),
        job_handle: tool.job_handle.clone(),
        job_state: tool.job_state,
        job_origin: tool.job_origin,
        job_outcome: tool.job_outcome.clone(),
        job_next_calls: tool.job_next_calls.clone(),
    }
}

/// Sequence, lowercased role, content, reasoning and tool call/result flags.
type MessagePresentationKey = (i64, String, Option<String>, Option<String>, bool, bool);

fn message_presentation_key(
    message: &MessageView,
    role: &str,
    content: &Option<String>,
    reasoning: &Option<String>,
) -> Option<MessagePresentationKey> {
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
    pending_turns: &[(PendingTurnView, Option<i64>)],
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
            TimelineSlot::Pending { key } => {
                if let Some((pending_turn, _)) = pending_turns.iter().find(|(turn, _)| {
                    turn.request_doc_id.as_deref().unwrap_or(&turn.request_id) == key
                }) {
                    timeline.push(RenderedTimelineItem::PendingUserTurn {
                        item_key: format!("pending-{}", pending_turn.request_id),
                        started_goal_id: pending_turn.started_goal_id.clone(),
                        request_id: pending_turn.request_id.clone(),
                        content: pending_turn.content.clone(),
                        captured_context: pending_turn.captured_context.clone(),
                        attachments: pending_turn.attachments.clone(),
                        selected_skill_ids: pending_turn.selected_skill_ids.clone(),
                        lifecycle_state: pending_turn.lifecycle_state,
                        folded_into_request_id: pending_turn.folded_into_request_id.clone(),
                        origin: pending_turn.origin.clone(),
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
    pending_turns: &[(PendingTurnView, Option<i64>)],
) -> Vec<RenderedTimelineItem> {
    let mut tool_groups: BTreeMap<Option<i64>, Vec<ToolCallView>> = BTreeMap::new();
    for tool in tool_calls.iter().cloned() {
        tool_groups
            .entry(tool.message_sequence)
            .or_default()
            .push(tool);
    }
    let group_sequences: Vec<Option<i64>> = tool_groups.keys().copied().collect();

    let mut inputs: Vec<TimelineMessageInput> = Vec::new();
    let mut rendered_message: BTreeMap<String, RenderedTimelineItem> = BTreeMap::new();
    for message in messages.iter() {
        let role = message
            .display_role
            .as_deref()
            .or(message.role.as_deref())
            .unwrap_or("assistant");
        let is_user = role.eq_ignore_ascii_case("user");
        let automated = message.origin.clone().filter(|_| is_user);
        let is_background_control = is_user && message.runtime_control;
        let unresolved = message.reconstruction_state != ReconstructionState::Ready;
        let keep = (!is_background_control || automated.is_some())
            && (unresolved
                || (!message.has_tool_results
                    && (!normalize_timeline_text(message.display_content.as_deref()).is_empty()
                        || !normalize_timeline_text(message.reasoning.as_deref()).is_empty()
                        || message.attachments.as_ref().is_some_and(|a| !a.is_empty())
                        || message.has_tool_calls
                        || message
                            .captured_context
                            .as_ref()
                            .is_some_and(|contexts| !contexts.is_empty()))));
        if !keep {
            continue;
        }
        let normalized_content = normalize_optional(message.display_content.as_deref());
        let normalized_reasoning = normalize_optional(message.reasoning.as_deref());
        let reconstruction = MessageReconstructionView {
            state: message.reconstruction_state,
            error: message.reconstruction_error.clone(),
            denied_dependency_doc_id: message.denied_dependency_doc_id.clone(),
        };
        let (emits_item, item) = if let Some(origin) = automated {
            (
                true,
                Some(RenderedTimelineItem::AutomatedInput {
                    item_key: message.message_key.clone(),
                    request_id: message.request_id.clone(),
                    input_request_id: message.input_request_id.clone(),
                    sequence: message.sequence,
                    origin,
                    content: message.input_text.clone(),
                    timestamp: normalize_optional(message.timestamp.as_deref()),
                    reconstruction,
                }),
            )
        } else if is_user {
            match (
                normalized_content.clone(),
                unresolved
                    || message.attachments.as_ref().is_some_and(|a| !a.is_empty())
                    || message
                        .captured_context
                        .as_ref()
                        .is_some_and(|contexts| !contexts.is_empty()),
            ) {
                (Some(_), _) | (None, true) => (
                    true,
                    Some(RenderedTimelineItem::UserMessage {
                        started_goal_id: message.started_goal_id.clone(),
                        item_key: message.message_key.clone(),
                        request_id: message.request_id.clone(),
                        owns_turn: message.owns_turn,
                        input_request_id: message.input_request_id.clone(),
                        sequence: message.sequence,
                        content: normalized_content.clone(),
                        captured_context: message.captured_context.clone(),
                        attachments: message.attachments.clone(),
                        selected_skill_ids: message.selected_skill_ids.clone(),
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
                    request_turn: message.request_turn.clone(),
                    item_key: message.message_key.clone(),
                    request_id: message.request_id.clone(),
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

    let pending = pending_turns
        .iter()
        .map(|(turn, anchor)| PendingInput {
            key: turn
                .request_doc_id
                .clone()
                .unwrap_or_else(|| turn.request_id.clone()),
            placement: anchor
                .and_then(|anchor| {
                    inputs
                        .iter()
                        .filter_map(|input| input.sequence)
                        .filter(|sequence| *sequence >= anchor)
                        .min()
                })
                .map_or(PendingPlacement::Tail, |message_sequence| {
                    PendingPlacement::BeforeMessage { message_sequence }
                }),
        })
        .collect::<Vec<_>>();
    let order = build_timeline_order(&inputs, &group_sequences, &pending, None);
    render_timeline_order(
        order,
        &rendered_message,
        &tool_groups,
        pending_turns,
        &None,
        &None,
    )
}

#[cfg(test)]
mod tests {
    use super::{
        build_rendered_timeline, render_tool_call, MessageReconstructionView, MessageView,
        ReconstructionState, RenderedTimelineItem, ToolCallView,
    };

    fn user_message(key: &str, sequence: i64, content: &str) -> MessageView {
        MessageView {
            started_goal_id: None,
            request_turn: None,
            message_key: key.to_string(),
            request_id: Some("request-1".to_string()),
            owns_turn: false,
            sequence: Some(sequence),
            role: Some("user".to_string()),
            display_role: Some("user".to_string()),
            display_content: Some(content.to_string()),
            captured_context: None,
            attachments: None,
            selected_skill_ids: None,
            reasoning: None,
            has_tool_calls: false,
            has_tool_results: false,
            reconstruction_state: ReconstructionState::Ready,
            reconstruction_error: None,
            denied_dependency_doc_id: None,
            runtime_control: false,
            input_request_id: None,
            origin: None,
            input_text: None,
            timestamp: None,
        }
    }

    #[test]
    fn assistant_projection_retains_each_request_identity_without_guessing_legacy_ownership() {
        let messages = [Some("request-first"), Some("request-second"), None]
            .into_iter()
            .enumerate()
            .map(|(index, request)| {
                let mut message =
                    user_message(&format!("assistant-{index}"), index as i64, "Result");
                message.role = Some("assistant".into());
                message.display_role = Some("assistant".into());
                message.request_id = request.map(str::to_owned);
                message
            })
            .collect::<Vec<_>>();
        let rendered = build_rendered_timeline(&messages, &[], &[]);
        assert_eq!(rendered.len(), 3);
        for (item, expected) in
            rendered
                .iter()
                .zip([Some("request-first"), Some("request-second"), None])
        {
            let RenderedTimelineItem::AssistantMessage { request_id, .. } = item else {
                panic!("expected assistant result");
            };
            assert_eq!(request_id.as_deref(), expected);
        }
    }

    #[test]
    fn session_message_identity_and_await_mode_reach_rendered_tool() {
        let tool = ToolCallView {
            node_did: Some("did:test:row-owner".into()),
            request_turn: Some(crate::types::RequestTurnView {
                request_doc_id: "physical-parent".into(),
                request_id: "parent-1".into(),
                settled: Some(false),
            }),
            background_available: None,
            tool_call_doc_id: Some("physical-tool".into()),
            tool_call_key: "start-1".to_string(),
            request_id: Some("parent-1".to_string()),
            message_sequence: Some(2),
            tool_name: Some(gents::toolset::AGENT_NEW_TOOL_NAME.to_string()),
            tool_call_id: Some("start-1".to_string()),
            args: Some(r#"{"agent":"researcher","prompt":"trace the request flow"}"#.to_string()),
            partial_output_tail: None,
            result: Some(
                r#"{"session_id":"session-child","request_id":"request-child","tool_call_id":"start-1"}"#
                    .to_string(),
            ),
            reconstruction: MessageReconstructionView {
                state: ReconstructionState::Ready,
                error: None,
                denied_dependency_doc_id: None,
            },
            status: Some("running".to_string()),
            lifecycle_state: Some("running".to_string()),
            await_mode: Some("background".to_string()),
            started_at: None,
            deadline_at: None,
            completed_at: None,
            denial: None,
            cancel_cause: None,
            job_progress: None,
            job_handle: None,
            job_state: None,
            job_origin: None,
            job_outcome: None,
            job_next_calls: Vec::new(),
        };
        let rendered = render_tool_call(tool.clone());
        assert_eq!(rendered.request_turn, tool.request_turn);
        assert_eq!(rendered.tool_call_doc_id.as_deref(), Some("physical-tool"));

        assert_eq!(rendered.tool_name, "agent_new");
        assert_eq!(rendered.request_id.as_deref(), Some("parent-1"));
        assert_eq!(rendered.tool_call_id.as_deref(), Some("start-1"));
        assert_eq!(rendered.await_mode.as_deref(), Some("background"));
        assert_eq!(rendered.status_kind, "running");
        match &rendered.presentation {
            crate::types::ToolPresentationView::Agent {
                action,
                name,
                session_id,
                description,
                ..
            } => {
                assert_eq!(action, "start");
                assert_eq!(name.as_deref(), Some("researcher"));
                assert_eq!(session_id.as_deref(), Some("session-child"));
                assert_eq!(description.as_deref(), Some("trace the request flow"));
            }
            other => panic!("agent_new must present as an agent row: {other:?}"),
        }

        let unresolved = render_tool_call(ToolCallView {
            node_did: None,
            request_turn: None,
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
    fn internal_completions_retain_inspectable_inputs_when_their_origin_is_known() {
        for text in [
            "<tool-completion tool_call_id=\"spawned:call\" tool_name=\"bash_unrestricted\" status=\"completed\"><result>finished</result></tool-completion>",
            "<tool-completion tool_call_id=\"spawned:call\" tool_name=\"bash_unrestricted\" status=\"failed\"><result>error: no bin target named `verad`</result><reason>tool_failed</reason></tool-completion>",
            gents::background_completion::BACKGROUND_COMPLETION_WAKE_PROMPT,
        ] {
            let message = MessageView {
                runtime_control: true,
                origin: Some(crate::types::RequestOriginView::BackgroundCompletion),
                input_text: Some(text.into()),
                ..user_message("internal-completion", 1, text)
            };
            assert!(matches!(build_rendered_timeline(&[message], &[], &[]).as_slice(),
                [RenderedTimelineItem::AutomatedInput { content: Some(content), .. }] if content == text));
            assert!(matches!(build_rendered_timeline(&[user_message("person", 1, text)], &[], &[]).as_slice(),
                [RenderedTimelineItem::UserMessage { content: Some(content), .. }] if content == text));
        }
    }

    #[test]
    fn user_text_that_looks_like_a_control_message_still_renders() {
        let content = gents::background_completion::BACKGROUND_COMPLETION_WAKE_PROMPT;
        let messages = vec![user_message("literal-user-text", 1, content)];

        let timeline = build_rendered_timeline(&messages, &[], &[]);

        assert!(matches!(
            &timeline[0],
            RenderedTimelineItem::UserMessage {
                content: rendered,
                ..
            } if rendered.as_deref() == Some(content)
        ));
    }
    #[test]
    fn attachment_only_input_is_visible_without_exposing_runtime_controls() {
        let capture = serde_json::from_value(serde_json::json!({
            "kind":"file", "source":"editor", "capturedAt":"2026-10-20T00:00:00Z",
            "uri":"file:///second/context.rs", "workspaceUri":"file:///second",
            "terminalName":null, "version":41, "unsaved":true, "range":null,
            "content":"captured buffer"
        }))
        .unwrap();
        let visible = MessageView {
            display_content: None,
            captured_context: Some(vec![capture]),
            attachments: None,
            ..user_message("captured-user-input", 1, "")
        };
        let hidden = MessageView {
            runtime_control: true,
            message_key: "runtime-control".into(),
            ..visible.clone()
        };
        let timeline = build_rendered_timeline(&[visible.clone(), hidden], &[], &[]);
        assert!(
            matches!(timeline.as_slice(), [RenderedTimelineItem::UserMessage {content:None, captured_context:Some(contexts), ..}] if contexts.len()==1)
        );
        let empty = MessageView {
            captured_context: Some(Vec::new()),
            attachments: None,
            ..visible
        };
        assert!(build_rendered_timeline(&[empty], &[], &[]).is_empty());
    }
}
