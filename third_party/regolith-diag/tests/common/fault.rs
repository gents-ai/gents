//! Fault-injection substrate for regolith's durability, crash and corruption
//! tests.
//!
//! Four facilities, each in its own submodule and all re-exported here so
//! a test only ever writes `use common::fault::...`:
//!
//! 1. [`shim`] + [`child`]: a subprocess crash harness. The workload runs
//!    in a real child process that really dies, at a kill point chosen by
//!    meaning rather than by wall clock. [`acks`] holds what the child was
//!    told, in records a kill cannot cut.
//! 2. [`power`]: power-loss simulation, which is a different failure from
//!    a process crash and is the one that actually tests durability.
//! 3. [`prefix`]: the valid-prefix validator, the property these tests
//!    exist to assert.
//! 4. [`bytes`]: byte-level mutators and file locators for deliberate
//!    corruption.
//!
//! # The distinction the whole module is built around
//!
//! A `kill -9` does not model a power cut. When a process is killed, every
//! byte it wrote is still in the OS page cache and the kernel goes on to
//! write it to disk. Nothing is lost that was not lost already. A power
//! cut discards everything that was never `fsync`ed. A suite that only
//! kills the process and calls durability proven is a false green, so this
//! module provides both and keeps them clearly separate:
//!
//! * [`run_child`] kills a process. Use it for "the engine survives losing
//!   its memory".
//! * [`simulate_power_loss`] additionally throws away the unsynced bytes.
//!   Use it for "the engine survives losing its disk writes".
//!
//! # How power loss is simulated here, and why
//!
//! `strace` is not installed on this machine and there is no way to
//! install it, so option (a) of the ALICE-style approaches was
//! unavailable. Option (b) was chosen: a **pure-Rust `LD_PRELOAD`
//! interposer** over `write`, `pwrite64`, `pwritev`, `writev`, `fsync`, `fdatasync`,
//! `open`/`openat`, `ftruncate`, `close`, `rename` and `unlink`. It
//! records the real syscall stream of the child, including the kernel
//! thread id and the exact byte offset of every write, and
//! [`power::simulate_power_loss`] replays that stream to work out which
//! byte ranges were never followed by a successful `fsync` on their file,
//! then rewrites the directory as the filesystem would have left it.
//!
//! This is the ALICE model driven by what regolith actually did, not by an
//! assumption about what regolith does. Interposition is sound here because
//! regolith performs every write through `std::fs`, which calls the glibc
//! symbols; it uses `rustix` raw syscalls only for `flock`, `fadvise` and
//! the positional reads of its tables (`pread`), none of which writes file
//! data, and it has no `mmap` write path.
//!
//! The weaker option (c) exists as [`power::simulate_power_loss_modelled`]
//! for platforms where the shim cannot run. **It is not used on this
//! machine.** It encodes an assumption about regolith's sync policy into the
//! test, so it prints a warning and is documented as weaker at its
//! definition. [`shim::available`] reports which world a test is in;
//! [`shim::require`] panics rather than degrading silently.
//!
//! # Determinism
//!
//! Every kill point is either a semantic point in the workload or the nth
//! matching syscall counted by the shim, never a sleep. Every workload is
//! generated from a fixed seed ([`DEFAULT_SEED`]) by the same function in
//! the parent and the child. Every byte of injected garbage comes from a
//! seeded stream. There is no wall-clock dependency anywhere in the kill
//! path, so a fast machine and a slow one crash at the same byte.

#![allow(dead_code, unused_imports)]

pub mod acks;
pub mod bytes;
pub mod child;
pub mod journal;
pub mod power;
pub mod prefix;
pub mod shim;

pub use acks::{ACK_BYTES, Acked, Acks, read_acks};
pub use bytes::{
    copy_tree, file_len, find_manifest, find_ssts, find_wals, first_sst, flip_bit, garbage,
    newest_wal, overwrite_range, truncate_at,
};
pub use child::{
    CHILD_ENV, CHILD_TEST, ChildOutcome, ChildSpec, CrashRun, DEFAULT_SEED, DieKind, Phase,
    Trigger, builtin_workload, child_entrypoint, kill_self, plan, run_child, started_path_for,
};
pub use journal::{Journal, OpKind, Record, journal_path_for};
pub use power::{
    CutPoint, PowerLossOptions, PowerLossReport, TearMode, simulate_power_loss,
    simulate_power_loss_modelled, simulate_power_loss_with,
};
pub use prefix::{
    History, OpValue, PrefixReport, PrefixViolation, Recovery, WriteOp, assert_acked_survived,
    assert_valid_prefix, recover_and_validate, recovered_state, validate_prefix,
    validate_prefix_of_state,
};
