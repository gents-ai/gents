use crate::state::{OperatorDraft, OperatorSection};

pub(super) fn draft_matches_selection(
    draft: &Option<OperatorDraft>,
    draft_source_entity_id: Option<&str>,
    section: OperatorSection,
    selected_entity_id: Option<&str>,
) -> bool {
    match (draft, draft_source_entity_id, selected_entity_id) {
        (Some(draft), Some(source_entity_id), Some(entity_id)) => {
            draft.section() == section && source_entity_id == entity_id
        }
        (None, _, None) => true,
        _ => false,
    }
}
