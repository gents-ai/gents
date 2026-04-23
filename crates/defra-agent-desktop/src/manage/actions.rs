use crate::state::{ManageDraft, ManageDraftOrigin, ManageSection, ManageState};

#[derive(Debug, Clone, PartialEq)]
pub enum ManageAction {
    SelectDeployment {
        peer_id: String,
        agent_did: String,
    },
    SelectSection {
        section: ManageSection,
    },
    SelectEntity {
        entity_id: String,
    },
    StartNewDocument {
        draft: Option<ManageDraft>,
    },
    DiscardDraft,
    ApplySucceeded {
        entity_id: String,
    },
    RunNowSucceeded,
    MutationFailed {
        error: String,
    },
    SnapshotApplied {
        selected_peer_id: Option<String>,
        selected_agent_did: Option<String>,
        selected_entity_id: Option<String>,
        draft: Option<ManageDraft>,
        draft_origin: Option<ManageDraftOrigin>,
    },
}

pub fn reduce(state: &mut ManageState, action: ManageAction) {
    match action {
        ManageAction::SelectDeployment { peer_id, agent_did } => {
            state.selected_peer_id = Some(peer_id);
            state.selected_agent_did = Some(agent_did);
            state.selected_entity_id = None;
            state.draft = None;
            state.draft_origin = None;
            state.last_apply_error = None;
            state.fire_task_draft = None;
        }
        ManageAction::SelectSection { section } => {
            state.selected_section = section;
            state.selected_entity_id = None;
            state.draft = None;
            state.draft_origin = None;
            state.last_apply_error = None;
            state.fire_task_draft = None;
        }
        ManageAction::SelectEntity { entity_id } => {
            state.selected_entity_id = Some(entity_id);
            state.draft = None;
            state.draft_origin = None;
            state.last_apply_error = None;
            state.fire_task_draft = None;
        }
        ManageAction::StartNewDocument { draft } => {
            state.selected_entity_id = None;
            state.draft_origin = draft.as_ref().map(|_| ManageDraftOrigin::NewDocument);
            state.draft = draft;
            state.last_apply_error = None;
        }
        ManageAction::DiscardDraft => {
            state.draft = None;
            state.draft_origin = None;
            state.last_apply_error = None;
        }
        ManageAction::ApplySucceeded { entity_id } => {
            state.selected_entity_id = Some(entity_id);
            state.draft = None;
            state.draft_origin = None;
            state.last_apply_error = None;
        }
        ManageAction::RunNowSucceeded => {
            state.draft = None;
            state.draft_origin = None;
            state.last_apply_error = None;
        }
        ManageAction::MutationFailed { error } => {
            state.last_apply_error = Some(error);
        }
        ManageAction::SnapshotApplied {
            selected_peer_id,
            selected_agent_did,
            selected_entity_id,
            draft,
            draft_origin,
        } => {
            state.selected_peer_id = selected_peer_id;
            state.selected_agent_did = selected_agent_did;
            state.selected_entity_id = selected_entity_id;
            state.draft = draft;
            state.draft_origin = draft_origin;
        }
    }
}
