use anyhow::{anyhow, bail, Result};
use gents::graphql::escape_graphql_string;
use gents_desktop_core::client::ClientCore;
use gents_protocol::row::AgentRequestRow;

use super::super::types::{
    EventSourceDeleteRequest, EventSourceSaveRequest, ScheduleRunRequest, ScheduleSaveRequest,
    TaskRunRequest, TaskRunResult, TaskSaveRequest, TriggerSaveRequest,
};
use super::util::require_trimmed;

async fn load_agent_request_by_request_id(
    core: &ClientCore,
    agent_did: &str,
    request_id: &str,
) -> Result<AgentRequestRow> {
    let escaped_request_id = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }}, limit: 1) {{
                _docID
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

    // Goal-backed manual runs are committed atomically on the managed
    // runtime so Goal, GoalCreationClaim and AgentRequest become visible
    // together. Resolve their document ID at that authoritative operator
    // node instead of racing the runtime-to-client P2P projection.
    let response = core.operator_access(agent_did)?.execute(&query).await?;
    let row = response
        .pointer("/data/AgentRequest/0")
        .cloned()
        .ok_or_else(|| anyhow!("manual task run request {request_id} was not found"))?;
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
    let selected_agent_did = core.selected_agent_did();
    let mut schedules = store
        .schedules
        .iter()
        .filter(|row| row.schedule_id == schedule_id)
        .filter(|row| {
            selected_agent_did
                .as_deref()
                .is_none_or(|agent_did| row.agent_did == agent_did)
        });
    let schedule = schedules
        .next()
        .ok_or_else(|| anyhow!("schedule {schedule_id} was not found"))?;
    if schedules.next().is_some() {
        bail!("schedule {schedule_id} is ambiguous across agent scopes");
    }
    let submitted = core
        .fire_schedule_now_for_agent(&schedule.agent_did, &schedule_id)
        .await?;
    let row =
        load_agent_request_by_request_id(core, &submitted.agent_did, &submitted.request_id).await?;
    let request_doc_id = row
        .doc_id
        .clone()
        .ok_or_else(|| anyhow!("manual schedule run request has no _docID"))?;

    Ok(TaskRunResult {
        request_doc_id,
        request_id: row.request_id,
        session_id: row.session_id.unwrap_or_default(),
        agent_did: row.agent_did.unwrap_or_default(),
        behavior_id: row.behavior_id.unwrap_or_default(),
        lifecycle_state: row.lifecycle_state.map(|state| state.as_str().to_string()),
    })
}

pub async fn save_trigger_config(core: &ClientCore, request: TriggerSaveRequest) -> Result<()> {
    core.save_trigger(&request.document).await
}

pub async fn run_task_config(core: &ClientCore, request: TaskRunRequest) -> Result<TaskRunResult> {
    let task_id = require_trimmed("task_id", request.task_id)?;
    let args = request.args.unwrap_or_else(|| serde_json::json!({}));
    let store = core.store().snapshot();
    let selected_agent_did = core.selected_agent_did();
    let mut tasks = store
        .tasks
        .iter()
        .filter(|row| row.task_id == task_id)
        .filter(|row| {
            selected_agent_did
                .as_deref()
                .is_none_or(|agent_did| row.agent_did == agent_did)
        });
    let task = tasks
        .next()
        .ok_or_else(|| anyhow!("task {task_id} was not found"))?;
    if tasks.next().is_some() {
        bail!("task {task_id} is ambiguous across agent scopes");
    }
    let submitted = core
        .fire_task_now_for_agent(&task.agent_did, &task_id, args)
        .await?;
    let row =
        load_agent_request_by_request_id(core, &submitted.agent_did, &submitted.request_id).await?;
    let request_doc_id = row
        .doc_id
        .clone()
        .ok_or_else(|| anyhow!("manual task run request has no _docID"))?;

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
