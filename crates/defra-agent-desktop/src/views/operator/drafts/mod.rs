mod hydrate;
mod meta;
mod selection;
mod summaries;

use crate::client::ClientStore;
use crate::state::{OperatorDraft, OperatorDraftOrigin, OperatorSection};

use super::EntitySummary;

pub(crate) fn draft_for_selection(
    store: &ClientStore,
    section: OperatorSection,
    selected_agent_did: Option<&str>,
    entity_id: &str,
) -> Option<OperatorDraft> {
    hydrate::draft_for_selection(store, section, selected_agent_did, entity_id)
}

pub(crate) fn new_draft_for_section(
    section: OperatorSection,
    selected_agent_did: Option<&str>,
) -> Option<OperatorDraft> {
    hydrate::new_draft_for_section(section, selected_agent_did)
}

pub(super) fn section_meta(
    store: &ClientStore,
    section: OperatorSection,
    selected_agent_did: Option<&str>,
) -> (&'static str, String) {
    meta::section_meta(store, section, selected_agent_did)
}

pub(crate) fn draft_matches_selection(
    draft: &Option<OperatorDraft>,
    draft_origin: Option<&OperatorDraftOrigin>,
    section: OperatorSection,
    selected_entity_id: Option<&str>,
) -> bool {
    selection::draft_matches_selection(draft, draft_origin, section, selected_entity_id)
}

pub(crate) fn entity_summaries(
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
