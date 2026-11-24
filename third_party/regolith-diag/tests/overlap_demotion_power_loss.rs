//! The open that moves an overlapping level, across a power cut at each of
//! its I/O calls (E14).
//!
//! The child opens a copy of `tests/fixtures/v0_1_9_overlap`, the database
//! the 0.1.9 release left with an overlapping bottom level; what is cut,
//! rebuilt and checked is `common/open_sweep.rs`'s. The repair here is the
//! move, counted by `regolith.manifest.tables_demoted`, with the copies of
//! the moved tables' range tombstones it owes: a move recorded without
//! them would bring back the key the moved table's range tombstone deletes.
//!
//! # Linux only
//!
//! `LD_PRELOAD` interposition is a glibc mechanism; the file is compiled out
//! elsewhere. See `power_loss.rs` for what the shim does and does not prove.
#![cfg(target_os = "linux")]

mod common;

use common::fault::{self, TearMode};
use common::open_sweep::{self, Upgrade};
use common::overlap_fixture::{self as fixture, MOVED};

#[test]
fn crash_child() {
    fault::child_entrypoint(open_sweep::dispatch);
}

const MOVE: Upgrade = Upgrade {
    copy_to: fixture::copy_to,
    state: fixture::state,
    assert_reads: fixture::assert_db_reads,
    repaired: fixture::moved,
    count: MOVED,
};

#[test]
fn a_power_cut_at_any_call_of_the_open_leaves_every_read_right() {
    open_sweep::sweep(&MOVE, TearMode::Truncate, false);
}

#[test]
fn a_power_cut_that_tears_a_sector_at_any_call_of_the_open_leaves_every_read_right() {
    open_sweep::sweep(&MOVE, TearMode::TornSector, false);
}

/// The same when the open that moves the tables is the first with a key
/// provider, which also rewrites the manifest sealed.
#[test]
fn a_power_cut_at_any_call_of_the_open_that_also_turns_encryption_on_leaves_every_read_right() {
    open_sweep::sweep(&MOVE, TearMode::Truncate, true);
}
