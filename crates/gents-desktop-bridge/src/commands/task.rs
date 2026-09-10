use anyhow::{Result, anyhow, bail};
use gents::graphql::escape_graphql_string;
use gents_desktop_core::client::ClientCore;
use gents_protocol::row::AgentRequestRow;

use super::super::types::{
    EventSourceDeleteRequest, EventSourceSaveRequest, EventTriggerSaveRequest, ScheduleRunRequest,
    ScheduleSaveRequest, TaskRunRequest, TaskRunResult, TaskSaveRequest,
};
use super::util::require_trimmed;

async fn load_agent_request_by_doc_id(
    core: &ClientCore,
    request_doc_id: &str,
) -> Result<AgentRequestRow> {
    let escaped_doc_id = escape_graphql_string(request_doc_id);
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }}, limit: 1) {{
                request_id
                agent_did
                behavior_id
                session_id
                lifecycle_state
            }}
        }}"#
    );
    let response = core.node().execute(&query).await;
    if response.has_errors() {
        bail!(
            "query manual task run request failed: {:?}",
            response.errors
        );
    }

    let row = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(|rows| rows.as_array())
        .and_then(|rows| rows.first())
        .cloned()
        .ok_or_else(|| anyhow!("manual task run request {request_doc_id} was not found"))?;
    serde_json::from_value(row).map_err(Into::into)
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
    let schedule = store
        .schedules
        .iter()
        .find(|row| row.schedule_id == schedule_id)
        .cloned()
        .ok_or_else(|| anyhow!("schedule {schedule_id} was not found"))?;
    let request_doc_id = core.fire_schedule_now(&schedule).await?;
    let row = load_agent_request_by_doc_id(core, &request_doc_id).await?;

    Ok(TaskRunResult {
        request_doc_id,
        request_id: row.request_id,
        session_id: row.session_id.unwrap_or_default(),
        agent_did: row.agent_did.unwrap_or_default(),
        behavior_id: row.behavior_id.unwrap_or_default(),
        lifecycle_state: row.lifecycle_state.map(|state| state.as_str().to_string()),
    })
}

pub async fn save_event_trigger_config(
    core: &ClientCore,
    request: EventTriggerSaveRequest,
) -> Result<()> {
    core.save_event_trigger(&request.document).await
}

pub async fn run_task_config(core: &ClientCore, request: TaskRunRequest) -> Result<TaskRunResult> {
    let task_id = require_trimmed("task_id", request.task_id)?;
    let args = request.args.unwrap_or_else(|| serde_json::json!({}));
    let store = core.store().snapshot();
    let task = store
        .tasks
        .iter()
        .find(|row| row.task_id == task_id)
        .cloned()
        .ok_or_else(|| anyhow!("task {task_id} was not found"))?;
    let request_doc_id = core.fire_task_now(&task, args).await?;
    let row = load_agent_request_by_doc_id(core, &request_doc_id).await?;

    Ok(TaskRunResult {
        request_doc_id,
        request_id: row.request_id,
        session_id: row.session_id.unwrap_or_default(),
        agent_did: row.agent_did.unwrap_or_default(),
        behavior_id: row.behavior_id.unwrap_or_default(),
        lifecycle_state: row.lifecycle_state.map(|state| state.as_str().to_string()),
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
    core.delete_event_source(&request.event_source_id, &request.agent_did)
        .await
}
