//! Loom models of the lock-free commit pipeline (plan 4.7) and of the step
//! the version store's sections run in.
//!
//! ```text
//! RUSTFLAGS="--cfg loom" cargo test --release --test loom_pipeline
//! ```
//!
//! The models live in `src/engine/commit/loom_model.rs`,
//! `src/engine/commit/loom_write_unit.rs`,
//! `src/engine/commit/loom_turn_wait.rs`,
//! `src/engine/loom_model/step.rs` and `src/engine/loom_model/park.rs`,
//! next to the crate-private types they drive. What each proves:
//!
//! - **Reserve and void**: an owner installs the index it reserved while a
//!   helper voids it; exactly one wins, and the descriptor is dropped once.
//! - **A cell's next lap**: a slot is retired while the index one lap later
//!   is installed into its cell; the new descriptor is never taken for the
//!   old one.
//! - **The pin**: a descriptor, or the pipeline's state, loaded while it is
//!   retired or replaced is alive, and one loaded while it is installed is
//!   the one installed.
//! - **The drop at the swap**: a replaced tip is dropped at once when no
//!   load is counted in, and waits on the list otherwise; no value is
//!   dropped while loaded or dropped twice.
//! - **Decide with helping**: two threads decide the same two slots at
//!   once; each slot gets one decision and the sequences are dense.
//! - **The watermark claim**: two commits need the log durable; one claims
//!   the round and syncs, the other parks and is woken, and no two syncs
//!   overlap.
//! - **Publish**: two helpers publish one slot; whoever sees it published
//!   finds the read horizon at its sequence.
//! - **A rollback meeting a decision under way**: the decision computed
//!   from the discarded state never becomes the pipeline's state.
//! - **The version step**: two sections handed over at once never overlap,
//!   both run, and a caller parked on its section is woken.
//! - **Parking** (a queue's poll never waits for another thread): a thread
//!   blocked on a job its holder parks runs it; a job parked for the step
//!   is woken when the step is given up; a rollback parked for a write in
//!   flight cuts the log only after the write left; a sync parked for a
//!   copy of a record in flight begins only after the copy left, and a copy
//!   counted in after the sync turned the gate writes nothing; and a queue
//!   the wake of a parked job had off the list when the job landed is told
//!   once.
//!
//! - **The write unit** (E45, L58): one thread at a time holds the unit; a
//!   writer parked on its record is woken when the holder's run writes it,
//!   and when the holder lets the unit go with the record still waiting; a
//!   run looks at its record again once counted in at the gate, so it never
//!   lands under a sync; a `commit_nowait` commit left on its queue is
//!   told when the unit is let go; one thread at a time encodes a record,
//!   the owner or a thread that needs it; and a writer parked on a record
//!   being encoded is woken when it is filled; the fast path of a writer
//!   racing a claimer writes only whole records; and two writers behind a
//!   holder are never left asleep, on the production note, park, release,
//!   tell and wake (the review's two lost wakes are its calibrations).
//! - **A call far from its turn** (E45 step 2, `turn_wait.rs`): a blocking
//!   call parked on its own slot is named by the pass that brings its turn
//!   near and told by the next finished commit, and is woken when its slot
//!   lands, whoever published it.
//!
//! Every model is bounded by a small scenario (a ring of two cells, two
//! threads or two and a reader) and an explicit preemption bound given in
//! the model's own code, never by a time limit. Each is paired with a
//! calibration that writes one step the wrong way and must fail; without
//! them a search that never reached the bad interleaving would pass for the
//! same reason a broken one does.

#![cfg(loom)]

use regolith::loom_exports::{park, pipeline, step, turn_wait, write_unit};

#[test]
fn an_index_is_installed_or_voided_never_both() {
    pipeline::an_index_is_installed_or_voided_never_both();
}

#[test]
#[should_panic(expected = "an index was installed and voided")]
fn a_void_that_is_not_a_compare_and_swap_gives_an_index_two_fates() {
    pipeline::a_void_that_is_not_a_compare_and_swap_gives_an_index_two_fates();
}

#[test]
fn a_cell_retired_while_it_takes_the_next_lap_keeps_the_new_descriptor() {
    pipeline::a_cell_retired_while_it_takes_the_next_lap_keeps_the_new_descriptor();
}

#[test]
#[should_panic(expected = "a cleaner took the descriptor of an install under way")]
fn a_cleaner_that_takes_an_install_under_way_empties_a_ready_cell() {
    pipeline::a_cleaner_that_takes_an_install_under_way_empties_a_ready_cell();
}

#[test]
fn a_descriptor_loaded_while_it_is_installed_is_the_one_installed() {
    pipeline::a_descriptor_loaded_while_it_is_installed_is_the_one_installed();
}

#[test]
#[should_panic(expected = "a loader found the slot READY and its cell empty")]
fn a_stamp_that_does_not_release_the_pointer_shows_an_empty_ready_cell() {
    pipeline::a_stamp_that_does_not_release_the_pointer_shows_an_empty_ready_cell();
}

#[test]
fn a_descriptor_loaded_while_its_slot_is_retired_is_alive() {
    pipeline::a_descriptor_loaded_while_its_slot_is_retired_is_alive();
}

#[test]
#[should_panic(expected = "a loader read a descriptor that was already dropped")]
fn a_loader_that_does_not_pin_reads_a_dropped_descriptor() {
    pipeline::a_loader_that_does_not_pin_reads_a_dropped_descriptor();
}

#[test]
fn a_tip_loaded_while_it_is_replaced_is_alive() {
    pipeline::a_tip_loaded_while_it_is_replaced_is_alive();
}

#[test]
fn a_tip_replaced_twice_while_loaded_drops_each_value_once() {
    pipeline::a_tip_replaced_twice_while_loaded_drops_each_value_once();
}

#[test]
#[should_panic(expected = "a loader holds a tip that was already dropped")]
fn a_replace_that_reads_the_pins_before_its_swap_drops_a_loaded_tip() {
    pipeline::a_replace_that_reads_the_pins_before_its_swap_drops_a_loaded_tip();
}

#[test]
fn two_helpers_decide_two_slots_in_ring_order() {
    pipeline::two_helpers_decide_two_slots_in_ring_order();
}

#[test]
#[should_panic(expected = "two slots took overlapping sequences, or left a hole")]
fn a_frontier_passed_before_the_tip_moved_gives_two_slots_one_sequence() {
    pipeline::a_frontier_passed_before_the_tip_moved_gives_two_slots_one_sequence();
}

#[test]
fn a_round_is_claimed_by_one_commit_and_wakes_the_other() {
    pipeline::a_round_is_claimed_by_one_commit_and_wakes_the_other();
}

/// Loom reports the overlap itself: the two syncs write one tracked cell
/// with nothing ordering them.
#[test]
#[should_panic(expected = "Concurrent write accesses")]
fn a_claim_that_is_a_load_and_a_store_lets_two_syncs_overlap() {
    pipeline::a_claim_that_is_a_load_and_a_store_lets_two_syncs_overlap();
}

#[test]
fn a_published_slot_is_visible_to_whoever_sees_it_published() {
    pipeline::a_published_slot_is_visible_to_whoever_sees_it_published();
}

#[test]
#[should_panic(expected = "a slot was published before the horizon reached it")]
fn a_frontier_passed_before_the_horizon_hides_a_published_slot() {
    pipeline::a_frontier_passed_before_the_horizon_hides_a_published_slot();
}

#[test]
fn a_decision_computed_before_a_rollback_never_becomes_the_tip() {
    pipeline::a_decision_computed_before_a_rollback_never_becomes_the_tip();
}

#[test]
#[should_panic(expected = "a decision of the discarded state became the tip")]
fn a_settle_that_ignores_the_epoch_brings_a_discarded_state_back() {
    pipeline::a_settle_that_ignores_the_epoch_brings_a_discarded_state_back();
}

#[test]
fn sections_never_overlap_and_none_is_left_behind() {
    step::sections_never_overlap_and_none_is_left_behind();
}

#[test]
#[should_panic(expected = "a section was left on the list with nobody to run it")]
fn a_step_given_up_without_looking_leaves_a_section_behind() {
    step::a_step_given_up_without_looking_leaves_a_section_behind();
}

#[test]
fn a_job_its_holder_parks_is_run_by_the_thread_that_waited() {
    park::a_job_its_holder_parks_is_run_by_the_thread_that_waited();
}

#[test]
#[should_panic(expected = "a thread slept on a parked job with nobody left to wake it")]
fn a_waiter_that_does_not_look_again_sleeps_on_a_parked_job() {
    park::a_waiter_that_does_not_look_again_sleeps_on_a_parked_job();
}

#[test]
fn a_job_parked_for_the_step_is_woken_when_it_is_given_up() {
    park::a_job_parked_for_the_step_is_woken_when_it_is_given_up();
}

#[test]
#[should_panic(expected = "a job parked for the step and was not woken when it was given up")]
fn a_job_that_parks_without_a_second_try_sleeps_on_a_free_step() {
    park::a_job_that_parks_without_a_second_try_sleeps_on_a_free_step();
}

#[test]
fn a_rollback_parked_for_a_write_in_flight_cuts_after_it_left() {
    park::a_rollback_parked_for_a_write_in_flight_cuts_after_it_left();
}

#[test]
#[should_panic(expected = "a write of a discarded commit landed after the cut")]
fn a_write_counted_in_after_its_epoch_check_lands_after_the_cut() {
    park::a_write_counted_in_after_its_epoch_check_lands_after_the_cut();
}

#[test]
fn a_late_copy_never_lands_under_a_sync() {
    park::a_late_copy_never_lands_under_a_sync();
}

#[test]
#[should_panic(expected = "a copy of the record landed under the sync")]
fn a_copy_that_does_not_look_again_lands_under_the_sync() {
    park::a_copy_that_does_not_look_again_lands_under_the_sync();
}

#[test]
#[should_panic(expected = "a copy of the record landed under the sync")]
fn a_sync_that_does_not_wait_has_a_copy_land_under_it() {
    park::a_sync_that_does_not_wait_has_a_copy_land_under_it();
}

#[test]
#[should_panic(expected = "a copy of the record landed under the sync")]
fn a_sync_that_only_loads_the_sides_has_a_copy_land_under_it() {
    park::a_sync_that_only_loads_the_sides_has_a_copy_land_under_it();
}

#[test]
fn a_queue_off_the_list_when_the_job_lands_is_told_once() {
    park::a_queue_off_the_list_when_the_job_lands_is_told_once();
}

#[test]
#[should_panic(expected = "a queue was never told the landing")]
fn a_wake_that_registers_again_blindly_loses_a_landing() {
    park::a_wake_that_registers_again_blindly_loses_a_landing();
}

#[test]
fn a_wake_that_comes_before_the_park_tells_the_queues_with_the_job_free() {
    park::a_wake_that_comes_before_the_park_tells_the_queues_with_the_job_free();
}

#[test]
#[should_panic(expected = "a queue was told the job may go on while it was still claimed")]
fn a_wake_that_tells_at_once_reaches_a_queue_with_the_job_claimed() {
    park::a_wake_that_tells_at_once_reaches_a_queue_with_the_job_claimed();
}

#[test]
fn a_parked_writer_is_woken_when_its_record_is_written() {
    write_unit::a_parked_writer_is_woken_when_its_record_is_written();
}

#[test]
fn a_writer_parked_for_the_next_claim_is_woken_when_the_unit_is_let_go() {
    write_unit::a_writer_parked_for_the_next_claim_is_woken_when_the_unit_is_let_go();
}

#[test]
#[should_panic(expected = "a writer parked for the next claim was never woken")]
fn a_holder_that_tells_nobody_leaves_a_writer_asleep() {
    write_unit::calibration_a_holder_that_tells_nobody_leaves_a_writer_asleep();
}

#[test]
fn a_run_never_lands_under_a_sync() {
    write_unit::a_run_never_lands_under_a_sync();
}

#[test]
#[should_panic(expected = "a run landed under the sync")]
fn a_run_that_does_not_look_again_lands_under_the_sync() {
    write_unit::calibration_a_run_that_does_not_look_again_lands_under_the_sync();
}

#[test]
fn one_thread_at_a_time_holds_the_unit() {
    write_unit::one_thread_at_a_time_holds_the_unit();
}

#[test]
#[should_panic]
fn a_claim_that_is_a_load_and_a_store_writes_two_runs_at_once() {
    write_unit::calibration_a_claim_that_is_a_load_and_a_store_writes_two_runs_at_once();
}

#[test]
fn a_queued_commit_is_told_when_the_unit_is_let_go() {
    write_unit::a_queued_commit_is_told_when_the_unit_is_let_go();
}

#[test]
#[should_panic(expected = "a commit left on its queue waits on a job nobody releases")]
fn a_commit_that_notes_before_it_loads_waits_on_a_job_nobody_releases() {
    write_unit::calibration_a_commit_that_notes_before_it_loads_waits_on_a_job_nobody_releases();
}

#[test]
fn one_thread_encodes_a_record() {
    write_unit::one_thread_encodes_a_record();
}

#[test]
#[should_panic]
fn an_encoding_claimed_with_a_load_and_a_store_writes_one_buffer_twice() {
    write_unit::calibration_an_encoding_claimed_with_a_load_and_a_store_writes_one_buffer_twice();
}

#[test]
fn a_writer_parked_on_a_record_being_encoded_is_woken_when_it_is_filled() {
    write_unit::a_writer_parked_on_a_record_being_encoded_is_woken_when_it_is_filled();
}

#[test]
#[should_panic(expected = "a writer parked on a record being encoded was never woken")]
fn a_fill_that_wakes_nobody_leaves_a_writer_asleep() {
    write_unit::calibration_a_fill_that_wakes_nobody_leaves_a_writer_asleep();
}

#[test]
fn a_call_far_from_its_turn_is_named_and_told() {
    turn_wait::a_call_far_from_its_turn_is_named_and_told();
}

#[test]
#[should_panic(expected = "a call far from its turn was never told")]
fn a_pass_that_reads_the_count_first_leaves_a_sleeper_untold() {
    turn_wait::calibration_a_pass_that_reads_the_count_first_leaves_a_sleeper_untold();
}

#[test]
fn a_call_far_from_its_turn_is_woken_when_its_slot_lands() {
    turn_wait::a_call_far_from_its_turn_is_woken_when_its_slot_lands();
}

#[test]
#[should_panic(expected = "a call far from its turn slept through its landing")]
fn a_call_that_looks_before_it_registers_sleeps_through_its_landing() {
    turn_wait::calibration_a_call_that_looks_before_it_registers_sleeps_through_its_landing();
}

#[test]
fn the_fast_path_racing_a_claimer_writes_only_whole_records() {
    write_unit::the_fast_path_racing_a_claimer_writes_only_whole_records();
}

#[test]
#[should_panic]
fn a_fast_path_that_marks_its_record_filled_first_lets_a_claimer_read_it_half_encoded() {
    write_unit::calibration_a_fast_path_that_marks_its_record_filled_first_lets_a_claimer_read_it_half_encoded();
}

#[test]
fn two_writers_behind_a_holder_are_never_left_asleep() {
    write_unit::two_writers_behind_a_holder_are_never_left_asleep();
}

#[test]
#[should_panic(expected = "a writer behind the holder was left asleep")]
fn a_note_trusted_off_a_load_leaves_a_writer_asleep() {
    write_unit::calibration_a_note_trusted_off_a_load_leaves_a_writer_asleep();
}

#[test]
#[should_panic(expected = "a writer behind the holder was left asleep")]
fn a_holder_that_wakes_the_first_record_only_leaves_a_writer_asleep() {
    write_unit::calibration_a_holder_that_wakes_the_first_record_only_leaves_a_writer_asleep();
}
