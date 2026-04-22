use anyhow::{anyhow, Context, Result};
use tokio::runtime::Runtime;

use crate::client::{ClientCore, ClientPeerStatus, ClientStore};
use crate::manage::actions::{reduce, ManageAction};
use crate::manage::projection::{project_manage, ManageProjection};
use crate::manage::{
    backend_row, behavior_row, event_trigger_row, inference_profile_row, new_draft_for_section,
    schedule_row, task_row, tool_selection_row,
};
use crate::state::{ManageDraft, ManageSection, ManageState};

pub fn sync_from_snapshot(
    manage: &mut ManageState,
    peer_statuses: &[ClientPeerStatus],
    store: &ClientStore,
) -> ManageProjection {
    let projection = project_manage(manage, peer_statuses, store);
    reduce(
        manage,
        ManageAction::SnapshotApplied {
            selected_peer_id: projection.selected_peer_id.clone(),
            selected_agent_did: projection.selected_agent_did.clone(),
            selected_entity_id: projection.selected_entity_id.clone(),
            draft: projection.draft.clone(),
            draft_origin: projection.draft_origin.clone(),
        },
    );
    projection
}

pub fn select_deployment(manage: &mut ManageState, peer_id: String, agent_did: String) {
    reduce(
        manage,
        ManageAction::SelectDeployment { peer_id, agent_did },
    );
}

pub fn select_section(manage: &mut ManageState, section: ManageSection) {
    reduce(manage, ManageAction::SelectSection { section });
}

pub fn select_entity(manage: &mut ManageState, entity_id: String) {
    reduce(manage, ManageAction::SelectEntity { entity_id });
}

pub fn start_new_document(manage: &mut ManageState) {
    let draft = new_draft_for_section(
        manage.selected_section,
        manage.selected_agent_did.as_deref(),
    );
    reduce(manage, ManageAction::StartNewDocument { draft });
}

pub fn discard_draft(
    manage: &mut ManageState,
    peer_statuses: &[ClientPeerStatus],
    store: &ClientStore,
) {
    reduce(manage, ManageAction::DiscardDraft);
    sync_from_snapshot(manage, peer_statuses, store);
}

pub fn apply_draft(
    manage: &mut ManageState,
    client: Option<&ClientCore>,
    runtime: &Runtime,
) -> Result<()> {
    let client = client.context("client core is offline")?;
    let draft = manage
        .draft
        .as_ref()
        .context("no manage draft is selected")?;
    let entity_id = draft.entity_id().to_string();

    let result = match draft {
        ManageDraft::Behavior(draft) => {
            runtime.block_on(client.save_behavior(&behavior_row(draft)?))
        }
        ManageDraft::Backend(draft) => runtime.block_on(client.save_backend(&backend_row(draft)?)),
        ManageDraft::ToolSelection(draft) => {
            runtime.block_on(client.save_tool_selection(&tool_selection_row(draft)?))
        }
        ManageDraft::InferenceProfile(draft) => {
            runtime.block_on(client.save_inference_profile(&inference_profile_row(draft)?))
        }
        ManageDraft::Task(draft) => runtime.block_on(client.save_task(&task_row(draft)?)),
        ManageDraft::Schedule(draft) => {
            runtime.block_on(client.save_schedule(&schedule_row(draft)?))
        }
        ManageDraft::EventTrigger(draft) => {
            runtime.block_on(client.save_event_trigger(&event_trigger_row(draft)?))
        }
    };

    match result {
        Ok(()) => {
            reduce(manage, ManageAction::ApplySucceeded { entity_id });
            let snapshot = client.store().snapshot();
            sync_from_snapshot(manage, &client.peer_statuses(), snapshot.as_ref());
            Ok(())
        }
        Err(error) => {
            let error_text = error.to_string();
            reduce(
                manage,
                ManageAction::MutationFailed {
                    error: error_text.clone(),
                },
            );
            Err(anyhow!(error_text))
        }
    }
}

/// Trigger the selected schedule to fire now.
///
/// Kept as `run_selected_task_now` for call-site stability; the body
/// now targets a `Schedule` document rather than the legacy
/// `ScheduledTask` collection.
pub fn run_selected_task_now(
    manage: &mut ManageState,
    client: Option<&ClientCore>,
    runtime: &Runtime,
) -> Result<()> {
    let client = client.context("client core is offline")?;
    let draft = manage
        .draft
        .as_ref()
        .context("no manage draft is selected")?;

    let result = match draft {
        ManageDraft::Schedule(draft) => {
            runtime.block_on(client.fire_schedule_now(&schedule_row(draft)?))
        }
        _ => Err(anyhow!("run now is only available for schedules")),
    };

    match result {
        Ok(()) => {
            reduce(manage, ManageAction::RunNowSucceeded);
            let snapshot = client.store().snapshot();
            sync_from_snapshot(manage, &client.peer_statuses(), snapshot.as_ref());
            Ok(())
        }
        Err(error) => {
            let error_text = error.to_string();
            reduce(
                manage,
                ManageAction::MutationFailed {
                    error: error_text.clone(),
                },
            );
            Err(anyhow!(error_text))
        }
    }
}

/// Submit the in-progress `fire_task_draft`.
///
/// Parses the JSON args, looks the `TaskRow` up in the store by
/// `task_id`, and dispatches `ClientCore::fire_task_now`. On success
/// the modal is cleared; on any failure the error is written back into
/// the draft's `error` field so the modal stays open and the operator
/// can correct their input.
pub fn submit_fire_task_draft(
    manage: &mut ManageState,
    client: Option<&ClientCore>,
    runtime: &Runtime,
) -> Result<()> {
    let draft = manage
        .fire_task_draft
        .as_ref()
        .context("no fire-task draft is open")?
        .clone();

    let apply_error = |manage: &mut ManageState, message: String| {
        if let Some(draft) = manage.fire_task_draft.as_mut() {
            draft.error = Some(message.clone());
        }
        Err::<(), _>(anyhow!(message))
    };

    let Some(client) = client else {
        return apply_error(manage, "client core is offline".to_string());
    };

    let args = match serde_json::from_str::<serde_json::Value>(&draft.args_text) {
        Ok(value) if value.is_object() => value,
        Ok(_) => {
            return apply_error(manage, "args must be a JSON object".to_string());
        }
        Err(error) => {
            return apply_error(manage, format!("JSON parse error: {error}"));
        }
    };

    let snapshot = client.store().snapshot();
    let task_row = snapshot
        .tasks
        .iter()
        .find(|row| row.task_id == draft.task_id)
        .cloned();
    let Some(task_row) = task_row else {
        return apply_error(
            manage,
            format!("task {} disappeared from store", draft.task_id),
        );
    };

    let fire_result = runtime.block_on(client.fire_task_now(&task_row, args));
    match fire_result {
        Ok(_doc_id) => {
            manage.fire_task_draft = None;
            manage.last_apply_error = None;
            let snapshot = client.store().snapshot();
            sync_from_snapshot(manage, &client.peer_statuses(), snapshot.as_ref());
            Ok(())
        }
        Err(error) => apply_error(manage, error.to_string()),
    }
}
