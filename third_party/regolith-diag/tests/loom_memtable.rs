//! Loom model checks for the arena memtable and the engine's handoffs.
//!
//! The models themselves live in `src/engine/loom_model/`, next to the
//! code they drive, because everything they touch is crate-private. This
//! target is how they are run:
//!
//! ```text
//! RUSTFLAGS="--cfg loom" cargo test --release --test loom_memtable
//! ```
//!
//! Without `--cfg loom` the whole file compiles away, so an ordinary
//! `cargo test --workspace` neither builds loom nor runs these.
//!
//! Each positive model is paired with a calibration model that
//! deliberately gets the ordering wrong and must fail. A model checker
//! whose search never reaches the bad interleaving reports success for
//! the same reason a broken one does, so the calibrations are what make
//! the passes mean something. They are `#[should_panic]`, and the
//! expected message is the assertion the wrong ordering trips.
//!
//! The memtable takes writes from any thread (the commit pipeline applies
//! a slot from whichever thread reaches it), so the arena and the skip list
//! are modelled with two writers at once: two allocations get ranges of
//! their own, two inserts both land, and one entry inserted twice is held
//! once. Their calibrations rebuild the one compare-and-swap each rests on
//! as a load and a store.

#![cfg(loom)]

use regolith::loom_exports::{arena, handoff, skiplist, slice, version};

#[test]
fn insert_publishes_a_whole_node_to_a_concurrent_reader() {
    skiplist::insert_publishes_a_whole_node_to_a_concurrent_reader();
}

#[test]
fn a_seeded_key_stays_findable_while_a_writer_inserts_before_it() {
    skiplist::a_seeded_key_stays_findable_while_a_writer_inserts_before_it();
}

#[test]
#[should_panic(expected = "the two-step seek lost `c`")]
fn the_two_step_seek_loses_a_key_that_is_present() {
    skiplist::the_two_step_seek_loses_a_key_that_is_present();
}

#[test]
fn two_writers_and_a_reader_share_one_key() {
    skiplist::two_writers_and_a_reader_share_one_key();
}

#[test]
fn a_hinted_run_publishes_whole_nodes_to_a_concurrent_reader() {
    skiplist::a_hinted_run_publishes_whole_nodes_to_a_concurrent_reader();
}

#[test]
fn two_writers_of_one_entry_hold_it_once() {
    skiplist::two_writers_of_one_entry_hold_it_once();
}

#[test]
#[should_panic(expected = "an insert was lost")]
fn a_link_stored_without_a_compare_and_swap_loses_an_insert() {
    skiplist::a_link_stored_without_a_compare_and_swap_loses_an_insert();
}

#[test]
fn two_allocations_at_once_get_ranges_of_their_own() {
    arena::two_allocations_at_once_get_ranges_of_their_own();
}

#[test]
#[should_panic(expected = "two allocations share bytes")]
fn a_cursor_moved_by_a_load_and_a_store_hands_one_range_out_twice() {
    arena::a_cursor_moved_by_a_load_and_a_store_hands_one_range_out_twice();
}

#[test]
fn a_slice_outlives_every_memtable_handle() {
    slice::a_slice_outlives_every_memtable_handle();
}

#[test]
fn a_slice_survives_the_arena_growing_a_new_chunk() {
    slice::a_slice_survives_the_arena_growing_a_new_chunk();
}

#[test]
fn a_flush_never_hides_a_key() {
    handoff::a_flush_never_hides_a_key();
}

#[test]
#[should_panic(expected = "the key went missing")]
fn a_flush_that_retires_before_it_installs_hides_a_key() {
    handoff::a_flush_that_retires_before_it_installs_hides_a_key();
}

#[test]
fn the_read_horizon_never_outruns_the_memtable() {
    handoff::the_read_horizon_never_outruns_the_memtable();
}

#[test]
#[should_panic(expected = "a relaxed horizon advertised a sequence the reader cannot see")]
fn a_relaxed_read_horizon_outruns_the_memtable() {
    handoff::a_relaxed_read_horizon_outruns_the_memtable();
}

#[test]
fn a_reader_pins_one_version_across_a_compaction() {
    version::a_reader_pins_one_version_across_a_compaction();
}

#[test]
#[should_panic(expected = "the split apply hid `k`")]
fn a_split_compaction_apply_hides_a_key() {
    version::a_split_compaction_apply_hides_a_key();
}

#[test]
fn a_flush_and_a_compaction_cannot_lose_each_other() {
    version::a_flush_and_a_compaction_cannot_lose_each_other();
}

#[test]
#[should_panic(expected = "an unserialized version swap lost an edit")]
fn an_unserialized_version_swap_loses_an_edit() {
    version::an_unserialized_version_swap_loses_an_edit();
}
