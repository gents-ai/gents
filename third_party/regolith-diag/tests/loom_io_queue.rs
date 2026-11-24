//! Loom models of the per-thread I/O queues (plan 4.10, D53).
//!
//! ```text
//! RUSTFLAGS="--cfg loom" cargo test --release --test loom_io_queue
//! ```
//!
//! The models live in `src/engine/loom_model/io_queue.rs`, next to the
//! crate-private types they drive. What each proves:
//!
//! - **One claim, one run**: three threads race one unit's
//!   compare-and-swap; one wins, and the work runs once.
//! - **Exactly one completion per waiting queue**: registrations race the
//!   finish that closes the waiter list; a queue that registered gets one
//!   completion, one that was refused gets none and sees the outcome.
//! - **An idle owner is woken once, a busy one never**: a push races the
//!   owner going idle, and the owner waking up again.
//! - **No lost wakeup on a wait**: a task polls a read's wait, then again
//!   with a new waker, while the owner completes it; the waker it last gave
//!   is woken. A clone polled on another thread is woken through its own.
//! - **No unit outlives close**: a miss that started before close puts its
//!   unit in the table while close sweeps it; once close has returned and
//!   the miss has ended, the unit is released, by the sweep or by the miss
//!   itself. When close returns, the miss may still hold it in the table.
//! - **A group's sync runs once**: three member threads race the job a
//!   commit group's fsync is; one claim wins and the sync runs once.
//! - **Exactly one completion per member queue**: member queues register on
//!   the group's job while another member runs it; each that registered is
//!   pushed one completion.
//! - **A ticket's callback runs exactly once**: `on_complete` races the
//!   delivery of the outcome; the callback runs once, with the outcome, on
//!   the registering thread only once the ticket is ready and on the
//!   delivering thread only before.
//!
//! Each is paired with a calibration that writes the one step the wrong way
//! and must fail; without them a search that never reached the bad
//! interleaving would pass for the same reason a broken one does.

#![cfg(loom)]

use regolith::loom_exports::{io_queue, jobs};

#[test]
fn a_unit_is_claimed_once_and_runs_once() {
    io_queue::a_unit_is_claimed_once_and_runs_once();
}

#[test]
fn every_registered_queue_gets_one_completion() {
    io_queue::every_registered_queue_gets_one_completion();
}

#[test]
fn an_idle_owner_is_woken_once() {
    io_queue::an_idle_owner_is_woken_once();
}

#[test]
fn a_busy_owner_is_never_woken() {
    io_queue::a_busy_owner_is_never_woken();
}

#[test]
fn a_wait_is_never_lost() {
    io_queue::a_wait_is_never_lost();
}

#[test]
fn a_cloned_wait_is_woken_too() {
    io_queue::a_cloned_wait_is_woken_too();
}

#[test]
#[should_panic(expected = "the unit ran twice")]
fn calibration_a_claim_without_a_cas_runs_twice() {
    io_queue::calibration_a_claim_without_a_cas_runs_twice();
}

#[test]
#[should_panic(expected = "a registered queue got no completion")]
fn calibration_a_close_apart_from_the_take_loses_a_completion() {
    io_queue::calibration_a_close_apart_from_the_take_loses_a_completion();
}

#[test]
#[should_panic(expected = "an idle owner slept on a non-empty inbox")]
fn calibration_an_idle_mark_apart_from_the_inbox_loses_a_wakeup() {
    io_queue::calibration_an_idle_mark_apart_from_the_inbox_loses_a_wakeup();
}

#[test]
#[should_panic(expected = "a wait parked with nobody left to wake it")]
fn calibration_a_wait_without_a_second_look_hangs() {
    io_queue::calibration_a_wait_without_a_second_look_hangs();
}

#[test]
fn no_unit_outlives_close() {
    io_queue::no_unit_outlives_close();
}

#[test]
#[should_panic(expected = "a unit outlived close")]
fn calibration_a_miss_checked_only_when_it_starts_outlives_close() {
    io_queue::calibration_a_miss_checked_only_when_it_starts_outlives_close();
}

#[test]
fn a_job_is_claimed_once_and_runs_once() {
    jobs::a_job_is_claimed_once_and_runs_once();
}

#[test]
fn every_member_queue_gets_one_completion() {
    jobs::every_member_queue_gets_one_completion();
}

#[test]
fn a_ticket_callback_runs_exactly_once() {
    jobs::a_ticket_callback_runs_exactly_once();
}

#[test]
#[should_panic(expected = "the group synced twice")]
fn calibration_a_job_claimed_without_a_cas_runs_twice() {
    jobs::calibration_a_job_claimed_without_a_cas_runs_twice();
}

#[test]
#[should_panic(expected = "a callback ran on the registering thread before the ticket was ready")]
fn calibration_a_list_closed_before_readiness_runs_a_callback_early() {
    jobs::calibration_a_list_closed_before_readiness_runs_a_callback_early();
}

#[test]
#[should_panic(expected = "the callback was lost")]
fn calibration_a_callback_pushed_after_the_take_is_lost() {
    jobs::calibration_a_callback_pushed_after_the_take_is_lost();
}
