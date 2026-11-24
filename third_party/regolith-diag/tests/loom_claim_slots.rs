//! Loom models for the slots the open transactions' claims sit in, for
//! `close` to find (`src/engine/open_transactions/claim_slots.rs`).
//!
//! ```text
//! RUSTFLAGS="--cfg loom" cargo test --release --test loom_claim_slots
//! ```
//!
//! The models live in `src/engine/open_transactions/loom_model.rs` and drive
//! the production `ClaimSlots`. What each proves:
//!
//! - **Every reference dropped once**: owners list and end claims while
//!   close drains; no slot is held twice, close reads each claim at most
//!   once and after it was made, and each slot's reference is dropped by
//!   exactly one of the owner and close.
//! - **Close finds what was listed before it began**, in a chunk made while
//!   it ran.
//! - **No ABA** on the free stack, and **a pushed slot keeps its link**: no
//!   slot is held twice or lost.
//!
//! Each is paired with a calibration that plants one step wrong and must
//! fail. `proofs/tla/ClaimSlots.tla` is the protocol's TLA+ model.

#![cfg(loom)]

use regolith::loom_exports::claim_slots;

#[test]
fn every_claim_is_dropped_once() {
    claim_slots::every_claim_is_dropped_once();
}

#[test]
#[should_panic(expected = "slot reference was dropped twice")]
fn calibration_a_load_and_store_drops_a_claim_twice() {
    claim_slots::calibration_a_load_and_store_drops_a_claim_twice();
}

#[test]
#[should_panic(expected = "slot reference was never dropped")]
fn calibration_giving_back_first_loses_a_claim() {
    claim_slots::calibration_giving_back_first_loses_a_claim();
}

/// The claim is made after its slot is reserved and stored with `Relaxed`:
/// nothing orders close's read of it after its making.
#[test]
#[should_panic(expected = "Causality violation")]
fn calibration_a_relaxed_fill_hides_the_claim() {
    claim_slots::calibration_a_relaxed_fill_hides_the_claim();
}

#[test]
fn close_finds_a_claim_listed_before_it() {
    claim_slots::close_finds_a_claim_listed_before_it();
}

#[test]
#[should_panic(expected = "close missed a claim listed before it began")]
fn calibration_a_drain_that_stops_early_misses_a_claim() {
    claim_slots::calibration_a_drain_that_stops_early_misses_a_claim();
}

#[test]
fn a_reused_slot_is_never_held_twice() {
    claim_slots::a_reused_slot_is_never_held_twice();
}

#[test]
#[should_panic(expected = "free stack broken")]
fn calibration_without_the_tag_a_slot_is_held_twice() {
    claim_slots::calibration_without_the_tag_a_slot_is_held_twice();
}

#[test]
fn a_pushed_slot_keeps_its_link() {
    claim_slots::a_pushed_slot_keeps_its_link();
}

#[test]
#[should_panic(expected = "free stack broken")]
fn calibration_a_relaxed_push_loses_a_slot() {
    claim_slots::calibration_a_relaxed_push_loses_a_slot();
}
