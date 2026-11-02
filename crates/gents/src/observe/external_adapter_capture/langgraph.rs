use std::collections::BTreeMap;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use super::{
    imported_external_message, imported_session, timestamp_for_index, value_to_text,
    ExternalAdapterCapture, ExternalAdapterImport, ExternalAdapterMapping, ExternalAdapterView,
    ImportedMessage,
};
use crate::observe::run_timeline::{TimelineRequestRow, TimelineToolCallRow};

pub(super) fn import_langgraph_capture(
    capture: &ExternalAdapterCapture,
    mapping: &ExternalAdapterMapping,
) -> Result<ExternalAdapterImport> {
    let session_id = mapping
        .session_id
        .clone()
        .or_else(|| {
            capture
                .native
                .get("thread_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| format!("external-session:{}", mapping.request_id));
    let status = mapping
        .status
        .clone()
        .or_else(|| {
            latest_langgraph_values(&capture.native)
                .and_then(|values| values.get("status"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| "completed".to_string());
    let lifecycle_state = gents_protocol::request_lifecycle::RequestLifecycleState::parse(&status)
        .with_context(|| format!("invalid mapped request lifecycle state {status:?}"))?;
    let started_at = "2026-06-05T00:00:00Z";
    langgraph_state_history_projection(capture, mapping, &session_id)?;
    let latest_values = latest_langgraph_values(&capture.native);
    let child_request_id = langgraph_child_request_id(latest_values, mapping);
    let child_tool_call_id = latest_values
        .and_then(|values| values.get("tool_call_id"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| {
            mapping
                .tool_events
                .iter()
                .find_map(|event| event.child_request_id.as_ref().map(|_| event.id.clone()))
        })
        .or_else(|| {
            child_request_id
                .as_ref()
                .map(|_| format!("langgraph:child:{}", mapping.request_id))
        });

    let mut requests = vec![TimelineRequestRow {
        request_id: mapping.request_id.clone(),
        node_did: mapping.node_did.clone(),
        agent_id: mapping.agent_id.clone(),
        session_id: Some(session_id.clone()),
        content: latest_values
            .and_then(|values| values.get("topic"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        lifecycle_state: Some(lifecycle_state),
        backend_id: capture.source.package.clone(),
        created_at: Some(started_at.to_string()),
        retry_count: Some(0),
        ..Default::default()
    }];
    if let Some(child_request_id) = child_request_id.as_ref() {
        requests.push(TimelineRequestRow {
            request_id: child_request_id.clone(),
            session_id: Some(session_id.clone()),
            content: Some("Imported LangGraph child request boundary".to_string()),
            lifecycle_state: Some(lifecycle_state),
            backend_id: capture.source.package.clone(),
            created_at: Some(started_at.to_string()),
            retry_count: Some(0),
            caused_by_parent_request_id: Some(mapping.request_id.clone()),
            caused_by_parent_tool_call_id: child_tool_call_id.clone(),
            ..Default::default()
        });
    }

    let messages = langgraph_messages(&capture.native)
        .into_iter()
        .enumerate()
        .map(|(index, message)| {
            imported_external_message(
                &session_id,
                &message
                    .request_id
                    .unwrap_or_else(|| mapping.request_id.clone()),
                (index as i64) + 1,
                &message.role,
                &message.content,
            )
        })
        .collect::<Vec<_>>();
    let tool_calls = child_request_id
        .as_ref()
        .map(|child_request_id| TimelineToolCallRow {
            request_id: Some(mapping.request_id.clone()),
            session_id: session_id.clone(),
            message_sequence: Some(messages.len().max(1) as i64),
            tool_name: "langgraph_child_boundary".to_string(),
            tool_call_id: child_tool_call_id
                .clone()
                .unwrap_or_else(|| format!("langgraph:child:{}", mapping.request_id)),
            args: json!({
                "source_system": capture.source.system,
                "child_request_id": child_request_id,
            })
            .to_string(),
            // The capture records the boundary, not a native tool delivery.
            result: None,
            status: status.clone(),
            started_at: Some(timestamp_for_index(messages.len().max(1))),
            completed_at: Some(timestamp_for_index(messages.len().max(1))),
            child_request_id: Some(child_request_id.clone()),
            ..Default::default()
        })
        .into_iter()
        .collect::<Vec<_>>();
    let root = requests
        .iter()
        .find(|request| request.request_id == mapping.request_id)
        .cloned()
        .context("imported LangGraph rows missing root request")?;
    let scenario_id = mapping
        .scenario_id
        .clone()
        .unwrap_or_else(|| format!("{}:{}", capture.source.system, mapping.request_id));
    Ok(ExternalAdapterImport {
        projection: mapping.projection,
        actor_did: mapping.actor_did.clone(),
        source_system: capture.source.system.clone(),
        scenario_id,
        view: ExternalAdapterView {
            request: root,
            session: imported_session(
                &session_id,
                mapping.node_did.as_deref(),
                mapping.agent_id.as_deref(),
                started_at,
                "Imported LangGraph state history".to_string(),
                latest_values
                    .and_then(|values| values.get("topic"))
                    .and_then(Value::as_str),
            ),
            requests,
            messages,
            tool_calls,
        },
    })
}

pub(crate) fn langgraph_state_history_projection(
    capture: &ExternalAdapterCapture,
    mapping: &ExternalAdapterMapping,
    session_id: &str,
) -> Result<crate::observe::adapter_projection::LangGraphStateHistoryProjection> {
    let latest_snapshot = capture
        .native
        .get("history")
        .and_then(Value::as_array)
        .and_then(|history| history.first())
        .context("LangGraph capture missing native.history[0]")?;
    let mut values = latest_snapshot
        .get("values")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    values.insert("request_id".to_string(), json!(mapping.request_id));
    values.insert(
        "history_checkpoint_count".to_string(),
        json!(capture
            .native
            .get("history")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or_default()),
    );
    if let Some(package_version) = capture.source.package_version.as_deref() {
        values.insert(
            "langgraph_package_version".to_string(),
            json!(package_version),
        );
    }
    if let Some(provider) = capture.native.get("provider") {
        values.insert("provider".to_string(), provider.clone());
    }

    serde_json::from_value(json!({
        "thread_id": capture
            .native
            .get("thread_id")
            .and_then(Value::as_str)
            .unwrap_or(session_id),
        "checkpoint_id": latest_snapshot
            .pointer("/config/configurable/checkpoint_id")
            .and_then(Value::as_str)
            .unwrap_or("langgraph:checkpoint:missing"),
        "root_request_id": mapping.request_id,
        "values": Value::Object(values),
        "nodes": langgraph_projection_nodes(&capture.native, mapping),
        "edges": capture
            .native
            .pointer("/graph/edges")
            .cloned()
            .unwrap_or_else(|| json!([])),
        "tasks": langgraph_projection_tasks(&capture.native, mapping),
    }))
    .context("decoding captured LangGraph projection")
}

fn latest_langgraph_values(native: &Value) -> Option<&serde_json::Map<String, Value>> {
    native
        .get("history")?
        .as_array()?
        .first()?
        .get("values")?
        .as_object()
}

fn langgraph_projection_nodes(native: &Value, mapping: &ExternalAdapterMapping) -> Vec<Value> {
    let values = latest_langgraph_values(native);
    let status = values
        .and_then(|values| values.get("status"))
        .and_then(Value::as_str)
        .unwrap_or("completed");
    let child_request_id = langgraph_child_request_id(values, mapping);
    let mut nodes = vec![json!({
        "id": "langgraph:start",
        "kind": "start",
        "status": "completed",
    })];
    for name in native
        .pointer("/graph/nodes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        let (id, kind, request_id) = if name.ends_with("_subgraph") {
            (
                format!("langgraph:subgraph:{}", name.trim_end_matches("_subgraph")),
                "subgraph",
                child_request_id
                    .as_deref()
                    .unwrap_or(mapping.request_id.as_str()),
            )
        } else {
            (
                format!("langgraph:node:{name}"),
                if name == "provider_model" {
                    "provider_node"
                } else {
                    "node"
                },
                if name == "provider_tool" {
                    child_request_id
                        .as_deref()
                        .unwrap_or(mapping.request_id.as_str())
                } else {
                    mapping.request_id.as_str()
                },
            )
        };
        nodes.push(json!({
            "id": id,
            "kind": kind,
            "request_id": request_id,
            "status": status,
        }));
    }
    if let Some(subgraphs) = native
        .pointer("/graph/subgraphs")
        .and_then(Value::as_object)
    {
        for (subgraph_name, subgraph) in subgraphs {
            let prefix = subgraph_name.trim_end_matches("_subgraph");
            nodes.push(json!({
                "id": format!("langgraph:subgraph:{prefix}:start"),
                "kind": "subgraph_start",
                "request_id": child_request_id.as_deref().unwrap_or(mapping.request_id.as_str()),
                "status": "completed",
            }));
            for name in subgraph
                .get("nodes")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                nodes.push(json!({
                    "id": format!("langgraph:subgraph:{prefix}:{name}"),
                    "kind": "subgraph_node",
                    "request_id": child_request_id.as_deref().unwrap_or(mapping.request_id.as_str()),
                    "status": status,
                }));
            }
            nodes.push(json!({
                "id": format!("langgraph:subgraph:{prefix}:end"),
                "kind": "subgraph_end",
                "request_id": child_request_id.as_deref().unwrap_or(mapping.request_id.as_str()),
                "status": status,
            }));
        }
    }
    nodes.push(json!({
        "id": "langgraph:end",
        "kind": "end",
        "status": status,
    }));
    nodes
}

fn langgraph_projection_tasks(native: &Value, mapping: &ExternalAdapterMapping) -> Vec<Value> {
    let values = latest_langgraph_values(native);
    let child_request_id = langgraph_child_request_id(values, mapping);
    let native_tasks = collect_langgraph_native_tasks(native);
    let mut tasks = Vec::new();
    for name in native
        .pointer("/graph/nodes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        tasks.push(langgraph_task_value(
            name,
            &native_tasks,
            if name == "provider_tool" {
                child_request_id
                    .as_deref()
                    .unwrap_or(mapping.request_id.as_str())
            } else {
                mapping.request_id.as_str()
            },
            langgraph_task_child_request_id(name, child_request_id.as_deref()),
        ));
    }
    if let Some(subgraphs) = native
        .pointer("/graph/subgraphs")
        .and_then(Value::as_object)
    {
        for subgraph in subgraphs.values() {
            for name in subgraph
                .get("nodes")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                tasks.push(langgraph_task_value(
                    name,
                    &native_tasks,
                    child_request_id
                        .as_deref()
                        .unwrap_or(mapping.request_id.as_str()),
                    None,
                ));
            }
        }
    }
    tasks
}

fn langgraph_task_value(
    name: &str,
    native_tasks: &BTreeMap<String, Value>,
    request_id: &str,
    task_child_request_id: Option<&str>,
) -> Value {
    let task = native_tasks.get(name);
    let status = if task
        .and_then(|task| task.get("error"))
        .is_some_and(|error| !error.is_null())
    {
        "failed"
    } else if task
        .and_then(|task| task.get("result"))
        .is_some_and(Value::is_null)
    {
        "pending"
    } else {
        "completed"
    };
    let mut value = json!({
        "id": task
            .and_then(|task| task.get("id"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| format!("langgraph:task:{name}")),
        "request_id": request_id,
        "name": name,
        "status": status,
    });
    if let Some(task_child_request_id) = task_child_request_id {
        if let Some(object) = value.as_object_mut() {
            object.insert("child_request_id".to_string(), json!(task_child_request_id));
        }
    }
    value
}

fn langgraph_task_child_request_id<'a>(
    name: &str,
    child_request_id: Option<&'a str>,
) -> Option<&'a str> {
    match name {
        "delegate" | "review_subgraph" | "provider_model" => child_request_id,
        _ => None,
    }
}

fn langgraph_child_request_id(
    values: Option<&serde_json::Map<String, Value>>,
    mapping: &ExternalAdapterMapping,
) -> Option<String> {
    values
        .and_then(|values| values.get("child_request_id"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| {
            mapping
                .tool_events
                .iter()
                .find_map(|event| event.child_request_id.clone())
        })
        .or_else(|| {
            values
                .and_then(|values| values.get("tool_call_id"))
                .and_then(Value::as_str)
                .map(|_| format!("{}:tool", mapping.request_id))
        })
}

fn collect_langgraph_native_tasks(native: &Value) -> BTreeMap<String, Value> {
    let mut tasks = BTreeMap::new();
    for snapshot in native
        .get("history")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .rev()
    {
        for task in snapshot
            .get("tasks")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(name) = task.get("name").and_then(Value::as_str) {
                tasks.insert(name.to_string(), task.clone());
            }
        }
    }
    tasks
}

fn langgraph_messages(native: &Value) -> Vec<ImportedMessage> {
    latest_langgraph_values(native)
        .and_then(|values| values.get("messages"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|message| match message {
            Value::String(content) => Some(ImportedMessage {
                role: "state".to_string(),
                content: content.clone(),
                request_id: None,
            }),
            Value::Object(object) => object.get("content").map(|content| ImportedMessage {
                role: object
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("message")
                    .to_string(),
                content: value_to_text(content),
                request_id: None,
            }),
            value => Some(ImportedMessage {
                role: "state".to_string(),
                content: value_to_text(value),
                request_id: None,
            }),
        })
        .collect()
}
