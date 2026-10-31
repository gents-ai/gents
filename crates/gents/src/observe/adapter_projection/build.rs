use gents_protocol::transcript::{present_message, PresentedMessageRole};

use crate::observe::run_timeline::{
    RunTimeline, RunTimelineEvent, TimelineMessageEvent, TimelineRequestEvent,
};

use super::langgraph::build_langgraph_state_history;
use super::redaction::{redact_json_value, redact_option, redact_str};
use super::{
    atif, rendered_capture_events, AdapterProjection, AdapterProjectionEnvelope,
    AdapterProjectionKind, MultiAgentDelegation, MultiAgentMessage, MultiAgentParticipant,
    MultiAgentTaskProjection, MultiAgentToolEvent, OpenAiCodexRunTraceProjection,
    OpenAiCodexTraceItem, ProjectionContext, ProjectionProvenance, ProjectionRedactionMode,
    ProjectionSourceVersionStatus, RenderedRequestProvenanceRef, ADAPTER_PROJECTION_VERSION,
    EXTERNAL_ADAPTER_CAPTURE_PROJECTION_ID, RUN_TIMELINE_PROJECTION_ID,
};

/// Project an external capture without persisting its framework state in requests.
/// Native timeline provenance remains independent of externally asserted mappings.
pub fn build_external_adapter_projection(
    capture: &crate::observe::external_adapter_capture::ExternalAdapterCapture,
    context: &ProjectionContext,
) -> anyhow::Result<AdapterProjectionEnvelope> {
    use crate::observe::external_adapter_capture::import_external_adapter_capture_to_derived_view;
    use crate::observe::external_adapter_capture::langgraph::langgraph_state_history_projection;
    let imported = import_external_adapter_capture_to_derived_view(capture)?;
    let mapping = capture.mapping.as_ref().expect("import validated mapping");
    let view = &imported.view;
    let output = match imported.projection {
        AdapterProjectionKind::LangGraphStateHistory => {
            let mut projection = langgraph_state_history_projection(
                capture,
                mapping,
                view.request
                    .session_id
                    .as_deref()
                    .unwrap_or(&mapping.request_id),
            )?;
            for value in projection.values.values_mut() {
                *value = redact_json_value(value.take(), context);
            }
            for node in &mut projection.nodes {
                node.content = redact_option(node.content.as_deref(), context);
                node.reasoning = redact_option(node.reasoning.as_deref(), context);
                node.node_did = redact_option(node.node_did.as_deref(), context);
            }
            AdapterProjection::LangGraphStateHistory(projection)
        }
        AdapterProjectionKind::MultiAgentTask => {
            let mut participants = Vec::new();
            for participant in &mapping.participants {
                push_participant(
                    &mut participants,
                    redact_option(participant.node_did.as_deref(), context),
                    participant.agent_id.clone(),
                    &participant.role,
                );
            }
            if participants.is_empty() {
                push_participant(
                    &mut participants,
                    redact_option(view.request.node_did.as_deref(), context),
                    view.request.agent_id.clone(),
                    "owner",
                );
            }
            AdapterProjection::MultiAgentTask(MultiAgentTaskProjection {
                task_id: mapping.request_id.clone(),
                context_id: view.request.session_id.clone(),
                status: view
                    .request
                    .lifecycle_state
                    .map(|state| state.as_str().to_string()),
                participants,
                messages: view
                    .messages
                    .iter()
                    .map(|message| {
                        let presented = present_message(&message.message);
                        MultiAgentMessage {
                            id: message.capture_message_label.clone(),
                            request_id: message.request_id.clone(),
                            role: message.source_role.clone(),
                            content: redact_str(&presented.body_markdown, context),
                            reasoning: redact_option(
                                presented.reasoning_markdown.as_deref(),
                                context,
                            ),
                        }
                    })
                    .collect(),
                delegations: view
                    .requests
                    .iter()
                    .filter_map(|request| {
                        Some(MultiAgentDelegation {
                            parent_request_id: request.caused_by_parent_request_id.clone()?,
                            child_request_id: request.request_id.clone(),
                            parent_tool_call_id: request.caused_by_parent_tool_call_id.clone(),
                            node_did: redact_option(request.node_did.as_deref(), context),
                            agent_id: request.agent_id.clone(),
                            status: request
                                .lifecycle_state
                                .map(|state| state.as_str().to_string()),
                            input: redact_option(request.content.as_deref(), context),
                        })
                    })
                    .collect(),
                tool_events: view
                    .tool_calls
                    .iter()
                    .map(|tool| MultiAgentToolEvent {
                        id: tool.tool_call_id.clone(),
                        request_id: tool.request_id.clone(),
                        tool_name: tool.tool_name.clone(),
                        status: tool.status.clone(),
                        selected_service_id: tool.selected_service_id.clone(),
                        selected_tool_name: tool.selected_tool_name.clone(),
                        denial_reason: redact_option(tool.denial_reason.as_deref(), context),
                        child_request_id: tool.child_request_id.clone(),
                    })
                    .collect(),
            })
        }
        _ => unreachable!("import rejects unsupported projections"),
    };
    Ok(AdapterProjectionEnvelope {
        projection_id: imported.projection.id().to_string(),
        projection_version: ADAPTER_PROJECTION_VERSION.to_string(),
        source_request_id: view.request.request_id.clone(),
        source_request_doc_id: None,
        source_session_id: view.request.session_id.clone(),
        source_node_did: redact_option(view.request.node_did.as_deref(), context),
        source_agent_id: view.request.agent_id.clone(),
        redaction_mode: context.redaction_mode,
        provenance: ProjectionProvenance {
            runtime: "gents".to_string(),
            source_projection_id: EXTERNAL_ADAPTER_CAPTURE_PROJECTION_ID.to_string(),
            source_projection_version: ADAPTER_PROJECTION_VERSION.to_string(),
            actor_did: redact_option(imported.actor_did.as_deref(), context),
            source_version_status: ProjectionSourceVersionStatus::ExternalAdapterCapture,
            rendered_request_refs: Vec::new(),
        },
        rendered_captures: Vec::new(),
        output,
    })
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
        source_request_doc_id: timeline.request_doc_id.clone(),
        source_session_id: timeline.session_id.clone(),
        source_node_did: timeline.node_did.clone(),
        source_agent_id: timeline.agent_id.clone(),
        redaction_mode: context.redaction_mode,
        provenance: ProjectionProvenance {
            runtime: "gents".to_string(),
            source_projection_id: RUN_TIMELINE_PROJECTION_ID.to_string(),
            source_projection_version: ADAPTER_PROJECTION_VERSION.to_string(),
            actor_did: context.actor_did.clone(),
            source_version_status: ProjectionSourceVersionStatus::CurrentStateCapturedOnly,
            rendered_request_refs: timeline
                .rendered_request_refs
                .iter()
                .map(|reference| RenderedRequestProvenanceRef {
                    capture_doc_id: match context.redaction_mode {
                        ProjectionRedactionMode::Public => "[redacted]".to_string(),
                        ProjectionRedactionMode::Full | ProjectionRedactionMode::TrainingSafe => {
                            reference.doc_id.clone()
                        }
                    },
                    request_doc_id: reference.request_doc_id.clone(),
                    request_commit_cid: reference.request_commit_cid.clone(),
                })
                .collect(),
        },
        rendered_captures: rendered_capture_events(timeline),
        output: match kind {
            AdapterProjectionKind::AtifTrajectory => {
                AdapterProjection::AtifTrajectory(atif::build_atif_trajectory(timeline, context))
            }
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
                    status: event.lifecycle_state.clone(),
                    node_did: event.node_did.clone(),
                    agent_id: event.agent_id.clone(),
                    parent_request_id: event.parent_request_id.clone(),
                    parent_tool_call_id: event.parent_tool_call_id.clone(),
                    input: timeline_request_input(timeline, event, context),
                    timestamp: event.timestamp.clone(),
                });
            }
            RunTimelineEvent::RenderedRequest(_) => {}
            RunTimelineEvent::InferenceCall(_) => {}
            RunTimelineEvent::Compaction(_) => {}
            RunTimelineEvent::ProviderContextReduction(_) => {}
            RunTimelineEvent::Message(event) => {
                let presented = present_message(&event.message);
                items.push(OpenAiCodexTraceItem::Message {
                    id: format!("{}:message:{}", event.session_id, event.sequence),
                    request_id: event.request_id.clone(),
                    role: presented_role_label(presented.role).to_string(),
                    content: redact_str(&presented.body_markdown, context),
                    reasoning: redact_option(presented.reasoning_markdown.as_deref(), context),
                    timestamp: valid_header_timestamp(event),
                });
            }
            RunTimelineEvent::ToolCall(event) => {
                items.push(OpenAiCodexTraceItem::ToolCall {
                    id: event.tool_call_id.clone(),
                    request_id: event.request_id.clone(),
                    name: event.tool_name.clone(),
                    arguments: redact_str(&event.args, context),
                    output: event
                        .result
                        .as_deref()
                        .map(|result| redact_str(result, context)),
                    status: event.status.clone(),
                    child_run_id: event.child_request_id.clone(),
                    started_at: event.started_at.clone(),
                    completed_at: event.completed_at.clone(),
                });
            }
            RunTimelineEvent::GoalTransition(_) => {}
        }
    }

    OpenAiCodexRunTraceProjection {
        run_id: timeline.request_id.clone(),
        thread_id: timeline.session_id.clone(),
        status: timeline
            .request
            .lifecycle_state
            .map(|s| s.as_str().to_string()),
        items,
        child_run_ids: timeline.child_request_ids.clone(),
    }
}

fn build_multi_agent_task(
    timeline: &RunTimeline,
    context: &ProjectionContext,
) -> MultiAgentTaskProjection {
    let mut participants = Vec::new();
    push_participant(
        &mut participants,
        timeline.node_did.clone(),
        timeline.agent_id.clone(),
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
                    request.node_did.clone(),
                    request.agent_id.clone(),
                    if request.request_id == timeline.request_id {
                        "owner"
                    } else {
                        "delegate"
                    },
                );
                {
                    if let Some(parent_request_id) = request.parent_request_id.as_deref() {
                        delegations.push(MultiAgentDelegation {
                            parent_request_id: parent_request_id.to_string(),
                            child_request_id: request.request_id.clone(),
                            parent_tool_call_id: request.parent_tool_call_id.clone(),
                            node_did: request.node_did.clone(),
                            agent_id: request.agent_id.clone(),
                            status: request.lifecycle_state.clone(),
                            input: redact_option(request.content.as_deref(), context),
                        });
                    }
                }
            }
            RunTimelineEvent::RenderedRequest(_) => {}
            RunTimelineEvent::InferenceCall(_) => {}
            RunTimelineEvent::Compaction(_) => {}
            RunTimelineEvent::ProviderContextReduction(_) => {}
            RunTimelineEvent::Message(message) => {
                let presented = present_message(&message.message);
                messages.push(MultiAgentMessage {
                    id: format!("{}:message:{}", message.session_id, message.sequence),
                    request_id: message.request_id.clone(),
                    role: presented_role_label(presented.role).to_string(),
                    content: redact_str(&presented.body_markdown, context),
                    reasoning: redact_option(presented.reasoning_markdown.as_deref(), context),
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
                    denial_reason: redact_option(tool.denial_reason.as_deref(), context),
                    child_request_id: tool.child_request_id.clone(),
                });
            }
            RunTimelineEvent::GoalTransition(_) => {}
        }
    }

    MultiAgentTaskProjection {
        task_id: timeline.request_id.clone(),
        context_id: timeline.session_id.clone(),
        status: timeline
            .request
            .lifecycle_state
            .map(|s| s.as_str().to_string()),
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
    redact_option(event.content.as_deref(), context).or_else(|| {
        (event.request_id == timeline.request_id)
            .then(|| redact_option(timeline.request.content.as_deref(), context))
            .flatten()
    })
}

pub(super) fn presented_role_label(role: PresentedMessageRole) -> &'static str {
    match role {
        PresentedMessageRole::User => "user",
        PresentedMessageRole::Assistant => "assistant",
        PresentedMessageRole::Tool => "tool",
    }
}

fn valid_header_timestamp(message: &TimelineMessageEvent) -> Option<String> {
    chrono::DateTime::parse_from_rfc3339(&message.header.created_at)
        .is_ok()
        .then(|| message.header.created_at.clone())
}

fn push_participant(
    participants: &mut Vec<MultiAgentParticipant>,
    node_did: Option<String>,
    agent_id: Option<String>,
    role: &str,
) {
    if node_did.is_none() && agent_id.is_none() {
        return;
    }
    if participants.iter().any(|participant| {
        participant.node_did == node_did
            && participant.agent_id == agent_id
            && participant.role == role
    }) {
        return;
    }
    participants.push(MultiAgentParticipant {
        node_did,
        agent_id,
        role: role.to_string(),
    });
}
