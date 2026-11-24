//! Loom model checks for a memtable's range-tombstone log.
//!
//! The models live in `src/engine/loom_model/tombstones.rs`, next to the
//! crate-private log they drive; this target runs them:
//!
//! ```text
//! RUSTFLAGS="--cfg loom" cargo test --release --test loom_range_tombstones
//! ```
//!
//! Without `--cfg loom` the file compiles away. Each calibration plants a
//! defect the log rules out and must fail: a length published before its
//! tombstone, and a position taken by a load and a store.

#![cfg(loom)]

use regolith::loom_exports::tombstones;

#[test]
fn a_reader_sees_a_prefix_of_whole_appends() {
    tombstones::a_reader_sees_a_prefix_of_whole_appends();
}

#[test]
fn a_published_range_delete_is_seen_by_every_later_read() {
    tombstones::a_published_range_delete_is_seen_by_every_later_read();
}

#[test]
#[should_panic]
fn a_length_published_before_its_tombstone_is_caught() {
    tombstones::a_length_published_before_its_tombstone_is_caught();
}

#[test]
fn two_appends_at_once_are_both_listed() {
    tombstones::two_appends_at_once_are_both_listed();
}

#[test]
fn one_tombstone_appended_twice_is_listed_once() {
    tombstones::one_tombstone_appended_twice_is_listed_once();
}

#[test]
fn a_batch_beside_an_append_loses_neither() {
    tombstones::a_batch_beside_an_append_loses_neither();
}

#[test]
#[should_panic(expected = "two appends were handed one position")]
fn a_position_taken_by_a_load_and_a_store_is_handed_out_twice() {
    tombstones::a_position_taken_by_a_load_and_a_store_is_handed_out_twice();
}
