use crate::operator;
use crate::views::operator::EntitySummary;

pub(super) fn filter_entity_summaries(
    entries: Vec<EntitySummary>,
    filter: &str,
) -> Vec<EntitySummary> {
    operator::filter_entity_summaries(entries, filter)
}
