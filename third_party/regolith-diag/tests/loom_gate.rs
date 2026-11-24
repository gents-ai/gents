//! Loom models of the compaction gate and of a job parked on its queue
//! (plan 4.6, E28).
//!
//! ```text
//! RUSTFLAGS="--cfg loom" cargo test --release --test loom_gate
//! ```
//!
//! The models live in `src/engine/loom_model/gate.rs`, next to the
//! crate-private types they drive. What each proves:
//!
//! - **A job never overlaps a pass**: a pass takes the gate shared while a
//!   job takes it alone; the tables they touch are a tracked cell, so a job
//!   beside a pass is a data race the model fails on.
//! - **E28, bounded bypass**: a job thread takes the gate back four times in
//!   a row while a pass waits; the pass is found short at most once when it
//!   arrives and `MAX_BYPASS` (2 under loom) times after, then let in.
//! - **No lost retry**: a job parked on its queue, waiting for a gate a pass
//!   holds, is pushed back to the queue when the gate frees, and its idle
//!   owner wakes and runs it, once.
//!
//! Each is paired with a calibration that gets the one step wrong and must
//! fail: a job that skips the gate, the gate this one replaced, whose
//! waiting exclusive user holds a pass back without bound, and a parked job
//! stepped with a waker that reaches no queue.

#![cfg(loom)]

use regolith::loom_exports::gate;

#[test]
fn a_job_never_overlaps_a_pass() {
    gate::a_job_never_overlaps_a_pass();
}

#[test]
#[should_panic]
fn calibration_a_job_that_skips_the_gate_races_a_pass() {
    gate::calibration_a_job_that_skips_the_gate_races_a_pass();
}

#[test]
fn a_waiting_pass_is_passed_over_at_most_max_bypass_times() {
    gate::a_waiting_pass_is_passed_over_at_most_max_bypass_times();
}

#[test]
#[should_panic(expected = "held back")]
fn calibration_the_old_gate_holds_a_pass_back_without_bound() {
    gate::calibration_the_old_gate_holds_a_pass_back_without_bound();
}

#[test]
fn a_parked_job_is_pushed_back_when_the_gate_frees() {
    gate::a_parked_job_is_pushed_back_when_the_gate_frees();
}

#[test]
#[should_panic]
fn calibration_a_parked_job_with_no_way_back_is_never_run() {
    gate::calibration_a_parked_job_with_no_way_back_is_never_run();
}
