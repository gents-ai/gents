use std::collections::{BTreeMap, BTreeSet};

use gents_protocol::transcript::{present_message, PresentedMessageRole};
use serde_json::json;

use crate::observe::run_timeline::{RunTimeline, RunTimelineEvent, TimelineMessageEvent};

use super::build::presented_role_label;
use super::redaction::{redact_option, redact_str};
use super::{
    rendered_captures_json, LangGraphEdge, LangGraphNode, LangGraphStateHistoryProjection,
    LangGraphTask, ProjectionContext, ADAPTER_PROJECTION_VERSION,
};

pub(super) fn build_langgraph_state_history(
    timeline: &RunTimeline,
    context: &ProjectionContext,
) -> LangGraphStateHistoryProjection {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut tasks = Vec::new();
    let mut last_node_id = None::<String>;
    let mut seen_nodes = BTreeSet::<String>::new();
    let mut values = BTreeMap::from([
        ("request_id".to_string(), json!(timeline.request_id)),
        ("session_id".to_string(), json!(timeline.session_id)),
        ("node_did".to_string(), json!(timeline.node_did)),
        ("agent_id".to_string(), json!(timeline.agent_id)),
        (
            "status".to_string(),
            json!(timeline.request.lifecycle_state),
        ),
        (
            "child_request_ids".to_string(),
            json!(timeline.child_request_ids.clone()),
        ),
    ]);

    for event in &timeline.events {
        let (
            node_id,
            kind,
            request_id,
            node_did,
            agent_id,
            parent_request_id,
            parent_tool_call_id,
            status,
            content,
            reasoning,
        ) = match event {
            RunTimelineEvent::RenderedRequest(_) => continue,
            RunTimelineEvent::ProviderContextReduction(event) => (
                format!("provider_context_reduction:{}", event.reduction_key),
                "provider_context_reduction".to_string(),
                Some(event.request_id.clone()),
                None,
                None,
                None,
                None,
                Some("completed".to_string()),
                None,
                None,
            ),
            RunTimelineEvent::Request(event) => (
                format!("request:{}", event.request_id),
                "request".to_string(),
                Some(event.request_id.clone()),
                event.node_did.clone(),
                event.agent_id.clone(),
                event.parent_request_id.clone(),
                event.parent_tool_call_id.clone(),
                event.lifecycle_state.clone(),
                redact_option(event.content.as_deref(), context),
                None,
            ),
            RunTimelineEvent::InferenceCall(event) => (
                format!("inference_call:{}", event.call_id),
                "inference_call".to_string(),
                Some(event.request_id.clone()),
                None,
                None,
                None,
                None,
                Some(event.call_state.clone()),
                None,
                None,
            ),
            RunTimelineEvent::Compaction(event) => (
                format!("compaction:{}", event.compaction_key),
                "compaction".to_string(),
                Some(event.request_id.clone()),
                None,
                None,
                None,
                None,
                Some("completed".to_string()),
                Some(redact_str(&event.summary, context)),
                None,
            ),
            RunTimelineEvent::Message(event) => {
                let presented = present_message(&event.message);
                (
                    format!("message:{}:{}", event.session_id, event.sequence),
                    "message".to_string(),
                    event.request_id.clone(),
                    None,
                    None,
                    None,
                    None,
                    Some(presented_role_label(presented.role).to_string()),
                    Some(redact_str(&presented.body_markdown, context)),
                    redact_option(presented.reasoning_markdown.as_deref(), context),
                )
            }
            RunTimelineEvent::ToolCall(event) => (
                format!("tool_call:{}", event.tool_call_id),
                "tool_call".to_string(),
                event.request_id.clone(),
                None,
                None,
                None,
                None,
                Some(event.status.clone()),
                None,
                None,
            ),
            RunTimelineEvent::GoalTransition(event) => (
                format!("goal_transition:{}", event.commit_cid),
                "goal_transition".to_string(),
                None,
                Some(event.node_did.clone()),
                None,
                None,
                None,
                Some(event.state.status.clone()),
                redact_option(
                    event
                        .completion_evidence
                        .as_deref()
                        .or(event.last_blocked_reason.as_deref())
                        .or(event.last_failure.as_deref()),
                    context,
                ),
                None,
            ),
        };

        if seen_nodes.insert(node_id.clone()) {
            nodes.push(LangGraphNode {
                id: node_id.clone(),
                kind,
                request_id,
                node_did,
                agent_id,
                parent_request_id,
                parent_tool_call_id,
                status,
                content,
                reasoning,
            });
        }
        if let Some(last) = last_node_id.replace(node_id.clone()) {
            edges.push(LangGraphEdge {
                from: last,
                to: node_id.clone(),
                kind: "timeline_order".to_string(),
            });
        }

        if let RunTimelineEvent::Request(request) = event {
            if let Some(parent_request_id) = request.parent_request_id.as_deref() {
                edges.push(LangGraphEdge {
                    from: format!("request:{parent_request_id}"),
                    to: node_id.clone(),
                    kind: "child_request".to_string(),
                });
            }
        }

        if let RunTimelineEvent::ToolCall(tool) = event {
            tasks.push(LangGraphTask {
                id: tool.tool_call_id.clone(),
                request_id: tool.request_id.clone(),
                name: tool.tool_name.clone(),
                status: tool.status.clone(),
                child_request_id: tool.child_request_id.clone(),
            });
            if let Some(child_request_id) = tool.child_request_id.as_deref() {
                edges.push(LangGraphEdge {
                    from: node_id,
                    to: format!("request:{child_request_id}"),
                    kind: "child_request".to_string(),
                });
            }
        }
    }

    if let Some(output) = root_final_output(timeline) {
        values.insert(
            "final_output".to_string(),
            json!(redact_str(&output, context)),
        );
    }
    if let Some(rendered_captures) = rendered_captures_json(timeline) {
        values.insert("rendered_captures".to_string(), rendered_captures);
    }

    let projection = LangGraphStateHistoryProjection {
        thread_id: timeline.session_id.clone(),
        checkpoint_id: format!(
            "gents:{}:{}",
            timeline.request_id, ADAPTER_PROJECTION_VERSION
        ),
        root_request_id: timeline.request_id.clone(),
        values,
        nodes,
        edges,
        tasks,
    };
    projection
}

fn root_final_output(timeline: &RunTimeline) -> Option<String> {
    timeline.events.iter().rev().find_map(|event| match event {
        RunTimelineEvent::Message(message)
            if message.request_id.as_deref() == Some(timeline.request_id.as_str()) =>
        {
            presented_assistant_message(message)
        }
        _ => None,
    })
}

fn presented_assistant_message(message: &TimelineMessageEvent) -> Option<String> {
    let presented = present_message(&message.message);
    (presented.role == PresentedMessageRole::Assistant)
        .then_some(presented.body_markdown)
        .and_then(|content| nonempty_str(Some(&content)).map(ToOwned::to_owned))
}

fn nonempty_str(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}
