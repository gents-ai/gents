//! Loom model checks for the snapshot registry's per-thread slots.
//!
//! The models live in `src/engine/loom_model/snapshots.rs`, next to the
//! crate-private registry they drive. This target runs them:
//!
//! ```text
//! RUSTFLAGS="--cfg loom" cargo test --release --test loom_snapshots
//! ```
//!
//! Without `--cfg loom` the whole file compiles away.
//!
//! Four calibrations must fail, or the passes mean nothing: a registration
//! that skips its confirm, a confirm with no fence after its announce, and
//! a scan with no sample fence each let a scan miss a live snapshot; and a
//! drain waiter that does not mark the chunks it read is never told of the
//! release it waits for.

#![cfg(loom)]

use regolith::loom_exports::snapshots;

#[test]
fn concurrent_pins_in_one_slot_keep_their_own_counts() {
    snapshots::concurrent_pins_in_one_slot_keep_their_own_counts();
}

#[test]
fn racing_claims_grow_the_chain_by_one_chunk() {
    snapshots::racing_claims_grow_the_chain_by_one_chunk();
}

#[test]
fn the_index_pool_never_hands_one_number_to_two_owners() {
    snapshots::the_index_pool_never_hands_one_number_to_two_owners();
}

#[test]
fn a_moved_release_frees_exactly_its_entry_beside_a_join() {
    snapshots::a_moved_release_frees_exactly_its_entry_beside_a_join();
}

#[test]
fn a_scan_never_misses_a_confirmed_snapshot() {
    snapshots::a_scan_never_misses_a_confirmed_snapshot();
}

#[test]
#[should_panic(expected = "the scan missed a confirmed snapshot")]
fn a_snapshot_announced_without_its_confirm_is_missed() {
    snapshots::a_snapshot_announced_without_its_confirm_is_missed();
}

#[test]
#[should_panic(expected = "the scan missed a confirmed snapshot")]
fn a_scan_without_its_sample_fence_misses_a_snapshot() {
    snapshots::a_scan_without_its_sample_fence_misses_a_snapshot();
}

#[test]
#[should_panic(expected = "the scan missed a confirmed snapshot")]
fn a_confirm_without_its_fence_is_missed() {
    snapshots::a_confirm_without_its_fence_is_missed();
}

#[test]
fn a_drain_waiter_is_always_woken() {
    snapshots::a_drain_waiter_is_always_woken();
}

#[test]
#[should_panic(expected = "never told it went free")]
fn a_waiter_that_does_not_mark_the_chunk_is_never_woken() {
    snapshots::a_waiter_that_does_not_mark_the_chunk_is_never_woken();
}
