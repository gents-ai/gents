//! Loom models of the pessimistic lock table (plan 4.6, 4.10; D49, D53).
//!
//! ```text
//! RUSTFLAGS="--cfg loom" cargo test --release --test loom_locks
//! ```
//!
//! The models live in `src/transaction/lock_loom.rs`, next to the
//! crate-private types they drive. What each proves:
//!
//! - **One holder per key**: two owners take, hold and release one key's
//!   lock; the value they guard is a loom cell, so two holders at once fail
//!   the model, and a waiter the release never wakes parks for good and
//!   fails it as a deadlock.
//! - **Bounded bypass**: a waiter keeps its place while another owner takes
//!   the lock back after every release; it loses at most `MAX_BYPASS` races
//!   (2 under loom) and is then handed the lock.
//! - **A wait reaches its own queue**: a transaction records its wait on its
//!   queue and polls while the holder releases; a wait left pending has one
//!   completion on its own queue, none on another, and is ready only once
//!   its queue's owner takes it in.
//! - **A retired entry is never used**: the last use of a table entry is
//!   given back while another caller takes one; never both.
//! - **No wait outlives close**: a wait queued while close marks the gate and
//!   wakes every waiter is told, or its call abandons it.
//! - **Two calls on one transaction share one wait**: two calls find the key
//!   held at once and may both make a wait; one is published, the other is
//!   given back unpolled, and the holder's release reaches the published one,
//!   so the transaction takes the freed lock.
//! - **Two pollers on one wait are both told**: two calls of one transaction
//!   poll one queued wait while the holder releases; a call that meets the
//!   other's poll returns waiting, and is told whenever the release's wake
//!   fell inside that poll, so the free lock never waits on the poller's
//!   caller coming back.
//! - **A poll racing a handoff is never fooled** (defect F1): a release
//!   hands the lock to an owed wait, name first and grant second, while the
//!   waiting transaction polls; a poll that says "held" is right, and the
//!   lock ends with the transaction.
//! - **A take racing a push keeps the order**: the owner pushes `on_abort`
//!   callbacks into the first four its claim holds in place while another
//!   thread takes them; the take reads only slots whose push it saw complete
//!   and every entry comes out once, oldest first, in place and across the
//!   edge to the overflow.
//!
//! Each is paired with a calibration that writes the one step the wrong way
//! and must fail.

#![cfg(loom)]

use regolith::loom_exports::locks;

#[test]
fn two_owners_never_hold_one_key() {
    locks::two_owners_never_hold_one_key();
}

#[test]
fn a_waiter_loses_at_most_the_bound() {
    locks::a_waiter_loses_at_most_the_bound();
}

#[test]
fn a_lock_wait_reaches_its_own_queue() {
    locks::a_lock_wait_reaches_its_own_queue();
}

#[test]
fn a_retired_entry_is_never_used() {
    locks::a_retired_entry_is_never_used();
}

#[test]
fn no_lock_wait_outlives_close() {
    locks::no_lock_wait_outlives_close();
}

#[test]
fn two_calls_on_one_transaction_share_one_wait() {
    locks::two_calls_on_one_transaction_share_one_wait();
}

#[test]
#[should_panic(expected = "the transaction never took the freed lock")]
fn calibration_two_published_waits_hide_the_told_one() {
    locks::calibration_two_published_waits_hide_the_told_one();
}

#[test]
fn two_pollers_on_one_wait_are_both_told() {
    locks::two_pollers_on_one_wait_are_both_told();
}

#[test]
#[should_panic(expected = "a call that met the poll was never told")]
fn calibration_a_poll_without_the_hand_back_leaves_a_call_untold() {
    locks::calibration_a_poll_without_the_hand_back_leaves_a_call_untold();
}

#[test]
fn a_poll_racing_a_handoff_is_never_fooled() {
    locks::a_poll_racing_a_handoff_is_never_fooled();
}

#[test]
#[should_panic(expected = "a poll said the transaction holds a lock the handoff then freed")]
fn calibration_a_poll_trusting_a_half_made_handoff_breaks_exclusion() {
    locks::calibration_a_poll_trusting_a_half_made_handoff_breaks_exclusion();
}

#[test]
fn a_take_racing_a_push_keeps_the_order() {
    locks::a_take_racing_a_push_keeps_the_order();
}

#[test]
fn a_take_racing_a_push_keeps_the_order_across_the_edge() {
    locks::a_take_racing_a_push_keeps_the_order_across_the_edge();
}

#[test]
#[should_panic(expected = "two owners hold one key")]
fn calibration_a_take_without_a_cas_admits_two_holders() {
    locks::calibration_a_take_without_a_cas_admits_two_holders();
}

#[test]
#[should_panic(expected = "a waiter lost more races than the bound allows")]
fn calibration_a_release_that_never_hands_off_starves_the_waiter() {
    locks::calibration_a_release_that_never_hands_off_starves_the_waiter();
}

#[test]
#[should_panic(expected = "a lock wait was never told")]
fn calibration_a_wait_recorded_after_the_poll_is_never_told() {
    locks::calibration_a_wait_recorded_after_the_poll_is_never_told();
}

#[test]
#[should_panic(expected = "an entry out of the table was used again")]
fn calibration_a_retire_without_a_cas_retires_a_used_entry() {
    locks::calibration_a_retire_without_a_cas_retires_a_used_entry();
}

#[test]
#[should_panic(expected = "a lock wait outlived close")]
fn calibration_a_wait_that_ignores_the_gate_outlives_close() {
    locks::calibration_a_wait_that_ignores_the_gate_outlives_close();
}

#[test]
#[should_panic(expected = "a take handed an entry out of order or not exactly once")]
fn calibration_a_take_past_a_slot_not_filled_breaks_the_order() {
    locks::calibration_a_take_past_a_slot_not_filled_breaks_the_order();
}
