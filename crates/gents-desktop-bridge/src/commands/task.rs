use gents::document_config::{Schedule, Task};
use gents::graphql::{escape_graphql_string, graphql_with_transaction_retry};
use gents_desktop_core::client::ClientCore;
use gents_protocol::row::AgentRequestRow;

use super::super::types::{
    EventSourceDeleteRequest, EventSourceSaveRequest, ScheduleRunRequest, ScheduleSaveRequest,
    TaskRunRequest, TaskRunResult, TaskSaveRequest, TriggerSaveRequest,
};
use super::util::{require_trimmed, BridgeCommandError, Result};

fn schedule_for_run<'a>(
    rows: &'a [Schedule],
    id: &str,
    node_did: Option<&str>,
) -> Result<&'a Schedule, BridgeCommandError> {
    let mut matches = rows
        .iter()
        .filter(|row| row.schedule_id == id && node_did.is_none_or(|did| row.node_did == did));
    let row = matches
        .next()
        .ok_or_else(|| BridgeCommandError::ScheduleNotFound { id: id.to_owned() })?;
    if matches.next().is_some() {
        return Err(BridgeCommandError::ScheduleAmbiguous { id: id.to_owned() });
    }
    Ok(row)
}

fn task_for_run<'a>(
    rows: &'a [Task],
    id: &str,
    node_did: Option<&str>,
) -> Result<&'a Task, BridgeCommandError> {
    let mut matches = rows
        .iter()
        .filter(|row| row.task_id == id && node_did.is_none_or(|did| row.node_did == did));
    let row = matches
        .next()
        .ok_or_else(|| BridgeCommandError::TaskNotFound { id: id.to_owned() })?;
    if matches.next().is_some() {
        return Err(BridgeCommandError::TaskAmbiguous { id: id.to_owned() });
    }
    Ok(row)
}

async fn load_agent_request_by_request_id(
    core: &ClientCore,
    node_did: &str,
    request_id: &str,
) -> Result<AgentRequestRow> {
    let escaped_request_id = escape_graphql_string(request_id);
    let escaped_node_did = escape_graphql_string(node_did);
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{escaped_request_id}" }}, node_did: {{ _eq: "{escaped_node_did}" }} }}, limit: 1) {{
                _docID
                request_id
                node_did
                agent_id
                session_id
                lifecycle_state
            }}
        }}"#
    );
    let response =
        graphql_with_transaction_retry(core.node(), &query, "query manual task run request")
            .await?;
    let local_row = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(|rows| rows.as_array())
        .and_then(|rows| rows.first())
        .cloned();
    if let Some(row) = local_row {
        return serde_json::from_value(row).map_err(Into::into);
    }

    // Goal-backed manual runs are committed on the managed runtime; resolve
    // the document ID there rather than racing its P2P projection.
    let response = core.operator_access(node_did)?.execute(&query).await?;
    let row = response
        .pointer("/data/AgentRequest/0")
        .cloned()
        .ok_or_else(|| BridgeCommandError::ManualRunRequestNotFound {
            request_id: request_id.to_owned(),
        })?;
    serde_json::from_value(row).map_err(Into::into)
}

pub async fn save_task_config(core: &ClientCore, request: TaskSaveRequest) -> Result<()> {
    Ok(core.save_task(&request.document).await?)
}

pub async fn save_schedule_config(core: &ClientCore, request: ScheduleSaveRequest) -> Result<()> {
    Ok(core.save_schedule(&request.document).await?)
}

pub async fn run_schedule_config(
    core: &ClientCore,
    request: ScheduleRunRequest,
) -> Result<TaskRunResult> {
    let schedule_id = require_trimmed("schedule_id", request.schedule_id)?;
    let store = core.store().snapshot();
    let selected_node_did = request
        .node_did
        .map(|did| require_trimmed("node_did", did))
        .transpose()?
        .or_else(|| core.selected_node_did());
    let schedule = schedule_for_run(&store.schedules, &schedule_id, selected_node_did.as_deref())?;
    let submitted = core
        .fire_schedule_now_for_node(&schedule.node_did, &schedule_id)
        .await?;
    let row =
        load_agent_request_by_request_id(core, &submitted.node_did, &submitted.request_id).await?;
    let request_doc_id = row
        .doc_id
        .clone()
        .ok_or(BridgeCommandError::ManualScheduleRunNoDocId)?;

    Ok(TaskRunResult {
        request_doc_id,
        request_id: row.request_id,
        session_id: row.session_id.unwrap_or_default(),
        node_did: row.node_did.unwrap_or_default(),
        agent_id: row.agent_id.unwrap_or_default(),
        lifecycle_state: row.lifecycle_state.map(|state| state.as_str().to_string()),
    })
}

pub async fn save_trigger_config(core: &ClientCore, request: TriggerSaveRequest) -> Result<()> {
    Ok(core.save_trigger(&request.document).await?)
}

pub async fn run_task_config(core: &ClientCore, request: TaskRunRequest) -> Result<TaskRunResult> {
    let task_id = require_trimmed("task_id", request.task_id)?;
    let args = request.args.unwrap_or_else(|| serde_json::json!({}));
    let store = core.store().snapshot();
    let selected_node_did = request
        .node_did
        .map(|did| require_trimmed("node_did", did))
        .transpose()?
        .or_else(|| core.selected_node_did());
    let task = task_for_run(&store.tasks, &task_id, selected_node_did.as_deref())?;
    let submitted = core
        .fire_task_now_for_node(&task.node_did, &task_id, args)
        .await?;
    let row =
        load_agent_request_by_request_id(core, &submitted.node_did, &submitted.request_id).await?;
    let request_doc_id = row
        .doc_id
        .clone()
        .ok_or(BridgeCommandError::ManualTaskRunNoDocId)?;

    Ok(TaskRunResult {
        request_doc_id,
        request_id: row.request_id,
        session_id: row.session_id.unwrap_or_default(),
        node_did: row.node_did.unwrap_or_default(),
        agent_id: row.agent_id.unwrap_or_default(),
        lifecycle_state: row.lifecycle_state.map(|state| state.as_str().to_string()),
    })
}

pub async fn save_event_source_config(
    core: &ClientCore,
    request: EventSourceSaveRequest,
) -> Result<()> {
    Ok(core.save_event_source(&request.document).await?)
}

pub async fn delete_event_source_config(
    core: &ClientCore,
    request: EventSourceDeleteRequest,
) -> Result<()> {
    Ok(core
        .delete_event_source(&request.event_source_id, &request.node_did)
        .await?)
}
