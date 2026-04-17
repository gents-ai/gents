use crate::client::{ClientPeerStatus, ClientStore};
use crate::state::{OperatorDraft, OperatorDraftOrigin, OperatorState};
use crate::views::chat::{build_deployment_entries, DeploymentEntry};
use crate::views::operator::drafts::{
    draft_for_selection, draft_matches_selection, entity_summaries,
};

#[derive(Debug, Clone, PartialEq)]
pub struct OperatorProjection {
    pub selected_peer_id: Option<String>,
    pub selected_agent_did: Option<String>,
    pub selected_entity_id: Option<String>,
    pub draft: Option<OperatorDraft>,
    pub draft_origin: Option<OperatorDraftOrigin>,
}

pub fn project_operator(
    state: &OperatorState,
    peer_statuses: &[ClientPeerStatus],
    store: &ClientStore,
) -> OperatorProjection {
    let deployments = build_deployment_entries(peer_statuses, store);
    let selected_peer_id = resolve_peer_id(state, &deployments);
    let selected_agent_did =
        resolve_agent_did(state, store, &deployments, selected_peer_id.as_deref());

    if preserves_new_document_draft(state) {
        return OperatorProjection {
            selected_peer_id,
            selected_agent_did,
            selected_entity_id: state.selected_entity_id.clone(),
            draft: state.draft.clone(),
            draft_origin: state.draft_origin.clone(),
        };
    }

    let entries = entity_summaries(store, state.selected_section, selected_agent_did.as_deref());
    let selected_entity_id = state
        .selected_entity_id
        .as_deref()
        .filter(|entity_id| entries.iter().any(|entry| entry.id == *entity_id))
        .map(ToOwned::to_owned)
        .or_else(|| entries.first().map(|entry| entry.id.clone()));

    if draft_matches_selection(
        &state.draft,
        state.draft_origin.as_ref(),
        state.selected_section,
        selected_entity_id.as_deref(),
    ) {
        return OperatorProjection {
            selected_peer_id,
            selected_agent_did,
            selected_entity_id,
            draft: state.draft.clone(),
            draft_origin: state.draft_origin.clone(),
        };
    }

    let draft = selected_entity_id.as_deref().and_then(|entity_id| {
        draft_for_selection(
            store,
            state.selected_section,
            selected_agent_did.as_deref(),
            entity_id,
        )
    });
    let draft_origin = selected_entity_id
        .as_ref()
        .and_then(|entity_id| draft.as_ref().map(|_| entity_id.clone()))
        .map(OperatorDraftOrigin::ExistingEntity);

    OperatorProjection {
        selected_peer_id,
        selected_agent_did,
        selected_entity_id,
        draft,
        draft_origin,
    }
}

fn preserves_new_document_draft(state: &OperatorState) -> bool {
    matches!(state.draft_origin, Some(OperatorDraftOrigin::NewDocument))
        && state
            .draft
            .as_ref()
            .is_some_and(|draft| draft.section() == state.selected_section)
}

fn resolve_peer_id(state: &OperatorState, deployments: &[DeploymentEntry]) -> Option<String> {
    state
        .selected_peer_id
        .as_deref()
        .filter(|peer_id| deployments.iter().any(|entry| entry.peer_id == *peer_id))
        .map(ToOwned::to_owned)
        .or_else(|| deployments.first().map(|entry| entry.peer_id.clone()))
}

fn resolve_agent_did(
    state: &OperatorState,
    store: &ClientStore,
    deployments: &[DeploymentEntry],
    selected_peer_id: Option<&str>,
) -> Option<String> {
    state
        .selected_agent_did
        .as_deref()
        .filter(|agent_did| {
            deployments
                .iter()
                .any(|entry| entry.agent_did == *agent_did)
                || store
                    .agent_principals
                    .iter()
                    .any(|row| row.agent_did == *agent_did)
        })
        .map(ToOwned::to_owned)
        .or_else(|| {
            deployments
                .iter()
                .find(|entry| Some(entry.peer_id.as_str()) == selected_peer_id)
                .map(|entry| entry.agent_did.clone())
        })
        .or_else(|| deployments.first().map(|entry| entry.agent_did.clone()))
        .or_else(|| {
            store
                .agent_principals
                .first()
                .map(|row| row.agent_did.clone())
        })
}

#[cfg(test)]
mod tests {
    use super::project_operator;
    use crate::client::ClientStore;
    use crate::state::{
        BackendDraft, OperatorDraft, OperatorDraftOrigin, OperatorSection, OperatorState,
    };

    #[test]
    fn projection_preserves_new_backend_draft() {
        let state = OperatorState {
            selected_section: OperatorSection::Backends,
            draft_origin: Some(OperatorDraftOrigin::NewDocument),
            draft: Some(OperatorDraft::Backend(BackendDraft {
                backend_id: "backend-new".to_string(),
                name: String::new(),
                provider_kind: String::new(),
                endpoint: String::new(),
                api_key: String::new(),
                api_key_env_var: String::new(),
                max_concurrent: String::new(),
                max_queue_depth: String::new(),
                enabled: true,
                models: String::new(),
                probe_status: String::new(),
            })),
            ..OperatorState::default()
        };

        let projection = project_operator(&state, &[], &ClientStore::default());
        assert_eq!(projection.selected_entity_id, None);
        assert_eq!(
            projection.draft_origin,
            Some(OperatorDraftOrigin::NewDocument)
        );
        assert!(matches!(projection.draft, Some(OperatorDraft::Backend(_))));
    }
}
