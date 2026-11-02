use std::collections::BTreeMap;

use serde_json::{json, Value};

use super::{
    AdapterProjection, AdapterProjectionEnvelope, AdapterProjectionEvalJsonlRecord, AtifStepSource,
    OpenAiCodexTraceItem,
};

pub fn adapter_projection_eval_jsonl_records(
    envelope: &AdapterProjectionEnvelope,
) -> Vec<AdapterProjectionEvalJsonlRecord> {
    let mut records = Vec::new();
    match &envelope.output {
        AdapterProjection::AtifTrajectory(projection) => {
            for step in &projection.steps {
                if let Some(tools) = step.tool_calls.as_deref().filter(|tools| !tools.is_empty()) {
                    for tool in tools {
                        let observation = step.observation.as_ref().and_then(|observation| {
                            observation.results.iter().find(|result| {
                                result.source_call_id.as_deref() == Some(tool.tool_call_id.as_str())
                            })
                        });
                        let status = tool
                            .extra
                            .as_ref()
                            .and_then(|extra| extra.get("status"))
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned);
                        let child_request_id = tool
                            .extra
                            .as_ref()
                            .and_then(|extra| extra.get("child_request_id"))
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned);
                        records.push(eval_record(
                            envelope,
                            records.len(),
                            "tool_call",
                            "atif_step",
                            &format!("step:{}:tool:{}", step.step_id, tool.tool_call_id),
                            EvalRecordFields {
                                role: Some(step.source.as_str().to_string()),
                                input: Some(
                                    serde_json::to_string(&tool.arguments)
                                        .unwrap_or_else(|_| "{}".to_string()),
                                ),
                                output: observation.and_then(|result| result.content.clone()),
                                tool_name: Some(tool.function_name.clone()),
                                status,
                                child_request_id,
                                metadata: step.extra.clone().unwrap_or_default(),
                                ..EvalRecordFields::default()
                            },
                        ));
                    }
                    continue;
                }
                records.push(eval_record(
                    envelope,
                    records.len(),
                    if step.source == AtifStepSource::User {
                        "prompt"
                    } else {
                        "message"
                    },
                    "atif_step",
                    &format!("step:{}", step.step_id),
                    EvalRecordFields {
                        role: Some(step.source.as_str().to_string()),
                        input: (step.source == AtifStepSource::User).then(|| step.message.clone()),
                        output: (step.source != AtifStepSource::User).then(|| step.message.clone()),
                        metadata: step.extra.clone().unwrap_or_default(),
                        ..EvalRecordFields::default()
                    },
                ));
            }
        }
        AdapterProjection::OpenAiCodexRunTrace(projection) => {
            for item in &projection.items {
                match item {
                    OpenAiCodexTraceItem::Request {
                        id,
                        status,
                        node_did,
                        agent_id,
                        parent_request_id,
                        parent_tool_call_id,
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
                            status: status.clone(),
                            metadata: metadata([
                                ("timestamp", timestamp.clone()),
                                ("node_did", node_did.clone()),
                                ("agent_id", agent_id.clone()),
                                ("parent_request_id", parent_request_id.clone()),
                                ("parent_tool_call_id", parent_tool_call_id.clone()),
                            ]),
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
                            output: output.clone(),
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
                            ("node_did", node.node_did.clone()),
                            ("agent_id", node.agent_id.clone()),
                            ("parent_request_id", node.parent_request_id.clone()),
                            ("parent_tool_call_id", node.parent_tool_call_id.clone()),
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
                        .node_did
                        .as_deref()
                        .or(participant.agent_id.as_deref())
                        .unwrap_or(participant.role.as_str()),
                    EvalRecordFields {
                        role: Some(participant.role.clone()),
                        metadata: metadata([
                            ("node_did", participant.node_did.clone()),
                            ("agent_id", participant.agent_id.clone()),
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
                            ("node_did", delegation.node_did.clone()),
                            ("agent_id", delegation.agent_id.clone()),
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
