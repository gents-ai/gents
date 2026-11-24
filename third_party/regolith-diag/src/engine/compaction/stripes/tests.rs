//! Unit tests for stripe reduction: where the cuts fall and what each
//! stripe keeps.

use super::fixtures::{
    Append, Judge, Partial, Refuse, del, operand, put, remove_a, remove_all, shout, show,
};
use super::*;
use crate::engine::range_tombstone::RangeTombstone;

/// Reduce `group` with the stripes cut at `live`, rendered by [`show`].
fn reduce(
    live: &[u64],
    merge: Option<&dyn MergeOperator>,
    filter: Option<&dyn CompactionFilter>,
    group: Vec<Entry>,
) -> Vec<String> {
    show(&Stripes::new(live, filter, merge, 1).reduce_group(group))
}

#[test]
fn top_is_the_smallest_live_snapshot_at_or_above_the_sequence() {
    let stripes = Stripes::new(&[4, 9], None, None, 0);
    assert_eq!(stripes.top(1), 4);
    assert_eq!(stripes.top(4), 4);
    assert_eq!(stripes.top(5), 9);
    assert_eq!(stripes.top(9), 9);
    assert_eq!(stripes.top(10), u64::MAX);
    assert_eq!(Stripes::new(&[], None, None, 0).top(7), u64::MAX);
}

// Without a live snapshot the key is one stripe. These pin what compaction
// did before stripes existed, one case per rule.

#[test]
fn without_snapshots_only_the_newest_version_survives() {
    let group = vec![put(7, "c"), put(5, "b"), put(3, "a")];
    assert_eq!(reduce(&[], None, None, group), ["7:v:c"]);
}

#[test]
fn without_snapshots_a_newest_deletion_ends_the_key() {
    let group = vec![del(9), put(5, "x"), put(3, "y")];
    assert_eq!(reduce(&[], None, None, group), ["9:d:"]);
}

#[test]
fn without_snapshots_operands_fold_onto_the_value_under_them() {
    for partial in [Partial::Always, Partial::Never] {
        let group = vec![operand(9, "c"), operand(7, "b"), put(5, "a"), put(3, "z")];
        assert_eq!(
            reduce(&[], Some(&Append(partial)), None, group),
            ["9:v:abc"]
        );
    }
}

#[test]
fn without_snapshots_operands_fold_onto_a_deletion_as_an_absent_base() {
    let group = vec![operand(9, "c"), operand(7, "b"), del(5), put(3, "a")];
    assert_eq!(
        reduce(&[], Some(&Append(Partial::Always)), None, group),
        ["9:v:bc"]
    );
}

#[test]
fn without_snapshots_operands_alone_fold_pairwise_when_the_operator_can() {
    let group = || vec![operand(9, "c"), operand(7, "b"), operand(5, "a")];
    assert_eq!(
        reduce(&[], Some(&Append(Partial::Always)), None, group()),
        ["9:m:abc"]
    );
    assert_eq!(
        reduce(&[], Some(&Append(Partial::Never)), None, group()),
        ["9:m:c", "7:m:b", "5:m:a"]
    );
}

#[test]
fn a_partial_merge_that_declines_midway_leaves_the_operands_as_written() {
    let group = vec![operand(9, "c"), operand(7, "b"), operand(5, "a")];
    assert_eq!(
        reduce(&[], Some(&Append(Partial::Once)), None, group),
        ["9:m:c", "7:m:b", "5:m:a"]
    );
}

#[test]
fn a_lone_operand_is_left_as_written() {
    assert_eq!(
        reduce(
            &[],
            Some(&Append(Partial::Always)),
            None,
            vec![operand(9, "a")]
        ),
        ["9:m:a"]
    );
}

#[test]
fn a_declined_full_merge_keeps_the_chain_and_drops_what_it_shadows() {
    let group = vec![operand(9, "c"), operand(7, "b"), put(5, "a"), put(3, "z")];
    assert_eq!(
        reduce(&[], Some(&Refuse), None, group),
        ["9:m:c", "7:m:b", "5:v:a"]
    );
}

#[test]
fn without_a_merge_operator_the_chain_is_kept_down_to_its_value() {
    let group = vec![operand(9, "c"), operand(7, "b"), put(5, "a"), put(3, "z")];
    assert_eq!(reduce(&[], None, None, group), ["9:m:c", "7:m:b", "5:v:a"]);
}

#[test]
fn without_snapshots_the_filter_sees_only_the_surviving_value() {
    let judge = Judge::new(|_| CompactionDecision::Keep);
    let group = vec![put(9, "a"), put(7, "b"), put(5, "c"), del(3)];
    assert_eq!(reduce(&[], None, Some(&judge), group), ["9:v:a"]);
    assert_eq!(judge.seen(), ["a"]);
}

#[test]
fn the_filter_removes_by_writing_a_deletion_at_the_same_sequence() {
    let judge = Judge::new(remove_all);
    let group = vec![put(9, "x"), put(5, "y")];
    assert_eq!(reduce(&[], None, Some(&judge), group), ["9:d:"]);
}

#[test]
fn the_filter_changes_a_value_in_place() {
    let judge = Judge::new(shout);
    assert_eq!(
        reduce(&[], None, Some(&judge), vec![put(9, "a")]),
        ["9:v:A"]
    );
}

#[test]
fn the_filter_never_sees_a_deletion() {
    let judge = Judge::new(remove_all);
    assert_eq!(reduce(&[], None, Some(&judge), vec![del(9)]), ["9:d:"]);
    assert!(judge.seen().is_empty());
}

#[test]
fn a_removed_base_folds_as_an_absent_one_and_a_changed_base_as_the_new_value() {
    let removed = Judge::new(remove_a);
    let group = || vec![operand(9, "b"), put(5, "a")];
    assert_eq!(
        reduce(&[], Some(&Append(Partial::Always)), Some(&removed), group()),
        ["9:v:b"]
    );
    let changed = Judge::new(shout);
    assert_eq!(
        reduce(&[], Some(&Append(Partial::Always)), Some(&changed), group()),
        ["9:v:Ab"]
    );
}

// With snapshots live, nothing crosses a stripe boundary.

#[test]
fn a_snapshot_keeps_operands_apart_from_the_value_beneath_it() {
    let group = || vec![operand(9, "c"), operand(7, "b"), put(5, "a")];
    assert_eq!(
        reduce(&[5], Some(&Append(Partial::Always)), None, group()),
        ["9:m:bc", "5:v:a"]
    );
    assert_eq!(
        reduce(&[5], Some(&Append(Partial::Never)), None, group()),
        ["9:m:c", "7:m:b", "5:v:a"]
    );
}

#[test]
fn a_put_above_a_snapshot_ends_the_fold_there() {
    let group = vec![operand(9, "d"), put(8, "c"), operand(7, "b"), put(3, "a")];
    assert_eq!(
        reduce(&[4], Some(&Append(Partial::Always)), None, group),
        ["9:v:cd", "3:v:a"]
    );
}

#[test]
fn every_stripe_folds_on_its_own() {
    let group = vec![
        operand(9, "e"),
        operand(8, "d"),
        operand(7, "c"),
        operand(6, "b"),
        put(5, "a"),
    ];
    assert_eq!(
        reduce(&[5, 8], Some(&Append(Partial::Always)), None, group),
        ["9:m:e", "8:m:bcd", "5:v:a"]
    );
}

#[test]
fn a_snapshot_at_a_sequence_holds_that_entry_in_its_own_stripe() {
    let group = vec![operand(9, "c"), operand(7, "b"), put(5, "a")];
    assert_eq!(
        reduce(&[7], Some(&Append(Partial::Always)), None, group),
        ["9:m:c", "7:v:ab"]
    );
}

#[test]
fn a_stripe_keeps_its_own_value_even_when_a_newer_stripe_has_one() {
    let group = vec![put(9, "c"), put(7, "b"), put(5, "a"), put(3, "z")];
    assert_eq!(
        reduce(&[5, 7], None, None, group),
        ["9:v:c", "7:v:b", "5:v:a"]
    );
}

#[test]
fn the_filter_judges_the_value_ending_each_stripe() {
    let judge = Judge::new(remove_all);
    let group = vec![put(9, "b"), put(7, "x"), put(5, "a"), put(3, "y")];
    assert_eq!(reduce(&[5], None, Some(&judge), group), ["9:d:", "5:d:"]);
    assert_eq!(judge.seen(), ["b", "a"]);
}

#[test]
fn a_removed_value_does_not_fold_across_into_the_stripe_above() {
    let judge = Judge::new(remove_a);
    let group = vec![operand(9, "c"), put(5, "a")];
    assert_eq!(
        reduce(&[5], Some(&Append(Partial::Always)), Some(&judge), group),
        ["9:m:c", "5:d:"]
    );
}

// Range tombstones shadow an entry only for readers that can see them.

/// A tombstone over `[b, d)` at `seq`.
fn tombstone_at(seq: u64) -> RangeTombstoneSet {
    RangeTombstoneSet::from_vec(vec![RangeTombstone::new(b"b".to_vec(), b"d".to_vec(), seq)])
}

#[test]
fn a_tombstone_shadows_a_covered_entry_older_than_it() {
    let stripes = Stripes::new(&[], None, None, 0);
    let tombstones = tombstone_at(6);
    assert!(stripes.shadowed(&tombstones, b"c", 5));
    assert!(!stripes.shadowed(&tombstones, b"c", 6), "same sequence");
    assert!(!stripes.shadowed(&tombstones, b"c", 7), "newer entry");
    assert!(!stripes.shadowed(&tombstones, b"a", 5), "uncovered key");
    assert!(!stripes.shadowed(&tombstones, b"d", 5), "end is exclusive");
}

#[test]
fn a_snapshot_below_a_tombstone_keeps_the_entries_it_reads() {
    let stripes = Stripes::new(&[5], None, None, 0);
    let tombstones = tombstone_at(6);
    assert!(!stripes.shadowed(&tombstones, b"c", 5));
    assert!(!stripes.shadowed(&tombstones, b"c", 3));
}

#[test]
fn a_snapshot_at_a_tombstone_lets_it_shadow_the_stripe_beneath() {
    let stripes = Stripes::new(&[6], None, None, 0);
    let tombstones = tombstone_at(6);
    assert!(stripes.shadowed(&tombstones, b"c", 5));
    assert!(stripes.shadowed(&tombstones, b"c", 1));
}

#[test]
fn only_the_top_stripe_is_shadowed_by_a_tombstone_above_every_snapshot() {
    let stripes = Stripes::new(&[4], None, None, 0);
    let tombstones = tombstone_at(9);
    assert!(stripes.shadowed(&tombstones, b"c", 7));
    assert!(!stripes.shadowed(&tombstones, b"c", 4));
}

#[test]
fn an_empty_tombstone_set_shadows_nothing() {
    let stripes = Stripes::new(&[], None, None, 0);
    assert!(!stripes.shadowed(&RangeTombstoneSet::default(), b"c", 1));
}
