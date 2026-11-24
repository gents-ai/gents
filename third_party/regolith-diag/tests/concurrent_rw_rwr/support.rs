//! What the scenarios share besides the forced-interleaving helpers in the
//! parent file: the storage shapes they run on, a rendezvous that fails
//! instead of hanging, a collector for violations seen while peers still have
//! to arrive, and a measure of whether a commit reached an SSTable.

use std::sync::{Condvar, Mutex};

use regolith::{Db, Options, PerfContext, PerfLevel};

use super::HANDOFF;

/// Where a scenario's data lives while it runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Storage {
    /// Default options: nothing leaves the active memtable, so a conflict
    /// probe is answered there and never reaches a table.
    Memtable,
    /// A small write buffer, so memtables rotate into SSTables by themselves
    /// and compaction runs underneath, with workers flushing on top: a
    /// conflict probe walks tables as well as the memtable.
    Tables,
}

pub const STORAGES: [Storage; 2] = [Storage::Memtable, Storage::Tables];

/// Steps between two flushes of one worker on [`Storage::Tables`].
const FLUSH_EVERY: u64 = 8;

impl Storage {
    pub fn options(self) -> Options {
        match self {
            Self::Memtable => Options::default(),
            Self::Tables => Options::default()
                .write_buffer_size(4 * 1024)
                .l0_compaction_trigger(2),
        }
    }

    pub fn is_tables(self) -> bool {
        self == Self::Tables
    }

    /// Flush behind a write that later probes should find in a table. Does
    /// nothing on [`Storage::Memtable`].
    pub fn flush(self, db: &Db) {
        if self.is_tables() {
            db.flush().unwrap();
        }
    }

    /// Flush on every `FLUSH_EVERY`-th step of a worker loop.
    pub fn churn(self, db: &Db, step: u64) {
        if step.is_multiple_of(FLUSH_EVERY) {
            self.flush(db);
        }
    }
}

/// A barrier that fails instead of hanging: a party that never arrives (it
/// panicked) turns into a panic in every party still waiting after
/// `HANDOFF`, where `std::sync::Barrier` would wait for ever.
pub struct Rendezvous {
    parties: usize,
    /// Parties arrived in this generation, and the generation.
    state: Mutex<(usize, u64)>,
    released: Condvar,
}

impl Rendezvous {
    pub fn new(parties: usize) -> Self {
        Self {
            parties,
            state: Mutex::new((0, 0)),
            released: Condvar::new(),
        }
    }

    pub fn wait(&self) {
        let mut state = self.state.lock().unwrap();
        state.0 += 1;
        if state.0 == self.parties {
            *state = (0, state.1 + 1);
            self.released.notify_all();
            return;
        }
        let generation = state.1;
        let waited = self
            .released
            .wait_timeout_while(state, HANDOFF, |state| state.1 == generation)
            .unwrap()
            .1;
        assert!(
            !waited.timed_out(),
            "a peer did not reach the rendezvous within {HANDOFF:?}"
        );
    }
}

/// Run `stage` on `threads` threads at once; when every one has staged, run
/// `force` on the calling thread; then run `finish` on each thread with what
/// it staged. Everything `force` does therefore lands after every thread has
/// staged and before any of them finishes.
///
/// A thread that panics while it stages never arrives, which fails its peers
/// at the rendezvous after `HANDOFF` instead of hanging them. `finish` may
/// assert freely: nothing waits on a thread once it runs.
pub fn forced_between<T>(
    threads: usize,
    stage: impl Fn(usize) -> T + Sync,
    force: impl FnOnce(),
    finish: impl Fn(usize, T) + Sync,
) {
    let staged = Rendezvous::new(threads + 1);
    std::thread::scope(|scope| {
        let (staged, stage, finish) = (&staged, &stage, &finish);
        let workers: Vec<_> = (0..threads)
            .map(|t| {
                scope.spawn(move || {
                    let mine = stage(t);
                    staged.wait(); // every thread has staged
                    staged.wait(); // what was forced has landed
                    finish(t, mine);
                })
            })
            .collect();
        staged.wait();
        force();
        staged.wait();
        for worker in workers {
            if let Err(panic) = worker.join() {
                std::panic::resume_unwind(panic);
            }
        }
    });
}

/// Violations seen by threads that must keep arriving at a [`Rendezvous`], so
/// that a failure cannot strand their peers. The test judges them after the
/// scope has ended.
#[derive(Default)]
pub struct Findings(Mutex<Vec<String>>);

impl Findings {
    pub fn check(&self, held: bool, what: impl FnOnce() -> String) {
        if !held {
            self.0.lock().unwrap().push(what());
        }
    }

    /// Fail with the violations recorded, if there are any.
    pub fn judge(self) {
        const SHOWN: usize = 12;
        let found = self.0.into_inner().unwrap();
        assert!(
            found.is_empty(),
            "{} violation(s), the first {}:\n{}",
            found.len(),
            found.len().min(SHOWN),
            found[..found.len().min(SHOWN)].join("\n")
        );
    }
}

/// Run `f` and count the block cache lookups it makes on this thread.
///
/// A conflict probe answered by the active memtable makes none; one that walks
/// into an SSTable makes at least one, hit or miss. A nonzero count is
/// therefore evidence that the work reached a table, and says nothing more:
/// reads made inside `f` count as well, which is why the commit is what a
/// caller wraps when it means a probe.
pub fn table_lookups<T>(f: impl FnOnce() -> T) -> (T, u64) {
    let before = PerfContext::set_level(PerfLevel::EnableCount);
    PerfContext::reset();
    let out = f();
    let lookups = PerfContext::capture().block_cache_lookup_count;
    PerfContext::set_level(before);
    (out, lookups)
}
