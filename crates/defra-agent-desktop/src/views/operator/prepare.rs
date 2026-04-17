use crate::client::{ClientCore, ClientStore};
use crate::state::ShellState;
use crate::views::chat::build_deployment_entries;

use super::drafts::{draft_for_selection, draft_matches_selection, entity_summaries};

pub(super) fn prepare_state(
    state: &mut ShellState,
    client: Option<&ClientCore>,
    store: Option<&ClientStore>,
) {
    let Some(store) = store else {
        return;
    };

    let peer_statuses = client.map(ClientCore::peer_statuses).unwrap_or_default();
    let deployments = build_deployment_entries(&peer_statuses, store);

    if state
        .operator
        .selected_peer_id
        .as_deref()
        .is_none_or(|peer_id| !deployments.iter().any(|entry| entry.peer_id == peer_id))
    {
        state.operator.selected_peer_id = deployments.first().map(|entry| entry.peer_id.clone());
    }

    if state
        .operator
        .selected_agent_did
        .as_deref()
        .is_none_or(|agent_did| {
            !deployments.iter().any(|entry| entry.agent_did == agent_did)
                && !store
                    .agent_principals
                    .iter()
                    .any(|row| row.agent_did == agent_did)
        })
    {
        state.operator.selected_agent_did = deployments
            .iter()
            .find(|entry| {
                Some(entry.peer_id.as_str()) == state.operator.selected_peer_id.as_deref()
            })
            .map(|entry| entry.agent_did.clone())
            .or_else(|| deployments.first().map(|entry| entry.agent_did.clone()))
            .or_else(|| {
                store
                    .agent_principals
                    .first()
                    .map(|row| row.agent_did.clone())
            });
    }

    let entries = entity_summaries(
        store,
        state.operator.selected_section,
        state.operator.selected_agent_did.as_deref(),
    );
    if state
        .operator
        .selected_entity_id
        .as_deref()
        .is_none_or(|entity_id| !entries.iter().any(|entry| entry.id == entity_id))
    {
        state.operator.selected_entity_id = entries.first().map(|entry| entry.id.clone());
    }

    let selected_entity_id = state.operator.selected_entity_id.clone();
    if !draft_matches_selection(
        &state.operator.draft,
        state.operator.draft_source_entity_id.as_deref(),
        state.operator.selected_section,
        selected_entity_id.as_deref(),
    ) {
        refresh_selected_draft(state, store, selected_entity_id.as_deref());
    }
}

pub(super) fn refresh_selected_draft(
    state: &mut ShellState,
    store: &ClientStore,
    selected_entity_id: Option<&str>,
) {
    state.operator.draft = selected_entity_id.and_then(|entity_id| {
        draft_for_selection(
            store,
            state.operator.selected_section,
            state.operator.selected_agent_did.as_deref(),
            entity_id,
        )
    });
    state.operator.draft_source_entity_id = state
        .operator
        .draft
        .as_ref()
        .and(selected_entity_id.map(ToOwned::to_owned));
    state.operator.last_apply_error = None;
}
