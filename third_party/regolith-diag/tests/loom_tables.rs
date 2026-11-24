//! Loom models for the open-file slot table and the column-family registry
//! (plan 4.6, Phase 7c1).
//!
//! ```text
//! RUSTFLAGS="--cfg loom" cargo test --release --test loom_tables
//! ```
//!
//! The slot-table models live in `src/env/open_file_limit/loom_model.rs` and
//! drive the production `SlotTable`; the column-family models live in
//! `src/engine/loom_model/families.rs` and are a protocol model of the
//! ordered step. What each proves:
//!
//! - **The slot table**: a descriptor is never closed while a reader uses
//!   it, the table never holds more descriptors than its capacity, a reader
//!   joining from a stale hint reads its own file or misses, and an open that
//!   finds every slot mid-read drains one and is served.
//! - **A parked reopen** (D60): a queue unit's reopen that may not wait,
//!   racing the read that frees the only slot or the open that loads it, is
//!   never forgotten: once the slot is free its queue holds the message that
//!   sends it back to run, and it reads.
//! - **The registry**: a write racing a drop either lands below the drop's
//!   tombstone or is refused, and a family moves unborn, live, dead and never
//!   back.
//!
//! Each is paired with a calibration that writes the one step the wrong way
//! and must fail.

#![cfg(loom)]

use regolith::loom_exports::{families, open_files};

#[test]
fn a_slot_is_never_closed_in_use_and_never_over_the_limit() {
    open_files::a_slot_is_never_closed_in_use_and_never_over_the_limit();
}

#[test]
fn a_starved_open_drains_a_busy_slot() {
    open_files::a_starved_open_drains_a_busy_slot();
}

/// The close is the claimer's write to the slot's descriptor cell; with no
/// claim CAS ordering it after the reader's leave, loom's cell tracking sees
/// that write race the reader's access first.
#[test]
#[should_panic(expected = "Concurrent read and write accesses to `UnsafeCell`")]
fn calibration_an_eviction_that_ignores_readers_closes_a_file_in_use() {
    open_files::calibration_an_eviction_that_ignores_readers_closes_a_file_in_use();
}

#[test]
#[should_panic(expected = "file 1's reader read file 2")]
fn calibration_a_join_without_the_owner_recheck_reads_another_file() {
    open_files::calibration_a_join_without_the_owner_recheck_reads_another_file();
}

#[test]
fn a_parked_reopen_is_woken_by_the_read_that_frees_its_slot() {
    open_files::a_parked_reopen_is_woken_by_the_read_that_frees_its_slot();
}

#[test]
fn a_parked_reopen_is_woken_through_a_load_of_its_slot() {
    open_files::a_parked_reopen_is_woken_through_a_load_of_its_slot();
}

#[test]
#[should_panic(expected = "a freed slot was missed")]
fn calibration_a_park_that_marks_before_it_registers_misses_the_free() {
    open_files::calibration_a_park_that_marks_before_it_registers_misses_the_free();
}

#[test]
#[should_panic(expected = "a freed slot was missed")]
fn calibration_a_publish_that_drops_the_mark_misses_the_free() {
    open_files::calibration_a_publish_that_drops_the_mark_misses_the_free();
}

#[test]
fn a_write_racing_a_drop_is_deleted_or_refused() {
    families::a_write_racing_a_drop_is_deleted_or_refused();
}

#[test]
fn a_family_lives_once_in_order() {
    families::a_family_lives_once_in_order();
}

#[test]
#[should_panic(expected = "a write landed after the drop's tombstone")]
fn calibration_a_fence_outside_the_ordered_step_lands_after_the_drop() {
    families::calibration_a_fence_outside_the_ordered_step_lands_after_the_drop();
}
