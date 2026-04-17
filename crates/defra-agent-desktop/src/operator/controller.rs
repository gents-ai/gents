use anyhow::{anyhow, Context, Result};
use tokio::runtime::Runtime;

use crate::client::{ClientCore, ClientPeerStatus, ClientStore};
use crate::operator::actions::{reduce, OperatorAction};
use crate::operator::projection::{project_operator, OperatorProjection};
use crate::state::{OperatorDraft, OperatorSection, OperatorState};
use crate::views::operator::drafts::new_draft_for_section;
use crate::views::operator::editors::{
    backend_row, behavior_row, inference_profile_row, scheduled_task_row, tool_selection_row,
};

pub fn sync_from_snapshot(
    operator: &mut OperatorState,
    peer_statuses: &[ClientPeerStatus],
    store: &ClientStore,
) -> OperatorProjection {
    let projection = project_operator(operator, peer_statuses, store);
    reduce(
        operator,
        OperatorAction::SnapshotApplied {
            selected_peer_id: projection.selected_peer_id.clone(),
            selected_agent_did: projection.selected_agent_did.clone(),
            selected_entity_id: projection.selected_entity_id.clone(),
            draft: projection.draft.clone(),
            draft_origin: projection.draft_origin.clone(),
        },
    );
    projection
}

pub fn select_deployment(operator: &mut OperatorState, peer_id: String, agent_did: String) {
    reduce(
        operator,
        OperatorAction::SelectDeployment { peer_id, agent_did },
    );
}

pub fn select_section(operator: &mut OperatorState, section: OperatorSection) {
    reduce(operator, OperatorAction::SelectSection { section });
}

pub fn select_entity(operator: &mut OperatorState, entity_id: String) {
    reduce(operator, OperatorAction::SelectEntity { entity_id });
}

pub fn start_new_document(operator: &mut OperatorState) {
    let draft = new_draft_for_section(
        operator.selected_section,
        operator.selected_agent_did.as_deref(),
    );
    reduce(operator, OperatorAction::StartNewDocument { draft });
}

pub fn discard_draft(
    operator: &mut OperatorState,
    peer_statuses: &[ClientPeerStatus],
    store: &ClientStore,
) {
    reduce(operator, OperatorAction::DiscardDraft);
    sync_from_snapshot(operator, peer_statuses, store);
}

pub fn apply_draft(
    operator: &mut OperatorState,
    client: Option<&ClientCore>,
    runtime: &Runtime,
) -> Result<()> {
    let client = client.context("client core is offline")?;
    let draft = operator
        .draft
        .as_ref()
        .context("no operator draft is selected")?;
    let entity_id = draft.entity_id().to_string();

    let result = match draft {
        OperatorDraft::Behavior(draft) => {
            runtime.block_on(client.save_behavior(&behavior_row(draft)?))
        }
        OperatorDraft::Backend(draft) => {
            runtime.block_on(client.save_backend(&backend_row(draft)?))
        }
        OperatorDraft::ToolSelection(draft) => {
            runtime.block_on(client.save_tool_selection(&tool_selection_row(draft)?))
        }
        OperatorDraft::InferenceProfile(draft) => {
            runtime.block_on(client.save_inference_profile(&inference_profile_row(draft)?))
        }
        OperatorDraft::ScheduledTask(draft) => {
            runtime.block_on(client.save_scheduled_task(&scheduled_task_row(draft)?))
        }
    };

    match result {
        Ok(()) => {
            reduce(operator, OperatorAction::ApplySucceeded { entity_id });
            let snapshot = client.store().snapshot();
            sync_from_snapshot(operator, &client.peer_statuses(), snapshot.as_ref());
            Ok(())
        }
        Err(error) => {
            let error_text = error.to_string();
            reduce(
                operator,
                OperatorAction::MutationFailed {
                    error: error_text.clone(),
                },
            );
            Err(anyhow!(error_text))
        }
    }
}

pub fn run_selected_task_now(
    operator: &mut OperatorState,
    client: Option<&ClientCore>,
    runtime: &Runtime,
) -> Result<()> {
    let client = client.context("client core is offline")?;
    let draft = operator
        .draft
        .as_ref()
        .context("no operator draft is selected")?;

    let result = match draft {
        OperatorDraft::ScheduledTask(draft) => {
            runtime.block_on(client.run_scheduled_task_now(&scheduled_task_row(draft)?))
        }
        _ => Err(anyhow!("run now is only available for scheduled tasks")),
    };

    match result {
        Ok(()) => {
            reduce(operator, OperatorAction::RunNowSucceeded);
            let snapshot = client.store().snapshot();
            sync_from_snapshot(operator, &client.peer_statuses(), snapshot.as_ref());
            Ok(())
        }
        Err(error) => {
            let error_text = error.to_string();
            reduce(
                operator,
                OperatorAction::MutationFailed {
                    error: error_text.clone(),
                },
            );
            Err(anyhow!(error_text))
        }
    }
}
