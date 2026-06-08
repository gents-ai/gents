use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::run_timeline::{
    RunTimeline, RunTimelineEvent, TimelineRequestEvent, TimelineResponseEvent,
};

pub const ADAPTER_PROJECTION_VERSION: &str = "v1";
pub const RUN_TIMELINE_PROJECTION_ID: &str = "run_timeline";

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

    pub fn title(self) -> &'static str {
        match self {
            Self::OpenAiCodexRunTrace => "OpenAI/Codex Run Trace Projection",
            Self::LangGraphStateHistory => "LangGraph State History Projection",
            Self::MultiAgentTask => "Multi-Agent Task Projection",
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

impl AdapterProjection {
    pub fn kind(&self) -> AdapterProjectionKind {
        match self {
            Self::OpenAiCodexRunTrace(_) => AdapterProjectionKind::OpenAiCodexRunTrace,
            Self::LangGraphStateHistory(_) => AdapterProjectionKind::LangGraphStateHistory,
            Self::MultiAgentTask(_) => AdapterProjectionKind::MultiAgentTask,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdapterProjectionJsonlRecord {
    pub projection_id: String,
    pub projection_version: String,
    pub source_request_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_session_id: Option<String>,
    pub redaction_mode: ProjectionRedactionMode,
    pub record_kind: String,
    pub record_index: usize,
    pub record_id: String,
    pub value: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdapterProjectionEvalJsonlRecord {
    pub projection_id: String,
    pub projection_version: String,
    pub source_request_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_session_id: Option<String>,
    pub redaction_mode: ProjectionRedactionMode,
    pub record_index: usize,
    pub record_id: String,
    pub sample_kind: String,
    pub adapter_record_kind: String,
    pub adapter_record_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub child_request_id: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdapterProjectionContractError {
    pub violations: Vec<String>,
}

impl AdapterProjectionContractError {
    fn from_violations(violations: Vec<String>) -> Self {
        Self { violations }
    }
}

impl fmt::Display for AdapterProjectionContractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "adapter projection contract failed: {}",
            self.violations.join("; ")
        )
    }
}

impl std::error::Error for AdapterProjectionContractError {}

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
            source_projection_id: RUN_TIMELINE_PROJECTION_ID.to_string(),
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

pub fn validate_adapter_projection_contract(
    envelope: &AdapterProjectionEnvelope,
) -> Result<(), AdapterProjectionContractError> {
    let mut violations = Vec::new();
    require_nonempty(
        &mut violations,
        "projection_id",
        envelope.projection_id.as_str(),
    );
    require_nonempty(
        &mut violations,
        "projection_version",
        envelope.projection_version.as_str(),
    );
    require_nonempty(
        &mut violations,
        "source_request_id",
        envelope.source_request_id.as_str(),
    );
    require_nonempty(
        &mut violations,
        "provenance.runtime",
        envelope.provenance.runtime.as_str(),
    );
    require_eq(
        &mut violations,
        "provenance.source_projection_id",
        envelope.provenance.source_projection_id.as_str(),
        RUN_TIMELINE_PROJECTION_ID,
    );
    require_eq(
        &mut violations,
        "projection_id",
        envelope.projection_id.as_str(),
        envelope.output.kind().id(),
    );
    require_eq(
        &mut violations,
        "projection_version",
        envelope.projection_version.as_str(),
        ADAPTER_PROJECTION_VERSION,
    );

    match &envelope.output {
        AdapterProjection::OpenAiCodexRunTrace(projection) => {
            validate_openai_codex_projection(&mut violations, projection);
        }
        AdapterProjection::LangGraphStateHistory(projection) => {
            validate_langgraph_projection(&mut violations, projection);
        }
        AdapterProjection::MultiAgentTask(projection) => {
            validate_multi_agent_projection(&mut violations, projection);
        }
    }

    if violations.is_empty() {
        Ok(())
    } else {
        Err(AdapterProjectionContractError::from_violations(violations))
    }
}

pub fn adapter_projection_jsonl_records(
    envelope: &AdapterProjectionEnvelope,
) -> Vec<AdapterProjectionJsonlRecord> {
    match &envelope.output {
        AdapterProjection::OpenAiCodexRunTrace(projection) => projection
            .items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                jsonl_record(
                    envelope,
                    "openai_codex_trace_item",
                    index,
                    openai_item_id(item),
                    serde_json::to_value(item).unwrap_or(Value::Null),
                )
            })
            .collect(),
        AdapterProjection::LangGraphStateHistory(projection) => {
            let mut records = Vec::new();
            records.push(jsonl_record(
                envelope,
                "langgraph_values",
                records.len(),
                projection.checkpoint_id.clone(),
                serde_json::to_value(&projection.values).unwrap_or(Value::Null),
            ));
            for node in &projection.nodes {
                records.push(jsonl_record(
                    envelope,
                    "langgraph_node",
                    records.len(),
                    node.id.clone(),
                    serde_json::to_value(node).unwrap_or(Value::Null),
                ));
            }
            for edge in &projection.edges {
                records.push(jsonl_record(
                    envelope,
                    "langgraph_edge",
                    records.len(),
                    format!("{}->{}:{}", edge.from, edge.to, edge.kind),
                    serde_json::to_value(edge).unwrap_or(Value::Null),
                ));
            }
            for task in &projection.tasks {
                records.push(jsonl_record(
                    envelope,
                    "langgraph_task",
                    records.len(),
                    task.id.clone(),
                    serde_json::to_value(task).unwrap_or(Value::Null),
                ));
            }
            records
        }
        AdapterProjection::MultiAgentTask(projection) => {
            let mut records = Vec::new();
            for participant in &projection.participants {
                records.push(jsonl_record(
                    envelope,
                    "multi_agent_participant",
                    records.len(),
                    participant
                        .agent_did
                        .clone()
                        .or_else(|| participant.behavior_id.clone())
                        .unwrap_or_else(|| participant.role.clone()),
                    serde_json::to_value(participant).unwrap_or(Value::Null),
                ));
            }
            for message in &projection.messages {
                records.push(jsonl_record(
                    envelope,
                    "multi_agent_message",
                    records.len(),
                    message.id.clone(),
                    serde_json::to_value(message).unwrap_or(Value::Null),
                ));
            }
            for delegation in &projection.delegations {
                records.push(jsonl_record(
                    envelope,
                    "multi_agent_delegation",
                    records.len(),
                    format!(
                        "{}->{}",
                        delegation.parent_request_id, delegation.child_request_id
                    ),
                    serde_json::to_value(delegation).unwrap_or(Value::Null),
                ));
            }
            for tool_event in &projection.tool_events {
                records.push(jsonl_record(
                    envelope,
                    "multi_agent_tool_event",
                    records.len(),
                    tool_event.id.clone(),
                    serde_json::to_value(tool_event).unwrap_or(Value::Null),
                ));
            }
            records
        }
    }
}

pub fn adapter_projection_eval_jsonl_records(
    envelope: &AdapterProjectionEnvelope,
) -> Vec<AdapterProjectionEvalJsonlRecord> {
    let mut records = Vec::new();
    match &envelope.output {
        AdapterProjection::OpenAiCodexRunTrace(projection) => {
            for item in &projection.items {
                match item {
                    OpenAiCodexTraceItem::Request {
                        id,
                        status,
                        lifecycle_state,
                        input,
                        timestamp,
                    } => records.push(eval_record(
                        envelope,
                        records.len(),
                        "prompt",
                        "openai_codex_trace_item",
                        id,
                        EvalRecordFields {
                            input: input.clone(),
                            status: lifecycle_state.clone().or(status.clone()),
                            metadata: metadata([("timestamp", timestamp.clone())]),
                            ..EvalRecordFields::default()
                        },
                    )),
                    OpenAiCodexTraceItem::Message {
                        id,
                        role,
                        content,
                        timestamp,
                        ..
                    } => records.push(eval_record(
                        envelope,
                        records.len(),
                        "message",
                        "openai_codex_trace_item",
                        id,
                        EvalRecordFields {
                            role: Some(role.clone()),
                            output: Some(content.clone()),
                            metadata: metadata([("timestamp", timestamp.clone())]),
                            ..EvalRecordFields::default()
                        },
                    )),
                    OpenAiCodexTraceItem::ToolCall {
                        id,
                        name,
                        arguments,
                        output,
                        status,
                        child_run_id,
                        started_at,
                        completed_at,
                        ..
                    } => records.push(eval_record(
                        envelope,
                        records.len(),
                        "tool_call",
                        "openai_codex_trace_item",
                        id,
                        EvalRecordFields {
                            input: Some(arguments.clone()),
                            output: Some(output.clone()),
                            tool_name: Some(name.clone()),
                            status: Some(status.clone()),
                            child_request_id: child_run_id.clone(),
                            metadata: metadata([
                                ("started_at", started_at.clone()),
                                ("completed_at", completed_at.clone()),
                            ]),
                            ..EvalRecordFields::default()
                        },
                    )),
                    OpenAiCodexTraceItem::Response {
                        id,
                        status,
                        output,
                        reasoning,
                        error,
                        timestamp,
                    } => records.push(eval_record(
                        envelope,
                        records.len(),
                        "response",
                        "openai_codex_trace_item",
                        id,
                        EvalRecordFields {
                            output: output.clone(),
                            status: status.clone(),
                            metadata: metadata([
                                ("reasoning", reasoning.clone()),
                                ("error", error.clone()),
                                ("timestamp", timestamp.clone()),
                            ]),
                            ..EvalRecordFields::default()
                        },
                    )),
                }
            }
        }
        AdapterProjection::LangGraphStateHistory(projection) => {
            let output = projection
                .values
                .get("final_output")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            records.push(eval_record(
                envelope,
                records.len(),
                "state_snapshot",
                "langgraph_values",
                &projection.checkpoint_id,
                EvalRecordFields {
                    output,
                    status: projection
                        .values
                        .get("status")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned),
                    metadata: projection.values.clone(),
                    ..EvalRecordFields::default()
                },
            ));
            for node in &projection.nodes {
                records.push(eval_record(
                    envelope,
                    records.len(),
                    "state_node",
                    "langgraph_node",
                    &node.id,
                    EvalRecordFields {
                        status: node.status.clone(),
                        metadata: metadata([
                            ("kind", Some(node.kind.clone())),
                            ("request_id", node.request_id.clone()),
                        ]),
                        ..EvalRecordFields::default()
                    },
                ));
            }
            for edge in &projection.edges {
                let mut edge_metadata = BTreeMap::new();
                edge_metadata.insert("from".to_string(), json!(edge.from));
                edge_metadata.insert("to".to_string(), json!(edge.to));
                edge_metadata.insert("kind".to_string(), json!(edge.kind));
                records.push(eval_record(
                    envelope,
                    records.len(),
                    "state_transition",
                    "langgraph_edge",
                    &format!("{}->{}:{}", edge.from, edge.to, edge.kind),
                    EvalRecordFields {
                        metadata: edge_metadata,
                        ..EvalRecordFields::default()
                    },
                ));
            }
            for task in &projection.tasks {
                records.push(eval_record(
                    envelope,
                    records.len(),
                    "task",
                    "langgraph_task",
                    &task.id,
                    EvalRecordFields {
                        tool_name: Some(task.name.clone()),
                        status: Some(task.status.clone()),
                        child_request_id: task.child_request_id.clone(),
                        metadata: metadata([("request_id", task.request_id.clone())]),
                        ..EvalRecordFields::default()
                    },
                ));
            }
        }
        AdapterProjection::MultiAgentTask(projection) => {
            for participant in &projection.participants {
                records.push(eval_record(
                    envelope,
                    records.len(),
                    "participant",
                    "multi_agent_participant",
                    participant
                        .agent_did
                        .as_deref()
                        .or(participant.behavior_id.as_deref())
                        .unwrap_or(participant.role.as_str()),
                    EvalRecordFields {
                        role: Some(participant.role.clone()),
                        metadata: metadata([
                            ("agent_did", participant.agent_did.clone()),
                            ("behavior_id", participant.behavior_id.clone()),
                        ]),
                        ..EvalRecordFields::default()
                    },
                ));
            }
            for message in &projection.messages {
                records.push(eval_record(
                    envelope,
                    records.len(),
                    "message",
                    "multi_agent_message",
                    &message.id,
                    EvalRecordFields {
                        role: Some(message.role.clone()),
                        output: Some(message.content.clone()),
                        metadata: metadata([("request_id", message.request_id.clone())]),
                        ..EvalRecordFields::default()
                    },
                ));
            }
            for delegation in &projection.delegations {
                records.push(eval_record(
                    envelope,
                    records.len(),
                    "delegation",
                    "multi_agent_delegation",
                    &format!(
                        "{}->{}",
                        delegation.parent_request_id, delegation.child_request_id
                    ),
                    EvalRecordFields {
                        parent_request_id: Some(delegation.parent_request_id.clone()),
                        child_request_id: Some(delegation.child_request_id.clone()),
                        status: delegation.status.clone(),
                        metadata: metadata([
                            (
                                "parent_tool_call_id",
                                delegation.parent_tool_call_id.clone(),
                            ),
                            ("agent_did", delegation.agent_did.clone()),
                            ("behavior_id", delegation.behavior_id.clone()),
                        ]),
                        ..EvalRecordFields::default()
                    },
                ));
            }
            for tool_event in &projection.tool_events {
                records.push(eval_record(
                    envelope,
                    records.len(),
                    "tool_call",
                    "multi_agent_tool_event",
                    &tool_event.id,
                    EvalRecordFields {
                        tool_name: Some(tool_event.tool_name.clone()),
                        status: Some(tool_event.status.clone()),
                        child_request_id: tool_event.child_request_id.clone(),
                        metadata: metadata([
                            ("request_id", tool_event.request_id.clone()),
                            (
                                "selected_service_id",
                                tool_event.selected_service_id.clone(),
                            ),
                            ("selected_tool_name", tool_event.selected_tool_name.clone()),
                            ("denial_reason", tool_event.denial_reason.clone()),
                        ]),
                        ..EvalRecordFields::default()
                    },
                ));
            }
        }
    }
    records
}

pub fn adapter_projection_json_schema(kind: AdapterProjectionKind) -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": format!("https://schemas.defra.ai/defra-agent/adapter-projection/{}/{}.schema.json", kind.id(), ADAPTER_PROJECTION_VERSION),
        "title": kind.title(),
        "type": "object",
        "additionalProperties": false,
        "required": [
            "projection_id",
            "projection_version",
            "source_request_id",
            "redaction_mode",
            "provenance",
            "output"
        ],
        "properties": {
            "projection_id": { "const": kind.id() },
            "projection_version": { "const": ADAPTER_PROJECTION_VERSION },
            "source_request_id": string_schema(),
            "source_session_id": optional_string_schema(),
            "source_agent_did": optional_string_schema(),
            "source_behavior_id": optional_string_schema(),
            "redaction_mode": redaction_mode_schema(),
            "provenance": provenance_schema(),
            "output": {
                "type": "object",
                "additionalProperties": false,
                "required": ["adapter", "projection"],
                "properties": {
                    "adapter": { "const": kind.id() },
                    "projection": projection_json_schema(kind)
                }
            }
        }
    })
}

pub fn adapter_projection_jsonl_record_schema(kind: AdapterProjectionKind) -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": format!("https://schemas.defra.ai/defra-agent/adapter-projection/{}/{}-jsonl-record.schema.json", kind.id(), ADAPTER_PROJECTION_VERSION),
        "title": format!("{} JSONL Record", kind.title()),
        "type": "object",
        "additionalProperties": false,
        "required": [
            "projection_id",
            "projection_version",
            "source_request_id",
            "redaction_mode",
            "record_kind",
            "record_index",
            "record_id",
            "value"
        ],
        "properties": {
            "projection_id": { "const": kind.id() },
            "projection_version": { "const": ADAPTER_PROJECTION_VERSION },
            "source_request_id": string_schema(),
            "source_session_id": optional_string_schema(),
            "redaction_mode": redaction_mode_schema(),
            "record_kind": jsonl_record_kind_schema(kind),
            "record_index": { "type": "integer", "minimum": 0 },
            "record_id": string_schema(),
            "value": { "type": "object" }
        }
    })
}

pub fn adapter_projection_eval_jsonl_record_schema(kind: AdapterProjectionKind) -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": format!("https://schemas.defra.ai/defra-agent/adapter-projection/{}/{}-eval-jsonl-record.schema.json", kind.id(), ADAPTER_PROJECTION_VERSION),
        "title": format!("{} Eval JSONL Record", kind.title()),
        "type": "object",
        "additionalProperties": false,
        "required": [
            "projection_id",
            "projection_version",
            "source_request_id",
            "redaction_mode",
            "record_index",
            "record_id",
            "sample_kind",
            "adapter_record_kind",
            "adapter_record_id"
        ],
        "properties": {
            "projection_id": { "const": kind.id() },
            "projection_version": { "const": ADAPTER_PROJECTION_VERSION },
            "source_request_id": string_schema(),
            "source_session_id": optional_string_schema(),
            "redaction_mode": redaction_mode_schema(),
            "record_index": { "type": "integer", "minimum": 0 },
            "record_id": string_schema(),
            "sample_kind": training_sample_kind_schema(kind),
            "adapter_record_kind": jsonl_record_kind_schema(kind),
            "adapter_record_id": string_schema(),
            "role": optional_string_schema(),
            "input": optional_string_schema(),
            "output": optional_string_schema(),
            "tool_name": optional_string_schema(),
            "status": optional_string_schema(),
            "parent_request_id": optional_string_schema(),
            "child_request_id": optional_string_schema(),
            "metadata": { "type": "object" }
        }
    })
}

pub fn adapter_projection_schema_index() -> Value {
    let projections = [
        AdapterProjectionKind::OpenAiCodexRunTrace,
        AdapterProjectionKind::LangGraphStateHistory,
        AdapterProjectionKind::MultiAgentTask,
    ];
    json!({
        "projection_version": ADAPTER_PROJECTION_VERSION,
        "source_projection_id": RUN_TIMELINE_PROJECTION_ID,
        "schemas": projections
            .iter()
            .map(|kind| {
                json!({
                    "projection_id": kind.id(),
                    "title": kind.title(),
                    "json_schema_id": adapter_projection_json_schema(*kind).get("$id").cloned().unwrap_or(Value::Null),
                    "jsonl_record_schema_id": adapter_projection_jsonl_record_schema(*kind).get("$id").cloned().unwrap_or(Value::Null),
                    "eval_jsonl_record_schema_id": adapter_projection_eval_jsonl_record_schema(*kind).get("$id").cloned().unwrap_or(Value::Null)
                })
            })
            .collect::<Vec<_>>()
    })
}

fn projection_json_schema(kind: AdapterProjectionKind) -> Value {
    match kind {
        AdapterProjectionKind::OpenAiCodexRunTrace => openai_codex_projection_schema(),
        AdapterProjectionKind::LangGraphStateHistory => langgraph_projection_schema(),
        AdapterProjectionKind::MultiAgentTask => multi_agent_projection_schema(),
    }
}

fn openai_codex_projection_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["run_id", "items"],
        "properties": {
            "run_id": string_schema(),
            "thread_id": optional_string_schema(),
            "status": optional_string_schema(),
            "items": {
                "type": "array",
                "items": {
                    "oneOf": [
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["type", "id"],
                            "properties": {
                                "type": { "const": "request" },
                                "id": string_schema(),
                                "status": optional_string_schema(),
                                "lifecycle_state": optional_string_schema(),
                                "input": optional_string_schema(),
                                "timestamp": optional_string_schema()
                            }
                        },
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["type", "id", "role", "content"],
                            "properties": {
                                "type": { "const": "message" },
                                "id": string_schema(),
                                "request_id": optional_string_schema(),
                                "role": string_schema(),
                                "content": string_schema(),
                                "timestamp": optional_string_schema()
                            }
                        },
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["type", "id", "name", "arguments", "output", "status"],
                            "properties": {
                                "type": { "const": "tool_call" },
                                "id": string_schema(),
                                "request_id": optional_string_schema(),
                                "name": string_schema(),
                                "arguments": string_schema(),
                                "output": string_schema(),
                                "status": string_schema(),
                                "child_run_id": optional_string_schema(),
                                "started_at": optional_string_schema(),
                                "completed_at": optional_string_schema()
                            }
                        },
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["type", "id", "status"],
                            "properties": {
                                "type": { "const": "response" },
                                "id": string_schema(),
                                "status": optional_string_schema(),
                                "output": optional_string_schema(),
                                "reasoning": optional_string_schema(),
                                "error": optional_string_schema(),
                                "timestamp": optional_string_schema()
                            }
                        }
                    ]
                }
            },
            "child_run_ids": {
                "type": "array",
                "items": string_schema()
            }
        }
    })
}

fn langgraph_projection_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["checkpoint_id", "root_request_id", "values", "nodes", "edges", "tasks"],
        "properties": {
            "thread_id": optional_string_schema(),
            "checkpoint_id": string_schema(),
            "root_request_id": string_schema(),
            "values": { "type": "object" },
            "nodes": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["id", "kind"],
                    "properties": {
                        "id": string_schema(),
                        "kind": string_schema(),
                        "request_id": optional_string_schema(),
                        "status": optional_string_schema()
                    }
                }
            },
            "edges": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["from", "to", "kind"],
                    "properties": {
                        "from": string_schema(),
                        "to": string_schema(),
                        "kind": string_schema()
                    }
                }
            },
            "tasks": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["id", "name", "status"],
                    "properties": {
                        "id": string_schema(),
                        "request_id": optional_string_schema(),
                        "name": string_schema(),
                        "status": string_schema(),
                        "child_request_id": optional_string_schema()
                    }
                }
            }
        }
    })
}

fn multi_agent_projection_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["task_id", "participants", "messages", "delegations", "tool_events"],
        "properties": {
            "task_id": string_schema(),
            "context_id": optional_string_schema(),
            "status": optional_string_schema(),
            "participants": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["role"],
                    "properties": {
                        "agent_did": optional_string_schema(),
                        "behavior_id": optional_string_schema(),
                        "role": string_schema()
                    }
                }
            },
            "messages": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["id", "role", "content"],
                    "properties": {
                        "id": string_schema(),
                        "request_id": optional_string_schema(),
                        "role": string_schema(),
                        "content": string_schema()
                    }
                }
            },
            "delegations": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["parent_request_id", "child_request_id"],
                    "properties": {
                        "parent_request_id": string_schema(),
                        "child_request_id": string_schema(),
                        "parent_tool_call_id": optional_string_schema(),
                        "agent_did": optional_string_schema(),
                        "behavior_id": optional_string_schema(),
                        "status": optional_string_schema()
                    }
                }
            },
            "tool_events": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["id", "tool_name", "status"],
                    "properties": {
                        "id": string_schema(),
                        "request_id": optional_string_schema(),
                        "tool_name": string_schema(),
                        "status": string_schema(),
                        "selected_service_id": optional_string_schema(),
                        "selected_tool_name": optional_string_schema(),
                        "denial_reason": optional_string_schema(),
                        "child_request_id": optional_string_schema()
                    }
                }
            }
        }
    })
}

fn provenance_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["runtime", "source_projection_id", "source_projection_version"],
        "properties": {
            "runtime": { "const": "defra-agent" },
            "source_projection_id": { "const": RUN_TIMELINE_PROJECTION_ID },
            "source_projection_version": { "const": ADAPTER_PROJECTION_VERSION },
            "actor_did": optional_string_schema()
        }
    })
}

fn jsonl_record_kind_schema(kind: AdapterProjectionKind) -> Value {
    match kind {
        AdapterProjectionKind::OpenAiCodexRunTrace => {
            json!({ "enum": ["openai_codex_trace_item"] })
        }
        AdapterProjectionKind::LangGraphStateHistory => {
            json!({ "enum": ["langgraph_values", "langgraph_node", "langgraph_edge", "langgraph_task"] })
        }
        AdapterProjectionKind::MultiAgentTask => {
            json!({ "enum": ["multi_agent_participant", "multi_agent_message", "multi_agent_delegation", "multi_agent_tool_event"] })
        }
    }
}

fn training_sample_kind_schema(kind: AdapterProjectionKind) -> Value {
    match kind {
        AdapterProjectionKind::OpenAiCodexRunTrace => {
            json!({ "enum": ["prompt", "message", "tool_call", "response"] })
        }
        AdapterProjectionKind::LangGraphStateHistory => {
            json!({ "enum": ["state_snapshot", "state_node", "state_transition", "task"] })
        }
        AdapterProjectionKind::MultiAgentTask => {
            json!({ "enum": ["participant", "message", "delegation", "tool_call"] })
        }
    }
}

fn redaction_mode_schema() -> Value {
    json!({ "enum": ["full", "training_safe", "public"] })
}

fn string_schema() -> Value {
    json!({ "type": "string" })
}

fn optional_string_schema() -> Value {
    json!({
        "anyOf": [
            { "type": "string" },
            { "type": "null" }
        ]
    })
}

#[derive(Default)]
struct EvalRecordFields {
    role: Option<String>,
    input: Option<String>,
    output: Option<String>,
    tool_name: Option<String>,
    status: Option<String>,
    parent_request_id: Option<String>,
    child_request_id: Option<String>,
    metadata: BTreeMap<String, Value>,
}

fn eval_record(
    envelope: &AdapterProjectionEnvelope,
    record_index: usize,
    sample_kind: &str,
    adapter_record_kind: &str,
    adapter_record_id: &str,
    fields: EvalRecordFields,
) -> AdapterProjectionEvalJsonlRecord {
    AdapterProjectionEvalJsonlRecord {
        projection_id: envelope.projection_id.clone(),
        projection_version: envelope.projection_version.clone(),
        source_request_id: envelope.source_request_id.clone(),
        source_session_id: envelope.source_session_id.clone(),
        redaction_mode: envelope.redaction_mode,
        record_index,
        record_id: format!("{adapter_record_kind}:{adapter_record_id}:{record_index}"),
        sample_kind: sample_kind.to_string(),
        adapter_record_kind: adapter_record_kind.to_string(),
        adapter_record_id: adapter_record_id.to_string(),
        role: fields.role,
        input: fields.input,
        output: fields.output,
        tool_name: fields.tool_name,
        status: fields.status,
        parent_request_id: fields.parent_request_id,
        child_request_id: fields.child_request_id,
        metadata: fields.metadata,
    }
}

fn metadata<'a, I>(entries: I) -> BTreeMap<String, Value>
where
    I: IntoIterator<Item = (&'a str, Option<String>)>,
{
    entries
        .into_iter()
        .filter_map(|(key, value)| value.map(|value| (key.to_string(), Value::String(value))))
        .collect()
}

fn jsonl_record(
    envelope: &AdapterProjectionEnvelope,
    record_kind: &str,
    record_index: usize,
    record_id: String,
    value: Value,
) -> AdapterProjectionJsonlRecord {
    AdapterProjectionJsonlRecord {
        projection_id: envelope.projection_id.clone(),
        projection_version: envelope.projection_version.clone(),
        source_request_id: envelope.source_request_id.clone(),
        source_session_id: envelope.source_session_id.clone(),
        redaction_mode: envelope.redaction_mode,
        record_kind: record_kind.to_string(),
        record_index,
        record_id,
        value,
    }
}

fn openai_item_id(item: &OpenAiCodexTraceItem) -> String {
    match item {
        OpenAiCodexTraceItem::Request { id, .. }
        | OpenAiCodexTraceItem::Message { id, .. }
        | OpenAiCodexTraceItem::ToolCall { id, .. }
        | OpenAiCodexTraceItem::Response { id, .. } => id.clone(),
    }
}

fn validate_openai_codex_projection(
    violations: &mut Vec<String>,
    projection: &OpenAiCodexRunTraceProjection,
) {
    require_nonempty(violations, "output.projection.run_id", &projection.run_id);
    require_nonempty_vec(
        violations,
        "output.projection.items",
        projection.items.len(),
    );
    for (index, item) in projection.items.iter().enumerate() {
        match item {
            OpenAiCodexTraceItem::Request { id, .. }
            | OpenAiCodexTraceItem::Message { id, .. }
            | OpenAiCodexTraceItem::Response { id, .. } => {
                require_nonempty(violations, &format!("items[{index}].id"), id);
            }
            OpenAiCodexTraceItem::ToolCall {
                id, name, status, ..
            } => {
                require_nonempty(violations, &format!("items[{index}].id"), id);
                require_nonempty(violations, &format!("items[{index}].name"), name);
                require_nonempty(violations, &format!("items[{index}].status"), status);
            }
        }
    }
}

fn validate_langgraph_projection(
    violations: &mut Vec<String>,
    projection: &LangGraphStateHistoryProjection,
) {
    require_nonempty(
        violations,
        "output.projection.checkpoint_id",
        &projection.checkpoint_id,
    );
    require_nonempty(
        violations,
        "output.projection.root_request_id",
        &projection.root_request_id,
    );
    require_nonempty_vec(
        violations,
        "output.projection.nodes",
        projection.nodes.len(),
    );
    if !projection.values.contains_key("request_id") {
        violations.push("output.projection.values.request_id is required".to_string());
    }
    for (index, node) in projection.nodes.iter().enumerate() {
        require_nonempty(violations, &format!("nodes[{index}].id"), &node.id);
        require_nonempty(violations, &format!("nodes[{index}].kind"), &node.kind);
    }
    for (index, edge) in projection.edges.iter().enumerate() {
        require_nonempty(violations, &format!("edges[{index}].from"), &edge.from);
        require_nonempty(violations, &format!("edges[{index}].to"), &edge.to);
        require_nonempty(violations, &format!("edges[{index}].kind"), &edge.kind);
    }
    for (index, task) in projection.tasks.iter().enumerate() {
        require_nonempty(violations, &format!("tasks[{index}].id"), &task.id);
        require_nonempty(violations, &format!("tasks[{index}].name"), &task.name);
        require_nonempty(violations, &format!("tasks[{index}].status"), &task.status);
    }
}

fn validate_multi_agent_projection(
    violations: &mut Vec<String>,
    projection: &MultiAgentTaskProjection,
) {
    require_nonempty(violations, "output.projection.task_id", &projection.task_id);
    require_nonempty_vec(
        violations,
        "output.projection.participants",
        projection.participants.len(),
    );
    for (index, participant) in projection.participants.iter().enumerate() {
        require_nonempty(
            violations,
            &format!("participants[{index}].role"),
            &participant.role,
        );
        if participant
            .agent_did
            .as_deref()
            .unwrap_or("")
            .trim()
            .is_empty()
            && participant
                .behavior_id
                .as_deref()
                .unwrap_or("")
                .trim()
                .is_empty()
        {
            violations.push(format!(
                "participants[{index}] must include agent_did or behavior_id"
            ));
        }
    }
    for (index, message) in projection.messages.iter().enumerate() {
        require_nonempty(violations, &format!("messages[{index}].id"), &message.id);
        require_nonempty(
            violations,
            &format!("messages[{index}].role"),
            &message.role,
        );
    }
    for (index, delegation) in projection.delegations.iter().enumerate() {
        require_nonempty(
            violations,
            &format!("delegations[{index}].parent_request_id"),
            &delegation.parent_request_id,
        );
        require_nonempty(
            violations,
            &format!("delegations[{index}].child_request_id"),
            &delegation.child_request_id,
        );
    }
    for (index, event) in projection.tool_events.iter().enumerate() {
        require_nonempty(violations, &format!("tool_events[{index}].id"), &event.id);
        require_nonempty(
            violations,
            &format!("tool_events[{index}].tool_name"),
            &event.tool_name,
        );
        require_nonempty(
            violations,
            &format!("tool_events[{index}].status"),
            &event.status,
        );
    }
}

fn require_nonempty(violations: &mut Vec<String>, path: &str, value: &str) {
    if value.trim().is_empty() {
        violations.push(format!("{path} is required"));
    }
}

fn require_nonempty_vec(violations: &mut Vec<String>, path: &str, len: usize) {
    if len == 0 {
        violations.push(format!("{path} must not be empty"));
    }
}

fn require_eq(violations: &mut Vec<String>, path: &str, actual: &str, expected: &str) {
    if actual != expected {
        violations.push(format!("{path} expected {expected:?}, got {actual:?}"));
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

    fn workspace_root() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(std::path::Path::parent)
            .map(std::path::Path::to_path_buf)
            .expect("workspace root")
    }

    fn read_adapter_projection_fixture(fixture_name: &str) -> (AdapterProjectionEnvelope, Value) {
        let path = workspace_root().join(format!(
            "docs/superpowers/fixtures/adapter-projections/v1/{fixture_name}.envelope.json"
        ));
        let raw = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("reading {}: {error}", path.display()));
        let value = serde_json::from_str::<Value>(&raw)
            .unwrap_or_else(|error| panic!("parsing {} as JSON: {error}", path.display()));
        let envelope = serde_json::from_value::<AdapterProjectionEnvelope>(value.clone())
            .unwrap_or_else(|error| panic!("deserializing {}: {error}", path.display()));
        (envelope, value)
    }

    fn assert_json_schema_valid(schema: &Value, instance: &Value, label: &str) {
        let validator = jsonschema::validator_for(schema)
            .unwrap_or_else(|error| panic!("{label} schema failed to compile: {error}"));
        let errors = validator
            .iter_errors(instance)
            .map(|error| format!("{}: {error}", error.instance_path()))
            .collect::<Vec<_>>();
        assert!(
            errors.is_empty(),
            "{label} failed JSON Schema validation:\n{}",
            errors.join("\n")
        );
    }

    fn assert_adapter_projection_matches_json_schema(envelope: &AdapterProjectionEnvelope) {
        let kind = envelope.output.kind();
        let envelope_value = serde_json::to_value(envelope).unwrap();
        assert_json_schema_valid(
            &adapter_projection_json_schema(kind),
            &envelope_value,
            kind.id(),
        );

        let jsonl_record_schema = adapter_projection_jsonl_record_schema(kind);
        for record in adapter_projection_jsonl_records(envelope) {
            let record_value = serde_json::to_value(&record).unwrap();
            assert_json_schema_valid(
                &jsonl_record_schema,
                &record_value,
                &format!("{} JSONL record {}", kind.id(), record.record_id),
            );
        }

        let eval_jsonl_record_schema = adapter_projection_eval_jsonl_record_schema(kind);
        for record in adapter_projection_eval_jsonl_records(envelope) {
            let record_value = serde_json::to_value(&record).unwrap();
            assert_json_schema_valid(
                &eval_jsonl_record_schema,
                &record_value,
                &format!("{} eval JSONL record {}", kind.id(), record.record_id),
            );
        }
    }

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
                doc_id: None,
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

        for kind in [
            AdapterProjectionKind::OpenAiCodexRunTrace,
            AdapterProjectionKind::LangGraphStateHistory,
            AdapterProjectionKind::MultiAgentTask,
        ] {
            let envelope_schema = adapter_projection_json_schema(kind);
            assert_eq!(
                envelope_schema
                    .pointer("/properties/projection_id/const")
                    .and_then(Value::as_str),
                Some(kind.id())
            );
            assert_eq!(
                envelope_schema
                    .pointer("/properties/output/properties/adapter/const")
                    .and_then(Value::as_str),
                Some(kind.id())
            );

            let jsonl_schema = adapter_projection_jsonl_record_schema(kind);
            assert_eq!(
                jsonl_schema
                    .pointer("/properties/projection_id/const")
                    .and_then(Value::as_str),
                Some(kind.id())
            );
            assert!(jsonl_schema.pointer("/properties/record_kind").is_some());
        }
        let schema_index = adapter_projection_schema_index();
        assert_eq!(
            schema_index
                .get("schemas")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(3)
        );

        assert_eq!(codex.projection_id, "openai_codex_run_trace");
        assert_eq!(langgraph.projection_id, "langgraph_state_history");
        assert_eq!(multi.projection_id, "multi_agent_task");
        validate_adapter_projection_contract(&codex).unwrap();
        validate_adapter_projection_contract(&langgraph).unwrap();
        validate_adapter_projection_contract(&multi).unwrap();
        assert_adapter_projection_matches_json_schema(&codex);
        assert_adapter_projection_matches_json_schema(&langgraph);
        assert_adapter_projection_matches_json_schema(&multi);

        let codex_records = adapter_projection_jsonl_records(&codex);
        assert!(!codex_records.is_empty());
        assert_eq!(codex_records[0].projection_id, "openai_codex_run_trace");
        assert_eq!(codex_records[0].source_request_id, "req-1");
        assert_eq!(codex_records[0].record_kind, "openai_codex_trace_item");

        let mut invalid = multi.clone();
        invalid.projection_id.clear();
        let error = validate_adapter_projection_contract(&invalid).unwrap_err();
        assert!(error
            .violations
            .iter()
            .any(|violation| violation == "projection_id is required"));

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

    #[test]
    fn external_contract_fixtures_validate_without_runtime_dependencies() {
        let cases: &[(AdapterProjectionKind, &str, &[&str])] = &[
            (
                AdapterProjectionKind::OpenAiCodexRunTrace,
                "openai_codex_run_trace",
                &["openai_codex_trace_item"],
            ),
            (
                AdapterProjectionKind::LangGraphStateHistory,
                "langgraph_state_history",
                &[
                    "langgraph_values",
                    "langgraph_node",
                    "langgraph_edge",
                    "langgraph_task",
                ],
            ),
            (
                AdapterProjectionKind::MultiAgentTask,
                "multi_agent_task",
                &[
                    "multi_agent_participant",
                    "multi_agent_message",
                    "multi_agent_delegation",
                    "multi_agent_tool_event",
                ],
            ),
        ];

        for (kind, fixture_name, expected_record_kinds) in cases {
            let (envelope, fixture_value) = read_adapter_projection_fixture(fixture_name);
            assert_eq!(envelope.projection_id, kind.id());
            assert_eq!(envelope.output.kind(), *kind);
            validate_adapter_projection_contract(&envelope)
                .unwrap_or_else(|error| panic!("{fixture_name} failed contract: {error}"));
            assert_json_schema_valid(
                &adapter_projection_json_schema(*kind),
                &fixture_value,
                fixture_name,
            );

            let round_trip = serde_json::to_value(&envelope).unwrap();
            assert_eq!(
                round_trip, fixture_value,
                "{fixture_name} fixture drifted from adapter DTO serialization"
            );

            let allowed_record_kinds = adapter_projection_jsonl_record_schema(*kind)
                .pointer("/properties/record_kind/enum")
                .and_then(Value::as_array)
                .expect("JSONL record kind enum")
                .iter()
                .map(|value| value.as_str().expect("string record kind").to_string())
                .collect::<std::collections::BTreeSet<_>>();
            let records = adapter_projection_jsonl_records(&envelope);
            assert!(
                !records.is_empty(),
                "{fixture_name} fixture should produce JSONL records"
            );

            let mut observed_record_kinds = std::collections::BTreeSet::new();
            for (index, record) in records.iter().enumerate() {
                assert_eq!(record.projection_id, kind.id());
                assert_eq!(record.projection_version, ADAPTER_PROJECTION_VERSION);
                assert_eq!(record.source_request_id, envelope.source_request_id);
                assert_eq!(record.record_index, index);
                assert!(
                    record.value.is_object(),
                    "{fixture_name} JSONL value must be an object: {record:#?}"
                );
                assert!(
                    allowed_record_kinds.contains(&record.record_kind),
                    "{fixture_name} produced unsupported JSONL record kind {}",
                    record.record_kind
                );
                assert_json_schema_valid(
                    &adapter_projection_jsonl_record_schema(*kind),
                    &serde_json::to_value(record).unwrap(),
                    &format!("{fixture_name} JSONL record {}", record.record_id),
                );
                observed_record_kinds.insert(record.record_kind.clone());
            }
            for expected in *expected_record_kinds {
                assert!(
                    observed_record_kinds.contains(*expected),
                    "{fixture_name} missing expected JSONL record kind {expected}"
                );
            }

            let eval_records = adapter_projection_eval_jsonl_records(&envelope);
            assert!(
                !eval_records.is_empty(),
                "{fixture_name} fixture should produce eval JSONL records"
            );
            for (index, record) in eval_records.iter().enumerate() {
                assert_eq!(record.projection_id, kind.id());
                assert_eq!(record.record_index, index);
                assert_json_schema_valid(
                    &adapter_projection_eval_jsonl_record_schema(*kind),
                    &serde_json::to_value(record).unwrap(),
                    &format!("{fixture_name} eval JSONL record {}", record.record_id),
                );
            }
        }
    }
}
