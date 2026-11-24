//! Per-operation performance counters captured in thread-local
//! state.
//!
//! [`Statistics`](crate::Statistics) is a database-global sink -
//! every thread bumps the same atomic counters and the caller
//! reads an aggregate view. That's the right shape for metrics
//! export, but it can't answer "what did *this one* `Db::get`
//! call spend its time on?". [`PerfContext`] is the
//! complementary tool: a thread-local bundle of counters that
//! the caller resets, runs an operation, and snapshots - the
//! per-op breakdown falls out.
//!
//! # Usage
//!
//! ```
//! use regolith::{Db, Options, PerfContext, PerfLevel};
//!
//! # fn main() -> regolith::Result<()> {
//! let db = Db::open("rows", Options::memory())?;
//!
//! PerfContext::set_level(PerfLevel::EnableTime);
//! PerfContext::reset();
//!
//! let _ = db.get(b"some-key")?;
//!
//! let snap = PerfContext::capture();
//! assert_eq!(snap.get_count, 1);
//! println!("get spent {} ns in memtable", snap.get_from_memtable_time_nanos);
//! println!("get spent {} ns in SSTables",  snap.get_from_output_files_time_nanos);
//! # Ok(())
//! # }
//! ```
//!
//! # Zero-cost when disabled
//!
//! The default level is [`PerfLevel::Disable`]. Every
//! instrumentation site checks the level via a thread-local read
//! and branches out cheaply when disabled, so a workload that
//! never touches `PerfContext` pays ~1 cached read per call site
//! and nothing else. `EnableCount` additionally bumps atomic-free
//! `u64` counters; `EnableTime` additionally measures elapsed
//! time via `Instant::now()`.
//!
//! # Thread-local
//!
//! All state lives in a `thread_local!` cell. Background threads
//! (compaction, flush) have their own `PerfContext` that the
//! foreground caller does not see; this mirrors how reads and
//! writes naturally attribute work to the originating thread.

use std::cell::RefCell;

/// Granularity of `PerfContext` measurement. Higher levels
/// produce more detail at the cost of more work per
/// instrumentation site.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PerfLevel {
    /// Perf counters are off. Every instrumentation site
    /// short-circuits at a single thread-local branch.
    #[default]
    Disable,
    /// Count events (cache lookups, bloom checks, memtable hits)
    /// but do not measure elapsed time. `Instant::now()` calls
    /// are skipped.
    EnableCount,
    /// Count events and measure elapsed time. Adds one
    /// `Instant::now()` per timed scope.
    EnableTime,
}

/// Immutable snapshot of the current thread's perf counters.
/// Returned by [`PerfContext::capture`] so callers can inspect
/// fields without holding the live thread-local borrow.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PerfContextSnapshot {
    /// Number of `Db::get` / `Snapshot::get` calls observed on
    /// this thread.
    pub get_count: u64,
    /// Nanoseconds spent consulting the memtable(s) during
    /// `Db::get`. Only populated at [`PerfLevel::EnableTime`].
    pub get_from_memtable_time_nanos: u64,
    /// Nanoseconds spent consulting SSTables (L0 and below)
    /// during `Db::get`. Only populated at
    /// [`PerfLevel::EnableTime`].
    pub get_from_output_files_time_nanos: u64,
    /// Number of `Db::put` / `Db::write` calls observed on this
    /// thread.
    pub write_count: u64,
    /// Nanoseconds spent appending to the WAL during writes.
    /// Only populated at [`PerfLevel::EnableTime`].
    pub write_wal_time_nanos: u64,
    /// Nanoseconds spent inserting into the active memtable
    /// during writes. Only populated at
    /// [`PerfLevel::EnableTime`].
    pub write_memtable_time_nanos: u64,
    /// Number of block cache lookups performed on this thread.
    /// Incremented at both `EnableCount` and `EnableTime`.
    pub block_cache_lookup_count: u64,
    /// Block cache lookups that hit - i.e. the block was
    /// already decompressed and resident.
    pub block_cache_hit_count: u64,
    /// Number of bloom filter checks performed on this thread.
    pub bloom_check_count: u64,
    /// Bloom filter checks that returned "definitely not
    /// present" and spared a block read.
    pub bloom_useful_count: u64,
}

/// Thread-local performance counters. See the module docs for
/// the usage pattern; every accessor is a static function so
/// callers never need a live reference.
pub struct PerfContext;

thread_local! {
    static PERF_LEVEL: RefCell<PerfLevel> = const { RefCell::new(PerfLevel::Disable) };
    static PERF_STATE: RefCell<PerfContextSnapshot> =
        const { RefCell::new(PerfContextSnapshot::new_const()) };
}

impl PerfContextSnapshot {
    /// `const` constructor used to seed the thread-local. Kept
    /// private because `Default::default` returns the same value.
    const fn new_const() -> Self {
        Self {
            get_count: 0,
            get_from_memtable_time_nanos: 0,
            get_from_output_files_time_nanos: 0,
            write_count: 0,
            write_wal_time_nanos: 0,
            write_memtable_time_nanos: 0,
            block_cache_lookup_count: 0,
            block_cache_hit_count: 0,
            bloom_check_count: 0,
            bloom_useful_count: 0,
        }
    }
}

impl PerfContext {
    /// Set the measurement level for the current thread. Returns
    /// the previous level so callers can restore it after a
    /// scoped measurement.
    pub fn set_level(level: PerfLevel) -> PerfLevel {
        PERF_LEVEL.with(|cell| std::mem::replace(&mut *cell.borrow_mut(), level))
    }

    /// Return the current thread's perf level.
    pub fn level() -> PerfLevel {
        PERF_LEVEL.with(|cell| *cell.borrow())
    }

    /// Reset every counter on the current thread's
    /// `PerfContext` to zero. Leaves the level unchanged.
    pub fn reset() {
        PERF_STATE.with(|cell| *cell.borrow_mut() = PerfContextSnapshot::default());
    }

    /// Take a snapshot of the current thread's counters. The
    /// returned value is a plain `Copy` struct that outlives any
    /// subsequent work on the thread-local state.
    pub fn capture() -> PerfContextSnapshot {
        PERF_STATE.with(|cell| *cell.borrow())
    }
}

// ---------------------------------------------------------------
// Internal fast-path helpers consumed by the engine.
//
// Every helper first reads the thread-local perf level so
// disabled sites pay only a single cached read + branch. When
// the level is below what the helper needs, the helper returns
// without touching `PERF_STATE`.
// ---------------------------------------------------------------

/// Quick check: is any counter active on this thread?
#[inline]
pub(crate) fn is_enabled() -> bool {
    PERF_LEVEL.with(|cell| *cell.borrow()) != PerfLevel::Disable
}

/// Quick check: should timing be measured on this thread?
#[inline]
pub(crate) fn is_timed() -> bool {
    PERF_LEVEL.with(|cell| *cell.borrow()) == PerfLevel::EnableTime
}

/// Run `f` on this thread for another thread whose level is `level`, and
/// return what `f` counted apart from this thread's own counters, which are
/// left as they were.
///
/// Whichever thread decides a transaction's slot of the commit ring validates
/// it on behalf of the thread that committed it; that thread [`absorb`]s the
/// counts, so the
/// work lands where the module docs say it does, on the originating thread.
pub(crate) fn on_behalf<T>(level: PerfLevel, f: impl FnOnce() -> T) -> (T, PerfContextSnapshot) {
    /// Puts this thread's own level and counters back, on unwind too.
    struct Restore {
        level: PerfLevel,
        state: PerfContextSnapshot,
    }
    impl Drop for Restore {
        fn drop(&mut self) {
            PERF_STATE.with(|cell| *cell.borrow_mut() = self.state);
            PerfContext::set_level(self.level);
        }
    }
    let restore = Restore {
        level: PerfContext::set_level(level),
        state: PERF_STATE.with(|cell| std::mem::take(&mut *cell.borrow_mut())),
    };
    let out = f();
    let counted = PerfContext::capture();
    drop(restore);
    (out, counted)
}

/// Add what another thread counted on this thread's behalf (see
/// [`on_behalf`]) to this thread's counters.
pub(crate) fn absorb(counted: &PerfContextSnapshot) {
    PERF_STATE.with(|cell| {
        let mut s = cell.borrow_mut();
        s.get_count += counted.get_count;
        s.get_from_memtable_time_nanos += counted.get_from_memtable_time_nanos;
        s.get_from_output_files_time_nanos += counted.get_from_output_files_time_nanos;
        s.write_count += counted.write_count;
        s.write_wal_time_nanos += counted.write_wal_time_nanos;
        s.write_memtable_time_nanos += counted.write_memtable_time_nanos;
        s.block_cache_lookup_count += counted.block_cache_lookup_count;
        s.block_cache_hit_count += counted.block_cache_hit_count;
        s.bloom_check_count += counted.bloom_check_count;
        s.bloom_useful_count += counted.bloom_useful_count;
    });
}

/// Bump the "Db::get" call counter. No-op when perf is off.
#[inline]
pub(crate) fn record_get_call() {
    if !is_enabled() {
        return;
    }
    PERF_STATE.with(|cell| cell.borrow_mut().get_count += 1);
}

/// Bump the "Db::put / Db::write" call counter. No-op when perf
/// is off.
#[inline]
pub(crate) fn record_write_call() {
    if !is_enabled() {
        return;
    }
    PERF_STATE.with(|cell| cell.borrow_mut().write_count += 1);
}

/// Bump the block cache lookup counters.
#[inline]
pub(crate) fn record_block_cache_lookup(hit: bool) {
    if !is_enabled() {
        return;
    }
    PERF_STATE.with(|cell| {
        let mut s = cell.borrow_mut();
        s.block_cache_lookup_count += 1;
        if hit {
            s.block_cache_hit_count += 1;
        }
    });
}

/// Bump the bloom filter counters.
#[inline]
pub(crate) fn record_bloom_check(useful: bool) {
    if !is_enabled() {
        return;
    }
    PERF_STATE.with(|cell| {
        let mut s = cell.borrow_mut();
        s.bloom_check_count += 1;
        if useful {
            s.bloom_useful_count += 1;
        }
    });
}

/// RAII guard that measures elapsed time between construction
/// and drop, then adds the result to a per-field accumulator on
/// the thread-local `PerfContext`. Zero-cost when the perf level
/// is below [`PerfLevel::EnableTime`]: `start` is `None` and
/// `Drop` does nothing.
#[must_use]
pub(crate) struct PerfTimer {
    /// Start reading in nanoseconds from the platform clock.
    start: Option<u64>,
    which: PerfTimerField,
}

/// Enum of fields the `PerfTimer` can write into. Using an enum
/// instead of a raw pointer / closure keeps the `no unsafe code`
/// invariant and the generated code is one match arm.
#[derive(Clone, Copy)]
pub(crate) enum PerfTimerField {
    GetFromMemtable,
    GetFromOutputFiles,
    WriteWal,
    WriteMemtable,
}

impl PerfTimer {
    #[inline]
    pub(crate) fn new(which: PerfTimerField) -> Self {
        // `None` covers both "not timing" and "this platform has no
        // monotonic clock". Either way the scope records nothing
        // rather than a fabricated zero.
        let start = if is_timed() {
            crate::env::platform_nanos()
        } else {
            None
        };
        Self { start, which }
    }
}

impl Drop for PerfTimer {
    #[inline]
    fn drop(&mut self) {
        let Some(start) = self.start else {
            return;
        };
        let Some(now) = crate::env::platform_nanos() else {
            return;
        };
        let nanos = now.saturating_sub(start);
        PERF_STATE.with(|cell| {
            let mut s = cell.borrow_mut();
            match self.which {
                PerfTimerField::GetFromMemtable => s.get_from_memtable_time_nanos += nanos,
                PerfTimerField::GetFromOutputFiles => s.get_from_output_files_time_nanos += nanos,
                PerfTimerField::WriteWal => s.write_wal_time_nanos += nanos,
                PerfTimerField::WriteMemtable => s.write_memtable_time_nanos += nanos,
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_counters_stay_zero() {
        PerfContext::set_level(PerfLevel::Disable);
        PerfContext::reset();
        record_get_call();
        record_write_call();
        record_block_cache_lookup(true);
        record_bloom_check(false);
        {
            let _t = PerfTimer::new(PerfTimerField::GetFromMemtable);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let snap = PerfContext::capture();
        assert_eq!(snap.get_count, 0);
        assert_eq!(snap.write_count, 0);
        assert_eq!(snap.block_cache_lookup_count, 0);
        assert_eq!(snap.bloom_check_count, 0);
        assert_eq!(snap.get_from_memtable_time_nanos, 0);
    }

    #[test]
    fn enable_count_bumps_counters_without_timing() {
        PerfContext::set_level(PerfLevel::EnableCount);
        PerfContext::reset();
        record_get_call();
        record_get_call();
        record_block_cache_lookup(true);
        record_block_cache_lookup(false);
        {
            let _t = PerfTimer::new(PerfTimerField::GetFromMemtable);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let snap = PerfContext::capture();
        assert_eq!(snap.get_count, 2);
        assert_eq!(snap.block_cache_lookup_count, 2);
        assert_eq!(snap.block_cache_hit_count, 1);
        // Timing is off, so the memtable field stayed 0.
        assert_eq!(snap.get_from_memtable_time_nanos, 0);
        PerfContext::set_level(PerfLevel::Disable);
    }

    #[test]
    fn enable_time_populates_time_fields() {
        PerfContext::set_level(PerfLevel::EnableTime);
        PerfContext::reset();
        {
            let _t = PerfTimer::new(PerfTimerField::WriteWal);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let snap = PerfContext::capture();
        assert!(
            snap.write_wal_time_nanos >= 1_000_000,
            "expected at least 1 ms of WAL time, got {}",
            snap.write_wal_time_nanos
        );
        PerfContext::set_level(PerfLevel::Disable);
    }

    /// Work done for another thread is counted at that thread's level, apart
    /// from this thread's own counters, which come back untouched, and the
    /// other thread adds it to its own.
    #[test]
    fn work_done_on_behalf_of_another_thread_lands_on_that_thread() {
        PerfContext::set_level(PerfLevel::Disable);
        PerfContext::reset();
        let (out, counted) = on_behalf(PerfLevel::EnableCount, || {
            record_block_cache_lookup(true);
            record_bloom_check(false);
            7
        });
        assert_eq!(out, 7);
        assert_eq!(counted.block_cache_lookup_count, 1);
        assert_eq!(counted.block_cache_hit_count, 1);
        assert_eq!(counted.bloom_check_count, 1);
        assert_eq!(PerfContext::level(), PerfLevel::Disable);
        assert_eq!(PerfContext::capture(), PerfContextSnapshot::default());

        PerfContext::set_level(PerfLevel::EnableCount);
        PerfContext::reset();
        record_get_call();
        let (_, nothing) = on_behalf(PerfLevel::Disable, record_get_call);
        assert_eq!(nothing, PerfContextSnapshot::default());
        assert_eq!(PerfContext::capture().get_count, 1, "own counters kept");
        absorb(&counted);
        let snap = PerfContext::capture();
        assert_eq!(snap.get_count, 1);
        assert_eq!(snap.block_cache_lookup_count, 1);
        assert_eq!(snap.bloom_check_count, 1);
        PerfContext::set_level(PerfLevel::Disable);
    }

    #[test]
    fn level_round_trip() {
        let prev = PerfContext::set_level(PerfLevel::EnableCount);
        assert_eq!(PerfContext::level(), PerfLevel::EnableCount);
        PerfContext::set_level(prev);
        assert_eq!(PerfContext::level(), prev);
    }

    #[test]
    fn reset_clears_every_field() {
        PerfContext::set_level(PerfLevel::EnableCount);
        PerfContext::reset();
        record_get_call();
        record_bloom_check(true);
        PerfContext::reset();
        let snap = PerfContext::capture();
        assert_eq!(snap.get_count, 0);
        assert_eq!(snap.bloom_check_count, 0);
        PerfContext::set_level(PerfLevel::Disable);
    }
}
