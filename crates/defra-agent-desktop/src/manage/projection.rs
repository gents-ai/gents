use crate::client::{ClientPeerStatus, ClientStore};
use crate::manage::{
    build_deployment_entries, draft_for_selection, draft_matches_selection, entity_summaries,
    DeploymentEntry,
};
use crate::state::{ManageDraft, ManageDraftOrigin, ManageState};

#[derive(Debug, Clone, PartialEq)]
pub struct ManageProjection {
    pub selected_peer_id: Option<String>,
    pub selected_agent_did: Option<String>,
    pub selected_entity_id: Option<String>,
    pub draft: Option<ManageDraft>,
    pub draft_origin: Option<ManageDraftOrigin>,
}

pub fn project_manage(
    state: &ManageState,
    peer_statuses: &[ClientPeerStatus],
    store: &ClientStore,
) -> ManageProjection {
    let deployments = build_deployment_entries(peer_statuses, store);
    let selected_peer_id = resolve_peer_id(state, &deployments);
    let selected_agent_did = resolve_agent_did(state, store, &deployments);

    if preserves_new_document_draft(state) {
        return ManageProjection {
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
        .map(ToOwned::to_owned);

    if draft_matches_selection(
        &state.draft,
        state.draft_origin.as_ref(),
        state.selected_section,
        selected_entity_id.as_deref(),
    ) {
        return ManageProjection {
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
        .map(ManageDraftOrigin::ExistingEntity);

    ManageProjection {
        selected_peer_id,
        selected_agent_did,
        selected_entity_id,
        draft,
        draft_origin,
    }
}

fn preserves_new_document_draft(state: &ManageState) -> bool {
    matches!(state.draft_origin, Some(ManageDraftOrigin::NewDocument))
        && state
            .draft
            .as_ref()
            .is_some_and(|draft| draft.section() == state.selected_section)
}

fn resolve_peer_id(state: &ManageState, deployments: &[DeploymentEntry]) -> Option<String> {
    state
        .selected_peer_id
        .as_deref()
        .filter(|peer_id| deployments.iter().any(|entry| entry.peer_id == *peer_id))
        .map(ToOwned::to_owned)
}

fn resolve_agent_did(
    state: &ManageState,
    store: &ClientStore,
    deployments: &[DeploymentEntry],
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
}

#[cfg(test)]
mod tests {
    use super::project_manage;
    use crate::client::ClientStore;
    use crate::state::{
        BackendDraft, ManageDraft, ManageDraftOrigin, ManageSection, ManageState,
    };

    #[test]
    fn projection_preserves_new_backend_draft() {
        let state = ManageState {
            selected_section: ManageSection::Backends,
            draft_origin: Some(ManageDraftOrigin::NewDocument),
            draft: Some(ManageDraft::Backend(BackendDraft {
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
            ..ManageState::default()
        };

        let projection = project_manage(&state, &[], &ClientStore::default());
        assert_eq!(projection.selected_entity_id, None);
        assert_eq!(
            projection.draft_origin,
            Some(ManageDraftOrigin::NewDocument)
        );
        assert!(matches!(projection.draft, Some(ManageDraft::Backend(_))));
    }

    #[test]
    fn projection_clears_invalid_existing_entity_selection() {
        let store = ClientStore::default();
        let state = ManageState {
            selected_section: ManageSection::Backends,
            selected_entity_id: Some("missing-backend".to_string()),
            draft: Some(ManageDraft::Backend(BackendDraft {
                backend_id: "missing-backend".to_string(),
                name: "stale".to_string(),
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
            ..ManageState::default()
        };

        let projection = project_manage(&state, &[], &store);
        assert_eq!(projection.selected_entity_id, None);
        assert_eq!(projection.draft, None);
        assert_eq!(projection.draft_origin, None);
    }

    #[test]
    fn projection_keeps_valid_agent_without_inventing_peer_or_entity() {
        let store = ClientStore::from_rows(crate::client::ClientStoreRows {
            agent_principals: vec![defra_agent_protocol::row::AgentPrincipalRow {
                agent_did: "did:defra:amy".to_string(),
                display_name: Some("Amy".to_string()),
                default_behavior_id: Some("amy-default".to_string()),
                enabled: Some(true),
                created_at: None,
                created_by: None,
            }],
            ..crate::client::ClientStoreRows::default()
        });
        let state = ManageState {
            selected_agent_did: Some("did:defra:amy".to_string()),
            selected_peer_id: Some("peer-missing".to_string()),
            selected_entity_id: Some("behavior-missing".to_string()),
            ..ManageState::default()
        };

        let projection = project_manage(&state, &[], &store);
        assert_eq!(projection.selected_peer_id, None);
        assert_eq!(
            projection.selected_agent_did.as_deref(),
            Some("did:defra:amy")
        );
        assert_eq!(projection.selected_entity_id, None);
    }
}
