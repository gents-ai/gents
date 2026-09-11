use gents::{Collection, DESIRED_STATE_APPLY_ORDER};
use std::collections::BTreeSet;

#[test]
fn apply_desired_state_changes_order_contains_each_collection_once() {
    let found = DESIRED_STATE_APPLY_ORDER.to_vec();
    let actual = found.iter().copied().collect::<BTreeSet<_>>();
    let expected = Collection::ALL.into_iter().collect::<BTreeSet<_>>();

    assert_eq!(actual, expected);
    assert_eq!(found.len(), Collection::ALL.len());
}
