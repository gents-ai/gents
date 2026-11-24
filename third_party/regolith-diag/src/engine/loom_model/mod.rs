//! Loom models for the arena memtable and the engine's read horizon.
//!
//! Loom replays each model under every thread interleaving the C11
//! memory model permits, so what it checks is *ordering*: which value an
//! `Acquire` load may legally return given the `Release` stores that
//! preceded it. That is exactly the shape of the memtable's publication
//! protocol (invariant S1 in `engine::skiplist`) and of the read
//! horizon (H1 to H3 in `engine::read_horizon`).
//!
//! # What loom here does and does not prove
//!
//! Loom sees the atomics that come from `crate::sync::internal` and the locks
//! built on them, and nothing else. It therefore checks:
//!
//! - that a reader which observes a node observes the links that were
//!   published before it, so a concurrent insert can neither hide an
//!   already-published key nor expose a half-linked tower;
//! - that a value read out of a node always belongs to the key and
//!   sequence the reader matched on;
//! - that the read horizon never advertises a sequence whose memtable
//!   insert the reader cannot yet see;
//! - that the flush handoff leaves every key reachable at every instant;
//! - that a reader pinning one version walks a whole snapshot across a
//!   concurrent compaction, and that a flush and a compaction
//!   publishing at once cannot lose one another's edits;
//! - that two threads allocating from one arena at once are handed ranges
//!   of their own (invariant A8 in `engine::arena`), two threads inserting
//!   into one skip list at once both land, and one entry inserted twice is
//!   held once (S2 in `engine::skiplist`): the memtable takes writes from
//!   any thread, as the commit pipeline needs;
//! - that the sections of a step never overlap and none is left behind
//!   (`engine::step`);
//! - that a job a queue's poll parks instead of waiting is woken, and that
//!   nobody who waited on it is left behind (`engine::commit::park`).
//!
//! The version models in [`version`] are protocol models and say so:
//! the production `VersionSet` writes a manifest record and opens an
//! SSTable reader inside `apply`, and its locks come from `std::sync`
//! rather than from `crate::sync::internal`, so loom can neither run it nor see
//! its ordering. What they reproduce is the part loom can decide - the
//! `Arc<Version>` pin, the clone-mutate-store, and the lock scope around
//! it - with the table contents stood in for.
//!
//! It does **not** check the raw key and value bytes for a data race:
//! those are plain arena memory, not a `loom::cell::UnsafeCell`, and
//! loom's cooperative scheduler physically sequences the writes anyway.
//! Byte-level aliasing, provenance and use-after-free are miri's job,
//! and `tests/loom_memtable.rs` documents which invariant each tool
//! covers.
//!
//! # Model size
//!
//! Every model is two or three threads doing two or three operations.
//! Nothing about the structure is shrunk for the checker: the tower is
//! its production `MAX_HEIGHT` of 12, because loom's partial-order
//! reduction prunes the levels no thread ever writes to, and the search
//! is the same size at 3 levels as at 12. [`explore`] counts both the
//! interleavings loom ran and the ones that reached the state being
//! checked, and fails the model if either is implausibly small: a model
//! that explores one schedule, or whose conditional assertion never
//! fires, is worse than no model at all.
//!
//! # Calibration
//!
//! Every positive model is paired with one that deliberately gets the
//! ordering wrong - the two-step seek `seek_ge` replaced, a flush that
//! retires the frozen memtable before it installs the table, a relaxed
//! read horizon, a compaction that publishes its removals and its
//! addition as two versions, two version swaps with nothing serializing
//! them, an arena cursor and a list link moved by a load and a store -
//! and the pair is only meaningful if the wrong one fails. They are run
//! as `#[should_panic]` tests in `tests/loom_memtable.rs`.
//!
//! A calibration stops at the first schedule that trips its assertion,
//! so [`explore`]'s floors never execute for one: the panic unwinds past
//! them. [`Report`] prints on the way out regardless, so the number a
//! calibration reports is how many schedules loom searched before it
//! found the bug, not how large its search space is.

pub mod arena;
pub mod families;
pub mod gate;
pub mod handoff;
pub mod io_queue;
pub mod jobs;
pub mod park;
pub mod skiplist;
pub mod slice;
pub mod snapshots;
pub mod step;
pub mod tombstones;
pub mod version;

use std::sync::Arc as StdArc;
use std::sync::atomic::{AtomicUsize, Ordering as StdOrdering};

use super::arena::ArenaProfile;
use super::lookup_key::LookupKey;
use super::memtable::{MemTable, MemTableConfig};

/// Per-memtable arena budget. One 4 KiB chunk holds every entry any
/// model inserts, so no model allocates a chunk once its threads are
/// running and the shared chunk pool stays uncontended.
const BUDGET: usize = 4 * 1024;

/// Column family every model writes into.
const CF: u32 = 0;

/// A fresh memtable on the embedded arena profile with a `budget`-byte
/// arena and its own chunk pool.
fn memtable_with_budget(budget: usize) -> MemTable {
    let config = MemTableConfig::new(ArenaProfile::EMBEDDED, budget, 2);
    MemTable::new(&config).expect("memtable head allocation")
}

/// A fresh memtable on the embedded arena profile.
fn memtable() -> MemTable {
    memtable_with_budget(BUDGET)
}

/// A lookup key for `user_key` at the newest visible sequence.
fn probe(user_key: &[u8]) -> LookupKey {
    LookupKey::new(CF, user_key, u64::MAX)
}

/// A count of the executions that reached the state a model exists to
/// check.
///
/// An interleaving count alone cannot tell a real search from one whose
/// conditional assertion never fired: a reader that never happens to
/// observe the writer's key passes every `if let Some(..)` body without
/// executing one. The witness count is that missing half, and [`explore`]
/// fails a model whose witness stays at zero.
#[derive(Clone)]
pub(crate) struct Witness(StdArc<AtomicUsize>);

impl Witness {
    /// Record that this execution reached the interesting state.
    pub(crate) fn record(&self) {
        self.0.fetch_add(1, StdOrdering::Relaxed);
    }

    fn count(&self) -> usize {
        self.0.load(StdOrdering::Relaxed)
    }
}

/// Prints the interleaving and witness counts on the way out, including
/// when the model failed and the stack is unwinding: the size of the
/// search is part of a failure report, not only of a success.
struct Report {
    name: &'static str,
    runs: StdArc<AtomicUsize>,
    witness: Witness,
}

impl Drop for Report {
    fn drop(&mut self) {
        println!(
            "loom model {}: {} interleavings explored, {} witnessed",
            self.name,
            self.runs.load(StdOrdering::Relaxed),
            self.witness.count()
        );
    }
}

/// Run `model` under loom and report how large the search was.
///
/// `min_interleavings` is the calibration on the search: it is how many
/// distinct schedules the model must produce for its assertions to mean
/// anything. `min_witnesses` is the calibration on the assertions: it is
/// how many of those schedules must have reached the state the model
/// exists to check. A model that fails either floor fails, because a
/// model that explores one schedule, or that never reaches its own
/// interesting branch, is worse than no model at all.
pub(crate) fn explore(
    name: &'static str,
    min_interleavings: usize,
    min_witnesses: usize,
    model: impl Fn(&Witness) + Sync + Send + 'static,
) {
    explore_bounded(name, None, min_interleavings, min_witnesses, model);
}

/// [`explore`] with at most `preemptions` preemptions per schedule, or
/// none when `None`. `LOOM_MAX_PREEMPTIONS` overrides a bound given here.
/// Models with three threads that each make several atomic accesses need
/// one: unbounded, their search runs for hours without reaching a new kind
/// of interleaving.
pub(crate) fn explore_bounded(
    name: &'static str,
    preemptions: Option<usize>,
    min_interleavings: usize,
    min_witnesses: usize,
    model: impl Fn(&Witness) + Sync + Send + 'static,
) {
    let runs = StdArc::new(AtomicUsize::new(0));
    let witness = Witness(StdArc::new(AtomicUsize::new(0)));
    let report = Report {
        name,
        runs: StdArc::clone(&runs),
        witness: witness.clone(),
    };
    let counted = {
        let runs = StdArc::clone(&runs);
        let witness = witness.clone();
        move || {
            model(&witness);
            runs.fetch_add(1, StdOrdering::Relaxed);
        }
    };

    let mut builder = loom::model::Builder::new();
    builder.preemption_bound = builder.preemption_bound.or(preemptions);
    builder.check(counted);

    let explored = runs.load(StdOrdering::Relaxed);
    let witnessed = witness.count();
    drop(report);
    assert!(
        explored >= min_interleavings,
        "{name} explored only {explored} interleavings, below the {min_interleavings} floor: \
         the model is too small to be checking anything"
    );
    assert!(
        witnessed >= min_witnesses,
        "{name} reached its interesting state {witnessed} times, below the {min_witnesses} \
         floor: the assertions never fired"
    );
}
