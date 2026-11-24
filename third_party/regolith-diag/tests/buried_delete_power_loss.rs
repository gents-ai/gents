//! The open that copies a range delete an earlier release left below older
//! keys it covers, across a power cut at each of its I/O calls (E37).
//!
//! The child opens a copy of `tests/fixtures/v0_1_9_buried_delete`, where
//! the 0.1.9 release left an ingested table's range delete in the bottom
//! level, below keys of level 1 it deletes, with no level overlapping; what
//! is cut, rebuilt and checked is `common/open_sweep.rs`'s. The repair here
//! is the copy, counted by `regolith.manifest.range_deletes_copied`, and the
//! manifest record that the range deletes were checked: a record made
//! before the log holding the copy is durable would let the reopen skip the
//! check, and `c`, which the range delete deletes, would read back.
//!
//! # Linux only
//!
//! `LD_PRELOAD` interposition is a glibc mechanism; the file is compiled out
//! elsewhere. See `power_loss.rs` for what the shim does and does not prove.
#![cfg(target_os = "linux")]

mod common;

use common::buried_fixture::{self as fixture, COPIED};
use common::fault::{self, TearMode};
use common::open_sweep::{self, Upgrade};

#[test]
fn crash_child() {
    fault::child_entrypoint(open_sweep::dispatch);
}

const COPY: Upgrade = Upgrade {
    copy_to: fixture::copy_to,
    state: fixture::state,
    assert_reads: fixture::assert_db_reads,
    repaired: fixture::copied,
    count: COPIED,
};

#[test]
fn a_power_cut_at_any_call_of_the_open_leaves_every_read_right() {
    open_sweep::sweep(&COPY, TearMode::Truncate, false);
}

#[test]
fn a_power_cut_that_tears_a_sector_at_any_call_of_the_open_leaves_every_read_right() {
    open_sweep::sweep(&COPY, TearMode::TornSector, false);
}

/// The same when the open that copies the range delete is the first with
/// a key provider, which also rewrites the manifest sealed.
#[test]
fn a_power_cut_at_any_call_of_the_open_that_also_turns_encryption_on_leaves_every_read_right() {
    open_sweep::sweep(&COPY, TearMode::Truncate, true);
}
