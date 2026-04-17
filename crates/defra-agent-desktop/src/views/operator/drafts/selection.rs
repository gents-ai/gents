use crate::state::{OperatorDraft, OperatorDraftOrigin, OperatorSection};

pub(super) fn draft_matches_selection(
    draft: &Option<OperatorDraft>,
    draft_origin: Option<&OperatorDraftOrigin>,
    section: OperatorSection,
    selected_entity_id: Option<&str>,
) -> bool {
    match (draft, draft_origin, selected_entity_id) {
        (
            Some(draft),
            Some(OperatorDraftOrigin::ExistingEntity(source_entity_id)),
            Some(entity_id),
        ) => draft.section() == section && source_entity_id == entity_id,
        (Some(draft), Some(OperatorDraftOrigin::NewDocument), None) => draft.section() == section,
        (None, _, None) => true,
        _ => false,
    }
}
