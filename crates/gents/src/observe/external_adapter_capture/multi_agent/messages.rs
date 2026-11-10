use serde_json::Value;

use super::super::{
    value_to_text, ExternalAdapterMapping, ExternalParticipantMapping, ImportedMessage,
};
use super::external_task_text;

pub(super) fn native_messages(
    source_system: &str,
    native: &Value,
    mapping: &ExternalAdapterMapping,
) -> Vec<ImportedMessage> {
    match source_system {
        "autogen-agentchat" => autogen_messages(native, mapping),
        "crewai" => crewai_messages(native, mapping),
        "microsoft-agent-framework" => microsoft_agent_framework_messages(native, mapping),
        _ => Vec::new(),
    }
}

fn autogen_messages(native: &Value, mapping: &ExternalAdapterMapping) -> Vec<ImportedMessage> {
    native
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|message| {
            let source = message.get("source")?.as_str()?;
            let participant = participant_for_native_name(mapping, source);
            Some(ImportedMessage {
                role: participant
                    .map(|participant| participant.role.clone())
                    .unwrap_or_else(|| source.to_string()),
                content: value_to_text(message.get("content")?),
                request_id: participant
                    .and_then(|participant| participant.request_id.clone())
                    .or_else(|| Some(mapping.request_id.clone())),
            })
        })
        .collect()
}

fn crewai_messages(native: &Value, mapping: &ExternalAdapterMapping) -> Vec<ImportedMessage> {
    let manager_responses = native
        .get("manager_responses")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    if !manager_responses.is_empty() {
        return crewai_hierarchical_messages(native, mapping, manager_responses);
    }

    native
        .get("tasks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|task| crewai_task_message(task, mapping))
        .collect()
}

fn crewai_hierarchical_messages(
    native: &Value,
    mapping: &ExternalAdapterMapping,
    manager_responses: &[Value],
) -> Vec<ImportedMessage> {
    let mut messages = Vec::new();
    let manager = participant_for_role(mapping, "manager");
    let manager_role = manager
        .map(|participant| participant.role.clone())
        .unwrap_or_else(|| "manager".to_string());
    let manager_request_id = manager
        .and_then(|participant| participant.request_id.clone())
        .unwrap_or_else(|| mapping.request_id.clone());
    let tasks = native
        .get("tasks")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);

    if let Some(response) = manager_responses.first() {
        messages.push(ImportedMessage {
            role: manager_role.clone(),
            content: value_to_text(response),
            request_id: Some(manager_request_id.clone()),
        });
    }
    push_crewai_llm_message(&mut messages, native, mapping, "researcher");
    if let Some(task) = tasks.first() {
        if let Some(mut message) = crewai_task_message(task, mapping) {
            message.role = manager_role.clone();
            message.request_id = Some(manager_request_id.clone());
            messages.push(message);
        }
    }
    if let Some(response) = manager_responses.get(2) {
        messages.push(ImportedMessage {
            role: manager_role.clone(),
            content: value_to_text(response),
            request_id: Some(manager_request_id.clone()),
        });
    }
    push_crewai_llm_message(&mut messages, native, mapping, "reviewer");
    if let Some(task) = tasks.get(1) {
        if let Some(mut message) = crewai_task_message(task, mapping) {
            message.role = manager_role;
            message.request_id = Some(manager_request_id);
            messages.push(message);
        }
    }
    messages
}

fn crewai_task_message(task: &Value, mapping: &ExternalAdapterMapping) -> Option<ImportedMessage> {
    let native_name = task
        .pointer("/agent/role")
        .and_then(Value::as_str)
        .unwrap_or("agent");
    let participant = participant_for_native_name(mapping, native_name)
        .or_else(|| participant_for_role(mapping, native_name));
    let output = task.pointer("/output/raw").or_else(|| task.get("output"))?;
    Some(ImportedMessage {
        role: participant
            .map(|participant| participant.role.clone())
            .unwrap_or_else(|| native_name.to_string()),
        content: value_to_text(output),
        request_id: participant
            .and_then(|participant| participant.request_id.clone())
            .or_else(|| Some(mapping.request_id.clone())),
    })
}

fn push_crewai_llm_message(
    messages: &mut Vec<ImportedMessage>,
    native: &Value,
    mapping: &ExternalAdapterMapping,
    role: &str,
) {
    let Some(content) = native
        .pointer(&format!("/llm_calls/{role}"))
        .and_then(Value::as_array)
        .and_then(|calls| calls.last())
        .and_then(|call| call.get("response"))
        .map(value_to_text)
    else {
        return;
    };
    let participant = participant_for_role(mapping, role);
    messages.push(ImportedMessage {
        role: participant
            .map(|participant| participant.role.clone())
            .unwrap_or_else(|| role.to_string()),
        content: strip_final_answer_prefix(&content).to_string(),
        request_id: participant
            .and_then(|participant| participant.request_id.clone())
            .or_else(|| Some(mapping.request_id.clone())),
    });
}

fn microsoft_agent_framework_messages(
    native: &Value,
    mapping: &ExternalAdapterMapping,
) -> Vec<ImportedMessage> {
    let mut messages = Vec::new();
    if let Some(task) = external_task_text(native) {
        messages.push(ImportedMessage {
            role: "user".to_string(),
            content: task,
            request_id: Some(mapping.request_id.clone()),
        });
    }
    if let Some(outputs) = native.get("agent_outputs").and_then(Value::as_object) {
        let mut keys = outputs.keys().collect::<Vec<_>>();
        keys.sort();
        for key in keys {
            let Some(items) = outputs.get(key).and_then(Value::as_array) else {
                continue;
            };
            for item in items {
                let role = item.get("role").and_then(Value::as_str).unwrap_or(key);
                let participant = participant_for_role(mapping, role)
                    .or_else(|| participant_for_native_name(mapping, key));
                messages.push(ImportedMessage {
                    role: participant
                        .map(|participant| participant.role.clone())
                        .unwrap_or_else(|| role.to_string()),
                    content: item
                        .get("text")
                        .map(value_to_text)
                        .unwrap_or_else(|| item.to_string()),
                    request_id: item
                        .get("request_id")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                        .or_else(|| {
                            participant.and_then(|participant| participant.request_id.clone())
                        })
                        .or_else(|| Some(mapping.request_id.clone())),
                });
            }
        }
    }
    if let Some(output) = microsoft_agent_framework_final_output(native) {
        messages.push(ImportedMessage {
            role: "orchestrator".to_string(),
            content: output,
            request_id: Some(mapping.request_id.clone()),
        });
    }
    messages
}

fn microsoft_agent_framework_final_output(native: &Value) -> Option<String> {
    native
        .get("events")?
        .as_array()?
        .iter()
        .rev()
        .find_map(|event| {
            if event.get("type").and_then(Value::as_str) != Some("output") {
                return None;
            }
            event
                .pointer("/data/text")
                .and_then(Value::as_str)
                .or_else(|| {
                    event
                        .pointer("/data/contents")
                        .and_then(Value::as_array)?
                        .iter()
                        .find_map(|content| content.get("text").and_then(Value::as_str))
                })
                .filter(|text| !text.trim().is_empty())
                .map(ToOwned::to_owned)
        })
}

fn participant_for_native_name<'a>(
    mapping: &'a ExternalAdapterMapping,
    native_name: &str,
) -> Option<&'a ExternalParticipantMapping> {
    mapping.participants.iter().find(|participant| {
        participant.native_name.as_deref() == Some(native_name) || participant.role == native_name
    })
}

fn participant_for_role<'a>(
    mapping: &'a ExternalAdapterMapping,
    role: &str,
) -> Option<&'a ExternalParticipantMapping> {
    mapping
        .participants
        .iter()
        .find(|participant| participant.role == role)
}

fn strip_final_answer_prefix(value: &str) -> &str {
    value.strip_prefix("Final Answer: ").unwrap_or(value)
}
