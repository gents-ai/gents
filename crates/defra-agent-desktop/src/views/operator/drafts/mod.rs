mod hydrate;
mod meta;
mod selection;
mod summaries;

use crate::client::ClientStore;
use crate::state::{OperatorDraft, OperatorSection};

use super::EntitySummary;

pub(super) fn draft_for_selection(
    store: &ClientStore,
    section: OperatorSection,
    selected_agent_did: Option<&str>,
    entity_id: &str,
) -> Option<OperatorDraft> {
    hydrate::draft_for_selection(store, section, selected_agent_did, entity_id)
}

pub(super) fn section_meta(
    store: &ClientStore,
    section: OperatorSection,
    selected_agent_did: Option<&str>,
) -> (&'static str, String) {
    meta::section_meta(store, section, selected_agent_did)
}

pub(super) fn draft_matches_selection(
    draft: &Option<OperatorDraft>,
    draft_source_entity_id: Option<&str>,
    section: OperatorSection,
    selected_entity_id: Option<&str>,
) -> bool {
    selection::draft_matches_selection(draft, draft_source_entity_id, section, selected_entity_id)
}

pub(super) fn entity_summaries(
    store: &ClientStore,
    section: OperatorSection,
    selected_agent_did: Option<&str>,
) -> Vec<EntitySummary> {
    summaries::entity_summaries(store, section, selected_agent_did)
}

pub(super) fn filter_entity_summaries(
    entries: Vec<EntitySummary>,
    filter: &str,
) -> Vec<EntitySummary> {
    summaries::filter_entity_summaries(entries, filter)
}

fn backend_ids_for_agent<'a>(
    store: &'a ClientStore,
    selected_agent_did: Option<&str>,
) -> Vec<&'a str> {
    store
        .behaviors
        .iter()
        .filter(|row| row.agent_did.as_deref() == selected_agent_did)
        .filter_map(|row| row.backend_id.as_deref())
        .collect()
}

fn inference_profile_ids_for_agent<'a>(
    store: &'a ClientStore,
    selected_agent_did: Option<&str>,
) -> Vec<&'a str> {
    store
        .behaviors
        .iter()
        .filter(|row| row.agent_did.as_deref() == selected_agent_did)
        .filter_map(|row| row.inference_profile_id.as_deref())
        .collect()
}
