use std::collections::{BTreeMap, BTreeSet};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::adapter_projection::{AdapterProjectionEnvelope, AdapterProjectionKind};
use crate::run_timeline::{
    RunTimelineRows, TimelineConversationRow, TimelineMessageRow, TimelineRequestRow,
    TimelineResponseRow, TimelineSessionRow, TimelineToolCallRow,
};

#[derive(Debug, Clone, Deserialize)]
pub struct ExternalAdapterCapture {
    pub source: ExternalAdapterSource,
    #[serde(default)]
    pub native: Value,
    #[serde(default)]
    pub mapping: Option<ExternalAdapterMapping>,
    #[serde(default)]
    pub envelope: Option<AdapterProjectionEnvelope>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExternalAdapterSource {
    pub system: String,
    #[serde(default)]
    pub package: Option<String>,
    #[serde(default)]
    pub package_version: Option<String>,
    #[serde(default)]
    pub generator: Option<String>,
    #[serde(default)]
    pub capture: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExternalAdapterMapping {
    #[serde(alias = "projection_id")]
    pub projection: AdapterProjectionKind,
    #[serde(default)]
    pub scenario_id: Option<String>,
    pub request_id: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub agent_did: Option<String>,
    #[serde(default)]
    pub behavior_id: Option<String>,
    #[serde(default)]
    pub actor_did: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub participants: Vec<ExternalParticipantMapping>,
    #[serde(default)]
    pub delegations: Vec<ExternalDelegationMapping>,
    #[serde(default)]
    pub tool_events: Vec<ExternalToolEventMapping>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ExternalParticipantMapping {
    #[serde(default)]
    pub native_name: Option<String>,
    pub role: String,
    #[serde(default)]
    pub agent_did: Option<String>,
    #[serde(default)]
    pub behavior_id: Option<String>,
    #[serde(default)]
    pub request_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExternalDelegationMapping {
    pub parent_request_id: String,
    pub child_request_id: String,
    #[serde(default)]
    pub parent_tool_call_id: Option<String>,
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub agent_did: Option<String>,
    #[serde(default)]
    pub behavior_id: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExternalToolEventMapping {
    pub id: String,
    pub request_id: String,
    pub tool_name: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub child_request_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ExternalAdapterImport {
    pub projection: AdapterProjectionKind,
    pub rows: RunTimelineRows,
    pub actor_did: Option<String>,
    pub source_system: String,
    pub scenario_id: String,
}

#[derive(Debug, Clone)]
struct ImportedMessage {
    role: String,
    content: String,
}

pub fn import_external_adapter_capture_to_timeline_rows(
    capture: &ExternalAdapterCapture,
) -> Result<ExternalAdapterImport> {
    let mapping = capture.mapping.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "external adapter capture {} is missing mapping metadata",
            capture.source.system
        )
    })?;
    match mapping.projection {
        AdapterProjectionKind::MultiAgentTask => import_multi_agent_capture(capture, mapping),
        AdapterProjectionKind::OpenAiCodexRunTrace
        | AdapterProjectionKind::LangGraphStateHistory => {
            bail!(
                "external adapter import for projection {} is not implemented",
                mapping.projection.id()
            )
        }
    }
}

fn import_multi_agent_capture(
    capture: &ExternalAdapterCapture,
    mapping: &ExternalAdapterMapping,
) -> Result<ExternalAdapterImport> {
    let session_id = mapping
        .session_id
        .clone()
        .unwrap_or_else(|| format!("external-session:{}", mapping.request_id));
    let status = mapping
        .status
        .clone()
        .unwrap_or_else(|| "completed".to_string());
    let started_at = "2026-06-05T00:00:00Z";
    let root_participant = mapping.participants.iter().find(|participant| {
        participant
            .request_id
            .as_deref()
            .is_none_or(|request_id| request_id == mapping.request_id)
    });
    let root_agent_did = first_owned([
        mapping.agent_did.as_deref(),
        root_participant.and_then(|participant| participant.agent_did.as_deref()),
    ]);
    let root_behavior_id = first_owned([
        mapping.behavior_id.as_deref(),
        root_participant.and_then(|participant| participant.behavior_id.as_deref()),
    ]);
    let participant_by_request = mapping
        .participants
        .iter()
        .filter_map(|participant| {
            let request_id = participant.request_id.as_ref()?;
            Some((request_id.as_str(), participant))
        })
        .collect::<BTreeMap<_, _>>();
    let delegation_by_child = mapping
        .delegations
        .iter()
        .map(|delegation| (delegation.child_request_id.as_str(), delegation))
        .collect::<BTreeMap<_, _>>();
    let child_request_ids = child_request_ids(mapping);
    let mut requests = Vec::new();
    requests.push(TimelineRequestRow {
        request_id: mapping.request_id.clone(),
        agent_did: root_agent_did.clone(),
        behavior_id: root_behavior_id.clone(),
        session_id: Some(session_id.clone()),
        content: external_task_text(&capture.native),
        metadata: Some(root_metadata(capture, mapping)?),
        status: Some(status.clone()),
        lifecycle_state: Some(status.clone()),
        backend_id: capture.source.package.clone(),
        created_at: Some(started_at.to_string()),
        retry_count: Some(0),
        ..Default::default()
    });

    for child_request_id in child_request_ids {
        let participant = participant_by_request
            .get(child_request_id.as_str())
            .copied();
        let delegation = delegation_by_child.get(child_request_id.as_str()).copied();
        requests.push(TimelineRequestRow {
            request_id: child_request_id.clone(),
            agent_did: first_owned([
                participant.and_then(|participant| participant.agent_did.as_deref()),
                delegation.and_then(|delegation| delegation.agent_did.as_deref()),
            ]),
            behavior_id: first_owned([
                participant.and_then(|participant| participant.behavior_id.as_deref()),
                delegation.and_then(|delegation| delegation.behavior_id.as_deref()),
            ]),
            session_id: Some(session_id.clone()),
            content: participant
                .and_then(|participant| participant.native_name.as_deref())
                .map(|name| format!("Imported external participant {name}")),
            metadata: participant
                .map(participant_metadata)
                .transpose()
                .context("serializing child request participant metadata")?,
            status: Some(status.clone()),
            lifecycle_state: Some(status.clone()),
            backend_id: capture.source.package.clone(),
            created_at: Some(started_at.to_string()),
            retry_count: Some(0),
            caused_by_parent_request_id: delegation
                .map(|delegation| delegation.parent_request_id.clone()),
            caused_by_parent_tool_call_id: delegation.and_then(|delegation| {
                delegation
                    .parent_tool_call_id
                    .clone()
                    .or_else(|| Some(default_delegation_tool_call_id(delegation)))
            }),
            ..Default::default()
        });
    }

    let messages = native_messages(&capture.source.system, &capture.native)
        .into_iter()
        .enumerate()
        .map(|(index, message)| TimelineMessageRow {
            session_id: session_id.clone(),
            sequence: (index as i64) + 1,
            role: message.role,
            content: message.content,
            timestamp: Some(timestamp_for_index(index + 1)),
            ..Default::default()
        })
        .collect::<Vec<_>>();

    let mut seen_tool_call_ids = BTreeSet::new();
    let mut tool_calls = Vec::new();
    for (index, delegation) in mapping.delegations.iter().enumerate() {
        let tool_call_id = delegation
            .parent_tool_call_id
            .clone()
            .unwrap_or_else(|| default_delegation_tool_call_id(delegation));
        seen_tool_call_ids.insert(tool_call_id.clone());
        tool_calls.push(TimelineToolCallRow {
            request_id: Some(delegation.parent_request_id.clone()),
            session_id: session_id.clone(),
            message_sequence: Some((index as i64) + 1),
            tool_name: delegation
                .tool_name
                .clone()
                .unwrap_or_else(|| "handoff".to_string()),
            tool_call_id,
            args: json!({
                "source_system": capture.source.system,
                "child_request_id": delegation.child_request_id,
            })
            .to_string(),
            result: "external framework delegation imported".to_string(),
            status: delegation
                .status
                .clone()
                .unwrap_or_else(|| "completed".to_string()),
            started_at: Some(timestamp_for_index(index + 1)),
            completed_at: Some(timestamp_for_index(index + 1)),
            child_request_id: Some(delegation.child_request_id.clone()),
            ..Default::default()
        });
    }
    for (index, event) in mapping.tool_events.iter().enumerate() {
        if !seen_tool_call_ids.insert(event.id.clone()) {
            continue;
        }
        tool_calls.push(TimelineToolCallRow {
            request_id: Some(event.request_id.clone()),
            session_id: session_id.clone(),
            message_sequence: Some((index as i64) + 1),
            tool_name: event.tool_name.clone(),
            tool_call_id: event.id.clone(),
            args: json!({ "source_system": capture.source.system }).to_string(),
            result: "external framework tool event imported".to_string(),
            status: event
                .status
                .clone()
                .unwrap_or_else(|| "completed".to_string()),
            started_at: Some(timestamp_for_index(index + 1)),
            completed_at: Some(timestamp_for_index(index + 1)),
            child_request_id: event.child_request_id.clone(),
            ..Default::default()
        });
    }

    let mut responses = requests
        .iter()
        .map(|request| TimelineResponseRow {
            request_id: request.request_id.clone(),
            agent_did: request.agent_did.clone(),
            behavior_id: request.behavior_id.clone(),
            session_id: Some(session_id.clone()),
            content: Some(response_content_for_request(request, &messages)),
            status: Some(status.clone()),
            materialized_message_sequence: (request.request_id == mapping.request_id)
                .then_some(messages.len() as i64),
            created_at: Some(started_at.to_string()),
            completed_at: Some(timestamp_for_index(messages.len() + 1)),
            ..Default::default()
        })
        .collect::<Vec<_>>();
    if responses.is_empty() {
        responses.push(TimelineResponseRow {
            request_id: mapping.request_id.clone(),
            session_id: Some(session_id.clone()),
            status: Some(status.clone()),
            created_at: Some(started_at.to_string()),
            completed_at: Some(timestamp_for_index(messages.len() + 1)),
            ..Default::default()
        });
    }

    let root = requests
        .iter()
        .find(|request| request.request_id == mapping.request_id)
        .cloned()
        .context("imported rows missing root request")?;
    let scenario_id = mapping
        .scenario_id
        .clone()
        .unwrap_or_else(|| format!("{}:{}", capture.source.system, mapping.request_id));
    Ok(ExternalAdapterImport {
        projection: mapping.projection,
        actor_did: mapping.actor_did.clone(),
        source_system: capture.source.system.clone(),
        scenario_id,
        rows: RunTimelineRows {
            request: root,
            session: Some(TimelineSessionRow {
                session_id: session_id.clone(),
                agent_name: root_agent_did
                    .as_deref()
                    .or(root_behavior_id.as_deref())
                    .map(ToOwned::to_owned),
                behavior_id: root_behavior_id.clone(),
                started: Some(started_at.to_string()),
                status: Some(status.clone()),
                ..Default::default()
            }),
            conversation: Some(TimelineConversationRow {
                session_id,
                agent_name: root_agent_did
                    .as_deref()
                    .or(root_behavior_id.as_deref())
                    .map(ToOwned::to_owned),
                agent_did: root_agent_did,
                behavior_id: root_behavior_id,
                title: Some(format!("Imported {}", capture.source.system)),
                title_source: Some("external_adapter_capture".to_string()),
                preview_text: external_task_text(&capture.native),
                status: Some(status),
                created_at: Some(started_at.to_string()),
                updated_at: Some(timestamp_for_index(messages.len() + 1)),
                latest_request_id: Some(mapping.request_id.clone()),
                ..Default::default()
            }),
            requests,
            messages,
            tool_calls,
            responses,
        },
    })
}

fn child_request_ids(mapping: &ExternalAdapterMapping) -> Vec<String> {
    let mut ids = BTreeSet::new();
    for participant in &mapping.participants {
        if let Some(request_id) = participant.request_id.as_deref() {
            if request_id != mapping.request_id {
                ids.insert(request_id.to_string());
            }
        }
    }
    for delegation in &mapping.delegations {
        if delegation.child_request_id != mapping.request_id {
            ids.insert(delegation.child_request_id.clone());
        }
    }
    ids.into_iter().collect()
}

fn external_task_text(native: &Value) -> Option<String> {
    native
        .get("task")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn root_metadata(
    capture: &ExternalAdapterCapture,
    mapping: &ExternalAdapterMapping,
) -> Result<String> {
    serde_json::to_string(&json!({
        "adapter_projection": {
            "source_system": capture.source.system,
            "source_package": capture.source.package,
            "source_package_version": capture.source.package_version,
            "scenario_id": mapping.scenario_id,
            "role": mapping
                .participants
                .iter()
                .find(|participant| {
                    participant
                        .request_id
                        .as_deref()
                        .is_none_or(|request_id| request_id == mapping.request_id)
                })
                .map(|participant| participant.role.as_str())
                .unwrap_or("owner"),
            "participants": mapping.participants,
        }
    }))
    .context("serializing external adapter root metadata")
}

fn participant_metadata(participant: &ExternalParticipantMapping) -> Result<String> {
    serde_json::to_string(&json!({
        "adapter_projection": {
            "role": participant.role,
            "native_name": participant.native_name,
        }
    }))
    .context("serializing external adapter participant metadata")
}

fn native_messages(source_system: &str, native: &Value) -> Vec<ImportedMessage> {
    match source_system {
        "autogen-agentchat" => autogen_messages(native),
        "crewai" => crewai_messages(native),
        "microsoft-agent-framework" => microsoft_agent_framework_messages(native),
        _ => Vec::new(),
    }
}

fn autogen_messages(native: &Value) -> Vec<ImportedMessage> {
    native
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|message| {
            Some(ImportedMessage {
                role: message.get("source")?.as_str()?.to_string(),
                content: value_to_text(message.get("content")?),
            })
        })
        .collect()
}

fn crewai_messages(native: &Value) -> Vec<ImportedMessage> {
    native
        .get("tasks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|task| {
            let role = task
                .pointer("/agent/role")
                .and_then(Value::as_str)
                .unwrap_or("agent");
            let output = task.pointer("/output/raw").or_else(|| task.get("output"))?;
            Some(ImportedMessage {
                role: role.to_string(),
                content: value_to_text(output),
            })
        })
        .collect()
}

fn microsoft_agent_framework_messages(native: &Value) -> Vec<ImportedMessage> {
    let mut messages = Vec::new();
    if let Some(task) = external_task_text(native) {
        messages.push(ImportedMessage {
            role: "user".to_string(),
            content: task,
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
                messages.push(ImportedMessage {
                    role: item
                        .get("role")
                        .and_then(Value::as_str)
                        .unwrap_or(key)
                        .to_string(),
                    content: item
                        .get("text")
                        .map(value_to_text)
                        .unwrap_or_else(|| item.to_string()),
                });
            }
        }
    }
    messages
}

fn response_content_for_request(
    request: &TimelineRequestRow,
    messages: &[TimelineMessageRow],
) -> String {
    let Some(metadata) = request.metadata.as_deref() else {
        return "external framework request imported".to_string();
    };
    let native_name = serde_json::from_str::<Value>(metadata)
        .ok()
        .and_then(|value| {
            value
                .pointer("/adapter_projection/native_name")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        });
    if let Some(native_name) = native_name {
        if let Some(message) = messages
            .iter()
            .rev()
            .find(|message| message.role == native_name)
        {
            return message.content.clone();
        }
    }
    messages
        .last()
        .map(|message| message.content.clone())
        .unwrap_or_else(|| "external framework request imported".to_string())
}

fn default_delegation_tool_call_id(delegation: &ExternalDelegationMapping) -> String {
    format!(
        "external:{}:{}",
        delegation.parent_request_id, delegation.child_request_id
    )
}

fn timestamp_for_index(index: usize) -> String {
    format!("2026-06-05T00:00:{:02}Z", index.min(59))
}

fn value_to_text(value: &Value) -> String {
    value
        .as_str()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| value.to_string())
}

fn first_owned<'a>(values: impl IntoIterator<Item = Option<&'a str>>) -> Option<String> {
    values
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}
