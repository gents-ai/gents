use crate::manage;
use crate::views::manage::EntitySummary;

pub(super) fn filter_entity_summaries(
    entries: Vec<EntitySummary>,
    filter: &str,
) -> Vec<EntitySummary> {
    manage::filter_entity_summaries(entries, filter)
}
