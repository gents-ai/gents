use anyhow::{anyhow, Context, Result};
use tokio::runtime::Runtime;

use crate::client::{ClientCore, ClientPeerStatus, ClientStore};
use crate::manage::actions::{reduce, ManageAction};
use crate::manage::projection::{project_manage, ManageProjection};
use crate::manage::{
    backend_row, behavior_row, inference_profile_row, new_draft_for_section, scheduled_task_row,
    tool_selection_row,
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
    reduce(manage, ManageAction::SelectDeployment { peer_id, agent_did });
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
        ManageDraft::Backend(draft) => {
            runtime.block_on(client.save_backend(&backend_row(draft)?))
        }
        ManageDraft::ToolSelection(draft) => {
            runtime.block_on(client.save_tool_selection(&tool_selection_row(draft)?))
        }
        ManageDraft::InferenceProfile(draft) => {
            runtime.block_on(client.save_inference_profile(&inference_profile_row(draft)?))
        }
        ManageDraft::ScheduledTask(draft) => {
            runtime.block_on(client.save_scheduled_task(&scheduled_task_row(draft)?))
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
        ManageDraft::ScheduledTask(draft) => {
            runtime.block_on(client.run_scheduled_task_now(&scheduled_task_row(draft)?))
        }
        _ => Err(anyhow!("run now is only available for scheduled tasks")),
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
