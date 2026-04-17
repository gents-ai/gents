use crate::state::{OperatorDraft, OperatorDraftOrigin, OperatorSection, OperatorState};

#[derive(Debug, Clone, PartialEq)]
pub enum OperatorAction {
    SelectDeployment {
        peer_id: String,
        agent_did: String,
    },
    SelectSection {
        section: OperatorSection,
    },
    SelectEntity {
        entity_id: String,
    },
    StartNewDocument {
        draft: Option<OperatorDraft>,
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
        draft: Option<OperatorDraft>,
        draft_origin: Option<OperatorDraftOrigin>,
    },
}

pub fn reduce(state: &mut OperatorState, action: OperatorAction) {
    match action {
        OperatorAction::SelectDeployment { peer_id, agent_did } => {
            state.selected_peer_id = Some(peer_id);
            state.selected_agent_did = Some(agent_did);
            state.selected_entity_id = None;
            state.entity_filter.clear();
            state.draft = None;
            state.draft_origin = None;
            state.last_apply_error = None;
        }
        OperatorAction::SelectSection { section } => {
            state.selected_section = section;
            state.selected_entity_id = None;
            state.entity_filter.clear();
            state.draft = None;
            state.draft_origin = None;
            state.last_apply_error = None;
        }
        OperatorAction::SelectEntity { entity_id } => {
            state.selected_entity_id = Some(entity_id);
            state.draft = None;
            state.draft_origin = None;
            state.last_apply_error = None;
        }
        OperatorAction::StartNewDocument { draft } => {
            state.selected_entity_id = None;
            state.entity_filter.clear();
            state.draft_origin = draft.as_ref().map(|_| OperatorDraftOrigin::NewDocument);
            state.draft = draft;
            state.last_apply_error = None;
        }
        OperatorAction::DiscardDraft => {
            state.draft = None;
            state.draft_origin = None;
            state.last_apply_error = None;
        }
        OperatorAction::ApplySucceeded { entity_id } => {
            state.selected_entity_id = Some(entity_id);
            state.draft = None;
            state.draft_origin = None;
            state.last_apply_error = None;
        }
        OperatorAction::RunNowSucceeded => {
            state.draft = None;
            state.draft_origin = None;
            state.last_apply_error = None;
        }
        OperatorAction::MutationFailed { error } => {
            state.last_apply_error = Some(error);
        }
        OperatorAction::SnapshotApplied {
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
