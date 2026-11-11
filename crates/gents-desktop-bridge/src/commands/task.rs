use anyhow::{anyhow, bail, Result};
use gents_desktop_core::client::ClientCore;
use gents_documents::{Schedule, Task};
use gents_protocol::row::AgentRequestRow;
use gents_protocol::operations::request::RequestRef;

use super::super::types::{
    EventSourceDeleteRequest, EventSourceSaveRequest, ScheduleRunRequest, ScheduleSaveRequest,
    TaskRunRequest, TaskRunResult, TaskSaveRequest, TriggerSaveRequest,
};
use super::util::require_trimmed;

fn schedule_for_run<'a>(
    rows: &'a [Schedule],
    id: &str,
    node_did: Option<&str>,
) -> Result<&'a Schedule> {
    let mut matches = rows
        .iter()
        .filter(|row| row.schedule_id == id && node_did.is_none_or(|did| row.node_did == did));
    let row = matches
        .next()
        .ok_or_else(|| anyhow!("schedule {id} was not found"))?;
    if matches.next().is_some() {
        bail!("schedule {id} is ambiguous across agent scopes");
    }
    Ok(row)
}

fn task_for_run<'a>(rows: &'a [Task], id: &str, node_did: Option<&str>) -> Result<&'a Task> {
    let mut matches = rows
        .iter()
        .filter(|row| row.task_id == id && node_did.is_none_or(|did| row.node_did == did));
    let row = matches
        .next()
        .ok_or_else(|| anyhow!("task {id} was not found"))?;
    if matches.next().is_some() {
        bail!("task {id} is ambiguous across agent scopes");
    }
    Ok(row)
}

async fn load_agent_request_by_request_id(
    core: &ClientCore,
    node_did: &str,
    request_id: &str,
) -> Result<AgentRequestRow> {
    gents_session::operations::request::get(
        &core.operator_access(node_did)?,
        &RequestRef {
            node_did: node_did.to_owned(),
            requester_did: None,
            request_id: request_id.to_owned(),
        },
    )
    .await
    .map_err(gents_desktop_core::error::request_operation_failure)
}

pub async fn save_task_config(core: &ClientCore, request: TaskSaveRequest) -> Result<()> {
    core.save_task(&request.document).await
}

pub async fn save_schedule_config(core: &ClientCore, request: ScheduleSaveRequest) -> Result<()> {
    core.save_schedule(&request.document).await
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
        .transpose()?;
    let schedule = schedule_for_run(&store.schedules, &schedule_id, selected_node_did.as_deref())?;
    let submitted = core
        .fire_schedule_now_for_node(&schedule.node_did, &schedule_id)
        .await?;
    let row =
        load_agent_request_by_request_id(core, &submitted.node_did, &submitted.request_id).await?;
    let request_doc_id = row
        .doc_id
        .clone()
        .ok_or_else(|| anyhow!("manual schedule run request has no _docID"))?;

    Ok(TaskRunResult {
        request_doc_id,
        request_id: row.request_id,
        session_id: row.session_id.unwrap_or_default(),
        node_did: row.node_did.unwrap_or_default(),
        agent_did: row.agent_did.unwrap_or_default(),
        lifecycle_state: row.lifecycle_state,
    })
}

pub async fn save_trigger_config(core: &ClientCore, request: TriggerSaveRequest) -> Result<()> {
    core.save_trigger(&request.document).await
}

pub async fn run_task_config(core: &ClientCore, request: TaskRunRequest) -> Result<TaskRunResult> {
    let task_id = require_trimmed("task_id", request.task_id)?;
    let args = request.args.unwrap_or_else(|| serde_json::json!({}));
    let store = core.store().snapshot();
    let selected_node_did = request
        .node_did
        .map(|did| require_trimmed("node_did", did))
        .transpose()?;
    let task = task_for_run(&store.tasks, &task_id, selected_node_did.as_deref())?;
    let submitted = core
        .fire_task_now_for_node(&task.node_did, &task_id, args)
        .await?;
    let row =
        load_agent_request_by_request_id(core, &submitted.node_did, &submitted.request_id).await?;
    let request_doc_id = row
        .doc_id
        .clone()
        .ok_or_else(|| anyhow!("manual task run request has no _docID"))?;

    Ok(TaskRunResult {
        request_doc_id,
        request_id: row.request_id,
        session_id: row.session_id.unwrap_or_default(),
        node_did: row.node_did.unwrap_or_default(),
        agent_did: row.agent_did.unwrap_or_default(),
        lifecycle_state: row.lifecycle_state,
    })
}

pub async fn save_event_source_config(
    core: &ClientCore,
    request: EventSourceSaveRequest,
) -> Result<()> {
    core.save_event_source(&request.document).await
}

pub async fn delete_event_source_config(
    core: &ClientCore,
    request: EventSourceDeleteRequest,
) -> Result<()> {
    core.delete_event_source(&request.event_source_id, &request.node_did)
        .await
}
