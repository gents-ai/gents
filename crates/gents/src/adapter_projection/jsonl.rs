use serde_json::Value;

use super::{
    AdapterProjection, AdapterProjectionEnvelope, AdapterProjectionJsonlRecord,
    OpenAiCodexTraceItem,
};

pub fn adapter_projection_jsonl_records(
    envelope: &AdapterProjectionEnvelope,
) -> Vec<AdapterProjectionJsonlRecord> {
    match &envelope.output {
        AdapterProjection::AtifTrajectory(projection) => {
            let mut records = Vec::new();
            records.push(jsonl_record(
                envelope,
                "atif_agent",
                records.len(),
                projection.agent.name.clone(),
                serde_json::to_value(&projection.agent).unwrap_or(Value::Null),
            ));
            for step in &projection.steps {
                records.push(jsonl_record(
                    envelope,
                    "atif_step",
                    records.len(),
                    format!("step:{}", step.step_id),
                    serde_json::to_value(step).unwrap_or(Value::Null),
                ));
            }
            if let Some(final_metrics) = projection.final_metrics.as_ref() {
                records.push(jsonl_record(
                    envelope,
                    "atif_final_metrics",
                    records.len(),
                    "final_metrics".to_string(),
                    serde_json::to_value(final_metrics).unwrap_or(Value::Null),
                ));
            }
            records
        }
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
                        .node_did
                        .clone()
                        .or_else(|| participant.agent_id.clone())
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

pub fn adapter_projection_native_json(envelope: &AdapterProjectionEnvelope) -> Value {
    match &envelope.output {
        AdapterProjection::AtifTrajectory(projection) => {
            serde_json::to_value(projection).unwrap_or(Value::Null)
        }
        AdapterProjection::OpenAiCodexRunTrace(projection) => {
            serde_json::to_value(projection).unwrap_or(Value::Null)
        }
        AdapterProjection::LangGraphStateHistory(projection) => {
            serde_json::to_value(projection).unwrap_or(Value::Null)
        }
        AdapterProjection::MultiAgentTask(projection) => {
            serde_json::to_value(projection).unwrap_or(Value::Null)
        }
    }
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
        | OpenAiCodexTraceItem::ToolCall { id, .. } => id.clone(),
    }
}
