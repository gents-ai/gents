use anyhow::{anyhow, bail, Result};
use gents::document_config::{Schedule, Task};
use gents::graphql::escape_graphql_string;
use gents_desktop_core::client::ClientCore;
use gents_protocol::row::AgentRequestRow;

use super::super::types::{
    EventSourceDeleteRequest, EventSourceSaveRequest, ScheduleRunRequest, ScheduleSaveRequest,
    TaskRunRequest, TaskRunResult, TaskSaveRequest, TriggerSaveRequest,
};
use super::util::require_trimmed;

fn schedule_for_run<'a>(
    rows: &'a [Schedule],
    id: &str,
    agent_did: Option<&str>,
) -> Result<&'a Schedule> {
    let mut matches = rows
        .iter()
        .filter(|row| row.schedule_id == id && agent_did.is_none_or(|did| row.agent_did == did));
    let row = matches
        .next()
        .ok_or_else(|| anyhow!("schedule {id} was not found"))?;
    if matches.next().is_some() {
        bail!("schedule {id} is ambiguous across agent scopes");
    }
    Ok(row)
}

fn task_for_run<'a>(rows: &'a [Task], id: &str, agent_did: Option<&str>) -> Result<&'a Task> {
    let mut matches = rows
        .iter()
        .filter(|row| row.task_id == id && agent_did.is_none_or(|did| row.agent_did == did));
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
    agent_did: &str,
    request_id: &str,
) -> Result<AgentRequestRow> {
    let escaped_request_id = escape_graphql_string(request_id);
    let escaped_agent_did = escape_graphql_string(agent_did);
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{escaped_request_id}" }}, agent_did: {{ _eq: "{escaped_agent_did}" }} }}, limit: 1) {{
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
    let selected_agent_did = request
        .agent_did
        .map(|did| require_trimmed("agent_did", did))
        .transpose()?
        .or_else(|| core.selected_agent_did());
    let schedule = schedule_for_run(
        &store.schedules,
        &schedule_id,
        selected_agent_did.as_deref(),
    )?;
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
    let selected_agent_did = request
        .agent_did
        .map(|did| require_trimmed("agent_did", did))
        .transpose()?
        .or_else(|| core.selected_agent_did());
    let task = task_for_run(&store.tasks, &task_id, selected_agent_did.as_deref())?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn task_and_schedule_actions_keep_their_agent_scope_across_views() {
        let tasks: Vec<Task> = ["did:alpha", "did:beta"].into_iter().map(|did| serde_json::from_value(json!({
            "agent_did": did, "task_id": "daily", "behavior_id": "default", "prompt_template": "hello"
        })).unwrap()).collect();
        let schedules: Vec<Schedule> = ["did:alpha", "did:beta"].into_iter().map(|did| serde_json::from_value(json!({
            "agent_did": did, "schedule_id": "daily", "cadence": { "kind": "interval", "interval_secs": 60 }
        })).unwrap()).collect();
        for did in ["did:alpha", "did:beta"] {
            assert_eq!(
                task_for_run(&tasks, "daily", Some(did)).unwrap().agent_did,
                did
            );
            assert_eq!(
                schedule_for_run(&schedules, "daily", Some(did))
                    .unwrap()
                    .agent_did,
                did
            );
        }
        assert!(task_for_run(&tasks, "daily", None)
            .unwrap_err()
            .to_string()
            .contains("ambiguous"));
        assert!(schedule_for_run(&schedules, "daily", None)
            .unwrap_err()
            .to_string()
            .contains("ambiguous"));
        assert!(task_for_run(&tasks, "daily", Some("did:missing")).is_err());
        assert!(schedule_for_run(&schedules, "daily", Some("did:missing")).is_err());
    }
}
