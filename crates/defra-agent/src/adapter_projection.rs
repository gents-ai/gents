use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::run_timeline::{
    RunTimeline, RunTimelineEvent, TimelineRequestEvent, TimelineResponseEvent,
};

pub const ADAPTER_PROJECTION_VERSION: &str = "v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterProjectionKind {
    #[serde(rename = "openai_codex_run_trace")]
    OpenAiCodexRunTrace,
    #[serde(rename = "langgraph_state_history")]
    LangGraphStateHistory,
    MultiAgentTask,
}

impl AdapterProjectionKind {
    pub fn id(self) -> &'static str {
        match self {
            Self::OpenAiCodexRunTrace => "openai_codex_run_trace",
            Self::LangGraphStateHistory => "langgraph_state_history",
            Self::MultiAgentTask => "multi_agent_task",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionRedactionMode {
    Full,
    TrainingSafe,
    Public,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionContext {
    pub actor_did: Option<String>,
    pub redaction_mode: ProjectionRedactionMode,
}

impl Default for ProjectionContext {
    fn default() -> Self {
        Self {
            actor_did: None,
            redaction_mode: ProjectionRedactionMode::Full,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdapterProjectionEnvelope {
    pub projection_id: String,
    pub projection_version: String,
    pub source_request_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_agent_did: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_behavior_id: Option<String>,
    pub redaction_mode: ProjectionRedactionMode,
    pub provenance: ProjectionProvenance,
    pub output: AdapterProjection,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionProvenance {
    pub runtime: String,
    pub source_projection_id: String,
    pub source_projection_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actor_did: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "adapter", content = "projection", rename_all = "snake_case")]
pub enum AdapterProjection {
    #[serde(rename = "openai_codex_run_trace")]
    OpenAiCodexRunTrace(OpenAiCodexRunTraceProjection),
    #[serde(rename = "langgraph_state_history")]
    LangGraphStateHistory(LangGraphStateHistoryProjection),
    MultiAgentTask(MultiAgentTaskProjection),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenAiCodexRunTraceProjection {
    pub run_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    pub items: Vec<OpenAiCodexTraceItem>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub child_run_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OpenAiCodexTraceItem {
    Request {
        id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        status: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        lifecycle_state: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        input: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        timestamp: Option<String>,
    },
    Message {
        id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        role: String,
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        timestamp: Option<String>,
    },
    ToolCall {
        id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        name: String,
        arguments: String,
        output: String,
        status: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        child_run_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        started_at: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        completed_at: Option<String>,
    },
    Response {
        id: String,
        status: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        output: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        reasoning: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        timestamp: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LangGraphStateHistoryProjection {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    pub checkpoint_id: String,
    pub root_request_id: String,
    pub values: BTreeMap<String, Value>,
    pub nodes: Vec<LangGraphNode>,
    pub edges: Vec<LangGraphEdge>,
    pub tasks: Vec<LangGraphTask>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LangGraphNode {
    pub id: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LangGraphEdge {
    pub from: String,
    pub to: String,
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LangGraphTask {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    pub name: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub child_request_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MultiAgentTaskProjection {
    pub task_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    pub participants: Vec<MultiAgentParticipant>,
    pub messages: Vec<MultiAgentMessage>,
    pub delegations: Vec<MultiAgentDelegation>,
    pub tool_events: Vec<MultiAgentToolEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultiAgentParticipant {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_did: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub behavior_id: Option<String>,
    pub role: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultiAgentMessage {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultiAgentDelegation {
    pub parent_request_id: String,
    pub child_request_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_did: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub behavior_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultiAgentToolEvent {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    pub tool_name: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_service_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub denial_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub child_request_id: Option<String>,
}

pub fn build_adapter_projection(
    kind: AdapterProjectionKind,
    timeline: &RunTimeline,
    context: &ProjectionContext,
) -> AdapterProjectionEnvelope {
    AdapterProjectionEnvelope {
        projection_id: kind.id().to_string(),
        projection_version: ADAPTER_PROJECTION_VERSION.to_string(),
        source_request_id: timeline.request_id.clone(),
        source_session_id: timeline.session_id.clone(),
        source_agent_did: timeline.agent_did.clone(),
        source_behavior_id: timeline.behavior_id.clone(),
        redaction_mode: context.redaction_mode,
        provenance: ProjectionProvenance {
            runtime: "defra-agent".to_string(),
            source_projection_id: "run_timeline".to_string(),
            source_projection_version: ADAPTER_PROJECTION_VERSION.to_string(),
            actor_did: context.actor_did.clone(),
        },
        output: match kind {
            AdapterProjectionKind::OpenAiCodexRunTrace => AdapterProjection::OpenAiCodexRunTrace(
                build_openai_codex_run_trace(timeline, context),
            ),
            AdapterProjectionKind::LangGraphStateHistory => {
                AdapterProjection::LangGraphStateHistory(build_langgraph_state_history(
                    timeline, context,
                ))
            }
            AdapterProjectionKind::MultiAgentTask => {
                AdapterProjection::MultiAgentTask(build_multi_agent_task(timeline, context))
            }
        },
    }
}

fn build_openai_codex_run_trace(
    timeline: &RunTimeline,
    context: &ProjectionContext,
) -> OpenAiCodexRunTraceProjection {
    let mut items = Vec::new();
    for event in &timeline.events {
        match event {
            RunTimelineEvent::Request(event) => {
                items.push(OpenAiCodexTraceItem::Request {
                    id: event.request_id.clone(),
                    status: event.status.clone(),
                    lifecycle_state: event.lifecycle_state.clone(),
                    input: timeline_request_input(timeline, event, context),
                    timestamp: event.timestamp.clone(),
                });
            }
            RunTimelineEvent::Message(event) => {
                items.push(OpenAiCodexTraceItem::Message {
                    id: format!("{}:message:{}", event.session_id, event.sequence),
                    request_id: event.request_id.clone(),
                    role: event.role.clone(),
                    content: redact_str(&event.content, context),
                    timestamp: event.timestamp.clone(),
                });
            }
            RunTimelineEvent::ToolCall(event) => {
                items.push(OpenAiCodexTraceItem::ToolCall {
                    id: event.tool_call_id.clone(),
                    request_id: event.request_id.clone(),
                    name: event.tool_name.clone(),
                    arguments: redact_str(&event.args, context),
                    output: redact_str(&event.result, context),
                    status: event.status.clone(),
                    child_run_id: event.child_request_id.clone(),
                    started_at: event.started_at.clone(),
                    completed_at: event.completed_at.clone(),
                });
            }
            RunTimelineEvent::Response(event) => {
                items.push(OpenAiCodexTraceItem::Response {
                    id: event.request_id.clone(),
                    status: event.status.clone(),
                    output: redact_option(event.content.as_deref(), context),
                    reasoning: redact_option(event.reasoning.as_deref(), context),
                    error: redact_option(event.error_message.as_deref(), context),
                    timestamp: event.timestamp.clone(),
                });
            }
        }
    }

    OpenAiCodexRunTraceProjection {
        run_id: timeline.request_id.clone(),
        thread_id: timeline.session_id.clone(),
        status: timeline
            .request
            .lifecycle_state
            .clone()
            .or(timeline.request.status.clone()),
        items,
        child_run_ids: timeline.child_request_ids.clone(),
    }
}

fn build_langgraph_state_history(
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
        ("agent_did".to_string(), json!(timeline.agent_did)),
        ("behavior_id".to_string(), json!(timeline.behavior_id)),
        ("status".to_string(), json!(timeline.request.status)),
        (
            "lifecycle_state".to_string(),
            json!(timeline.request.lifecycle_state),
        ),
        (
            "child_request_ids".to_string(),
            json!(timeline.child_request_ids),
        ),
    ]);

    for event in &timeline.events {
        let (node_id, kind, request_id, status) = match event {
            RunTimelineEvent::Request(event) => (
                format!("request:{}", event.request_id),
                "request".to_string(),
                Some(event.request_id.clone()),
                event.lifecycle_state.clone().or(event.status.clone()),
            ),
            RunTimelineEvent::Message(event) => (
                format!("message:{}:{}", event.session_id, event.sequence),
                "message".to_string(),
                event.request_id.clone(),
                Some(event.role.clone()),
            ),
            RunTimelineEvent::ToolCall(event) => (
                format!("tool_call:{}", event.tool_call_id),
                "tool_call".to_string(),
                event.request_id.clone(),
                Some(event.status.clone()),
            ),
            RunTimelineEvent::Response(event) => (
                format!("response:{}", event.request_id),
                "response".to_string(),
                Some(event.request_id.clone()),
                event.status.clone(),
            ),
        };

        if seen_nodes.insert(node_id.clone()) {
            nodes.push(LangGraphNode {
                id: node_id.clone(),
                kind,
                request_id,
                status,
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

    if let Some(response) = last_response(timeline) {
        values.insert(
            "final_output".to_string(),
            json!(redact_option(response.content.as_deref(), context)),
        );
    }

    LangGraphStateHistoryProjection {
        thread_id: timeline.session_id.clone(),
        checkpoint_id: format!(
            "defra:{}:{}",
            timeline.request_id, ADAPTER_PROJECTION_VERSION
        ),
        root_request_id: timeline.request_id.clone(),
        values,
        nodes,
        edges,
        tasks,
    }
}

fn build_multi_agent_task(
    timeline: &RunTimeline,
    context: &ProjectionContext,
) -> MultiAgentTaskProjection {
    let mut participants = Vec::new();
    push_participant(
        &mut participants,
        timeline.agent_did.clone(),
        timeline.behavior_id.clone(),
        "owner",
    );
    let mut messages = Vec::new();
    let mut delegations = Vec::new();
    let mut tool_events = Vec::new();

    for event in &timeline.events {
        match event {
            RunTimelineEvent::Request(request) => {
                push_participant(
                    &mut participants,
                    request.agent_did.clone(),
                    request.behavior_id.clone(),
                    if request.request_id == timeline.request_id {
                        "owner"
                    } else {
                        "delegate"
                    },
                );
                if let Some(parent_request_id) = request.parent_request_id.as_deref() {
                    delegations.push(MultiAgentDelegation {
                        parent_request_id: parent_request_id.to_string(),
                        child_request_id: request.request_id.clone(),
                        parent_tool_call_id: request.parent_tool_call_id.clone(),
                        agent_did: request.agent_did.clone(),
                        behavior_id: request.behavior_id.clone(),
                        status: request.lifecycle_state.clone().or(request.status.clone()),
                    });
                }
            }
            RunTimelineEvent::Message(message) => {
                messages.push(MultiAgentMessage {
                    id: format!("{}:message:{}", message.session_id, message.sequence),
                    request_id: message.request_id.clone(),
                    role: message.role.clone(),
                    content: redact_str(&message.content, context),
                });
            }
            RunTimelineEvent::ToolCall(tool) => {
                tool_events.push(MultiAgentToolEvent {
                    id: tool.tool_call_id.clone(),
                    request_id: tool.request_id.clone(),
                    tool_name: tool.tool_name.clone(),
                    status: tool.status.clone(),
                    selected_service_id: tool.selected_service_id.clone(),
                    selected_tool_name: tool.selected_tool_name.clone(),
                    denial_reason: tool.denial_reason.clone(),
                    child_request_id: tool.child_request_id.clone(),
                });
            }
            RunTimelineEvent::Response(_) => {}
        }
    }

    MultiAgentTaskProjection {
        task_id: timeline.request_id.clone(),
        context_id: timeline.session_id.clone(),
        status: timeline
            .request
            .lifecycle_state
            .clone()
            .or(timeline.request.status.clone()),
        participants,
        messages,
        delegations,
        tool_events,
    }
}

fn timeline_request_input(
    timeline: &RunTimeline,
    event: &TimelineRequestEvent,
    context: &ProjectionContext,
) -> Option<String> {
    if event.request_id == timeline.request_id {
        redact_option(timeline.request.content.as_deref(), context)
    } else {
        None
    }
}

fn last_response(timeline: &RunTimeline) -> Option<&TimelineResponseEvent> {
    timeline.events.iter().rev().find_map(|event| match event {
        RunTimelineEvent::Response(response) => Some(response),
        _ => None,
    })
}

fn push_participant(
    participants: &mut Vec<MultiAgentParticipant>,
    agent_did: Option<String>,
    behavior_id: Option<String>,
    role: &str,
) {
    if agent_did.is_none() && behavior_id.is_none() {
        return;
    }
    if participants.iter().any(|participant| {
        participant.agent_did == agent_did
            && participant.behavior_id == behavior_id
            && participant.role == role
    }) {
        return;
    }
    participants.push(MultiAgentParticipant {
        agent_did,
        behavior_id,
        role: role.to_string(),
    });
}

fn redact_option(value: Option<&str>, context: &ProjectionContext) -> Option<String> {
    value.map(|value| redact_str(value, context))
}

fn redact_str(value: &str, context: &ProjectionContext) -> String {
    match context.redaction_mode {
        ProjectionRedactionMode::Full => value.to_string(),
        ProjectionRedactionMode::TrainingSafe => redact_training_safe(value),
        ProjectionRedactionMode::Public => "[redacted]".to_string(),
    }
}

fn redact_training_safe(value: &str) -> String {
    if value.trim().is_empty() {
        String::new()
    } else {
        "[training_safe_redacted]".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_timeline::{
        build_run_timeline, RunTimelineRows, TimelineMessageRow, TimelineRequestRow,
        TimelineResponseRow, TimelineToolCallRow,
    };

    #[test]
    fn builds_three_adapter_shapes_from_one_timeline_with_redaction() {
        let timeline = build_run_timeline(RunTimelineRows {
            request: TimelineRequestRow {
                request_id: "req-1".to_string(),
                agent_did: Some("did:defra-agent:root".to_string()),
                behavior_id: Some("root".to_string()),
                session_id: Some("session-1".to_string()),
                content: Some("sensitive prompt".to_string()),
                status: Some("completed".to_string()),
                lifecycle_state: Some("completed".to_string()),
                created_at: Some("2026-06-05T00:00:00Z".to_string()),
                ..TimelineRequestRow::default()
            },
            requests: vec![TimelineRequestRow {
                request_id: "child-1".to_string(),
                agent_did: Some("did:defra-agent:child".to_string()),
                behavior_id: Some("child".to_string()),
                session_id: Some("session-1".to_string()),
                status: Some("completed".to_string()),
                lifecycle_state: Some("completed".to_string()),
                caused_by_parent_request_id: Some("req-1".to_string()),
                caused_by_parent_tool_call_id: Some("call-child".to_string()),
                created_at: Some("2026-06-05T00:00:03Z".to_string()),
                ..TimelineRequestRow::default()
            }],
            messages: vec![TimelineMessageRow {
                session_id: "session-1".to_string(),
                sequence: 1,
                role: "assistant".to_string(),
                content: "sensitive assistant text".to_string(),
                timestamp: Some("2026-06-05T00:00:01Z".to_string()),
            }],
            tool_calls: vec![TimelineToolCallRow {
                request_id: Some("req-1".to_string()),
                session_id: "session-1".to_string(),
                message_sequence: Some(1),
                tool_name: "delegate".to_string(),
                tool_call_id: "call-child".to_string(),
                args: "{\"prompt\":\"secret\"}".to_string(),
                result: "{\"ok\":true}".to_string(),
                status: "completed".to_string(),
                child_request_id: Some("child-1".to_string()),
                started_at: Some("2026-06-05T00:00:02Z".to_string()),
                completed_at: Some("2026-06-05T00:00:03Z".to_string()),
                ..TimelineToolCallRow::default()
            }],
            responses: vec![TimelineResponseRow {
                request_id: "req-1".to_string(),
                session_id: Some("session-1".to_string()),
                content: Some("sensitive final".to_string()),
                status: Some("completed".to_string()),
                completed_at: Some("2026-06-05T00:00:04Z".to_string()),
                ..TimelineResponseRow::default()
            }],
            ..RunTimelineRows::default()
        });
        let context = ProjectionContext {
            actor_did: Some("did:defra-agent:viewer".to_string()),
            redaction_mode: ProjectionRedactionMode::Public,
        };

        let codex = build_adapter_projection(
            AdapterProjectionKind::OpenAiCodexRunTrace,
            &timeline,
            &context,
        );
        let langgraph = build_adapter_projection(
            AdapterProjectionKind::LangGraphStateHistory,
            &timeline,
            &context,
        );
        let multi =
            build_adapter_projection(AdapterProjectionKind::MultiAgentTask, &timeline, &context);

        assert_eq!(codex.projection_id, "openai_codex_run_trace");
        assert_eq!(langgraph.projection_id, "langgraph_state_history");
        assert_eq!(multi.projection_id, "multi_agent_task");
        assert!(!serde_json::to_string(&codex)
            .unwrap()
            .contains("sensitive prompt"));
        assert!(serde_json::to_string(&langgraph)
            .unwrap()
            .contains("child_request"));
        assert!(serde_json::to_string(&multi)
            .unwrap()
            .contains("\"role\":\"delegate\""));
    }
}
