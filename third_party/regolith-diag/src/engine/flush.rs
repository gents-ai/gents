//! Writing a frozen memtable out to an L0 table, by whoever flushes it.
//!
//! A [`Flusher`] holds what a flush touches (the read view, the version set,
//! the table directory, the options, the background health) and nothing of
//! the engine around it. The engine and its compaction workers share one, so
//! a worker flushes what writers sealed (E9) without holding a handle on the
//! engine: dropping the last handle on a database closes it there and then,
//! and a worker never runs the engine's teardown.
//!
//! Every flush takes the `flushing` exclusion and the oldest frozen memtable,
//! so frozen memtables install in L0 in the order they were sealed, whichever
//! thread flushes (`LsmOrder.tla`, RED FlushAnyOrder). A flush installs its
//! table and retires its memtable in one publication of the read view, so no
//! view holds a memtable and its table at once (`LsmOrder.tla`,
//! `NoDoubleSource`, RED SplitRetire).

use std::path::PathBuf;
use std::sync::Arc;
use std::task::Waker;

use super::background_health::{BackgroundHealth, Job};
use super::callback::{self, InBackground};
use super::commit::StallSignal;
use super::io::parked::Parked;
use super::io::scope;
use super::io::shared::thread_token;
use super::log_retirement::{self, RetiredLogs};
use super::manifest::VersionEdit;
use super::memtable::MemTable;
use super::pending_outputs::PendingOutputs;
use super::range_tombstone::RangeTombstoneSet;
use super::read_view::ReadViewCell;
use super::sstable::{LiveSst, SsTableMeta, SsTableReader, SsTableWriter, sst_filename};
use super::version_store::VersionStore;
use super::{EngineOptions, event_listener};
use crate::env::Env;
use crate::sync::internal::{AtomicUsize, Ordering, wait_here};
use crate::sync::{Mutex, MutexGuard};

/// What a flush needs, shared by the engine and its compaction workers.
pub(crate) struct Flusher {
    view: Arc<ReadViewCell>,
    versions: Arc<VersionStore>,
    sst_dir: PathBuf,
    /// The logs flushes have put in tables, and whether a removal is owed
    /// (`log_retirement.rs`). Every flush path runs through this flusher,
    /// so every one retires its log the same way.
    retired_logs: RetiredLogs,
    options: EngineOptions,
    env: Arc<dyn Env>,
    health: Arc<BackgroundHealth>,
    /// Refreshed after each flush a worker makes: a flush removes a memtable
    /// and adds an L0 file, both of which the stall thresholds count, and
    /// writers stopped on too many memtables are told once it cleared.
    stall_signal: Arc<StallSignal>,
    /// Serializes [`Flusher::flush_oldest`] against itself.
    ///
    /// The exclusion is not protecting shared state, which the read view
    /// already publishes atomically: it is what keeps L0 installs in the
    /// order the memtables were sealed. A writer seals under the pipeline
    /// mutex while a worker, a checkpoint's drain or a stopped writer flushes
    /// outside it, so all of them could be inside a flush at once. They
    /// would then both take `frozen.first()` as their victim and both retire
    /// it, and the second retirement would drop a memtable whose contents are
    /// in no published version.
    ///
    /// An ingest takes it only through `flush_until_retired`, for the
    /// memtables holding a key of its file's range, and holds the pipeline
    /// mutex until the file is installed. A memtable holding none of its keys
    /// may be flushed after it and land in front of it in L0, which changes no
    /// read: the two share no key (LsmOrder.tla, Lean `ingest_ordered`).
    ///
    /// A [`crate::sync::Mutex`]: one compare-and-swap to take when free, and
    /// no thread sleeps in the kernel on it. A step a write owes only tries
    /// it ([`Flusher::try_flushing`]); what must have a memtable flushed
    /// before it goes on (a worker, a job's drain, close, a rotation at the
    /// memtable cap) waits for the flush in flight on its own thread
    /// ([`Flusher::flushing`]). An ordered unit a queue's poll runs may not
    /// wait: the poll takes the exclusion before the unit's body begins, or
    /// parks ([`Flusher::hold_or_park`]). Built without spin (E48): it is
    /// held for a whole memtable flush, table writes and syncs included, so
    /// a caller that waits for it queues at once.
    flushing: Mutex<()>,
    /// The thread whose poll took the exclusion for a unit's body, as its
    /// thread token; zero otherwise. That thread's own calls for the
    /// exclusion, inside the body, find it held here and go on.
    held_by: AtomicUsize,
    /// The jobs that found the exclusion held and parked, woken each time
    /// it is released (`io::parked`, PK1).
    parked: Parked,
}

/// The flush exclusion, held: what [`Flusher::flush_oldest`] is called
/// with. Released when dropped, unless it only stands for a hold further up
/// this thread's stack.
pub(crate) struct Flushing<'a> {
    flusher: &'a Flusher,
    /// `None` when the exclusion is held further up this thread's stack, by
    /// the poll running the unit this call is part of.
    guard: Option<MutexGuard<'a, ()>>,
    /// The hold of [`Flusher::hold_or_park`], which `held_by` names.
    for_unit: bool,
}

impl Drop for Flushing<'_> {
    fn drop(&mut self) {
        let Some(guard) = self.guard.take() else {
            return;
        };
        if self.for_unit {
            self.flusher.held_by.store(0, Ordering::Release);
        }
        drop(guard);
        self.flusher.parked.released();
    }
}

impl Flusher {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        view: Arc<ReadViewCell>,
        versions: Arc<VersionStore>,
        sst_dir: PathBuf,
        retired_logs: RetiredLogs,
        options: EngineOptions,
        health: Arc<BackgroundHealth>,
        stall_signal: Arc<StallSignal>,
    ) -> Self {
        Self {
            view,
            versions,
            sst_dir,
            retired_logs,
            env: Arc::clone(&options.env),
            options,
            health,
            stall_signal,
            flushing: Mutex::new_without_spin(()),
            held_by: AtomicUsize::new(0),
            parked: Parked::new(),
        }
    }

    /// The exclusion as this thread's poll already holds it for the unit it
    /// is running, if it does.
    fn held_here(&self) -> Option<Flushing<'_>> {
        (self.held_by.load(Ordering::Acquire) == thread_token()).then_some(Flushing {
            flusher: self,
            guard: None,
            for_unit: false,
        })
    }

    fn held<'a>(&'a self, guard: MutexGuard<'a, ()>, for_unit: bool) -> Flushing<'a> {
        Flushing {
            flusher: self,
            guard: Some(guard),
            for_unit,
        }
    }

    /// The flush exclusion, waited for on this thread. Fails only on
    /// `wasm32`, when this same thread is already inside a flush.
    pub(crate) fn flushing(&self) -> std::io::Result<Flushing<'_>> {
        if let Some(held) = self.held_here() {
            return Ok(held);
        }
        scope::waits("the flush exclusion, taken without holding it");
        if let Some(guard) = self.flushing.try_lock() {
            return Ok(self.held(guard, false));
        }
        Ok(self.held(wait_here(self.flushing.lock())?, false))
    }

    /// The flush exclusion if no flush is in flight, without waiting.
    pub(crate) fn try_flushing(&self) -> Option<Flushing<'_>> {
        self.held_here()
            .or_else(|| Some(self.held(self.flushing.try_lock()?, false)))
    }

    /// The flush exclusion for the body of an ordered unit a queue's poll
    /// is about to run, which may not wait for it once it began. `None`
    /// when a flush is in flight: `waker` is then woken when the exclusion
    /// is next released, and the unit parks. Until the hold is dropped,
    /// this thread's own calls for the exclusion find it held and go on.
    pub(crate) fn hold_or_park(&self, waker: &Waker) -> Option<Flushing<'_>> {
        let guard = self
            .parked
            .take_or_park(waker, || self.flushing.try_lock())?;
        self.held_by.store(thread_token(), Ordering::Release);
        Some(self.held(guard, true))
    }

    /// Flush every frozen memtable, oldest first, as a background step: what
    /// a compaction worker runs before it compacts.
    ///
    /// A caller's code that panics here has no call to unwind into, so it
    /// fails the flush, as any failing flush does (`callback.rs`). A failure
    /// is recorded as background health and told to the listeners by the
    /// flush itself, and the memtable stays frozen for the next wake.
    pub(crate) fn flush_all_frozen(&self) {
        let _background = InBackground::enter();
        loop {
            // The worker may wait here on a writer's or a checkpoint's
            // flush, a whole table write. The wait releases the views its
            // last read holds back before it parks.
            let Ok(flushing) = self.flushing() else {
                break;
            };
            let flushed = self.flush_oldest(&flushing);
            drop(flushing);
            // One memtable fewer and maybe one L0 file more: writers cache
            // the level that leaves, and those a stop stopped are told once
            // it no longer holds.
            self.stall_signal.refresh();
            if !matches!(flushed, Ok(true)) {
                break;
            }
        }
    }

    /// Retire one frozen memtable in one publication. Called only once
    /// its contents are durable in an SSTable the published version
    /// already references, or once they proved to be empty.
    ///
    /// By identity, not by position. The memtable named here is the one
    /// this flush read, and between reading it and getting here the
    /// list can have changed: a rotation appends, and another flush
    /// could have retired ahead of this one. Dropping "index 0" would
    /// then drop somebody else's memtable, whose contents are in no
    /// published version. Retiring a memtable that is already gone is a
    /// no-op, which is what makes this safe to call on every exit path.
    fn retire(&self, flushed: &Arc<MemTable>) {
        self.view.retire_memtable(flushed);
    }

    /// Unlink the log that backed `flushed`, now that its records are in
    /// an SSTable the published version references, and any log a failed
    /// unlink left below it. A failure is reported and retried by the next
    /// flush or open; the log is never replayed meanwhile, because the
    /// table's batch recorded it as flushed (`log_retirement.rs`).
    ///
    /// Keyed off the memtable rather than off whatever log the caller
    /// happened to seal. A flush and the seal that fed it are not
    /// necessarily about the same memtable: the seal appends to the
    /// frozen list while the flush takes the front of it, and the two
    /// are separated by a whole SSTable write. Unlinking the caller's
    /// log would delete the only durable copy of a memtable that has not
    /// been flushed, and a crash would then lose every write in it.
    fn remove_sealed_wal(&self, flushed: &MemTable) {
        self.retired_logs
            .retire(&*self.env, flushed, self.options.statistics.as_deref());
    }

    /// Write the oldest frozen memtable out to an L0 SSTable and retire
    /// it. Returns `false` when there was nothing frozen to flush.
    ///
    /// The caller holds `flushing` and passes its guard, which is the
    /// only way to call this. Serialized against itself. The exclusion
    /// is not protecting shared state, which the read view already
    /// publishes atomically: it is what keeps L0 installs in the order
    /// the memtables were sealed, because the format gives an L0 file no
    /// sequence of its own and recency is install order. Two flushes
    /// racing would install a newer file under an older one, and a read
    /// that had seen the newer version would then see the older one.
    pub(crate) fn flush_oldest(&self, flushing: &Flushing<'_>) -> std::io::Result<bool> {
        match self.flush_oldest_inner(flushing) {
            Ok(flushed) => {
                self.health.record_success(Job::Flush);
                Ok(flushed)
            }
            Err(e) => {
                let hazard = self.health.record_failure(Job::Flush, &e);
                tracing::error!(error = %e, hazard = hazard.label(), "Flush failed");
                if !self.options.listeners.is_empty() {
                    let err = crate::Error::from(crate::Error::clone_io(&e));
                    crate::event_listener::dispatch_contained(&self.options.listeners, |l| {
                        l.on_background_error(
                            crate::event_listener::BackgroundErrorReason::Flush,
                            &err,
                        )
                    })
                    .map_err(crate::Error::into_io_error)?;
                }
                Err(e)
            }
        }
    }

    fn flush_oldest_inner(&self, _flushing: &Flushing<'_>) -> std::io::Result<bool> {
        // Through the env: a target with no monotonic clock reports
        // nothing measured rather than a fabricated duration.
        let flush_start = self.env.now_micros();
        let memtable = match self.view.load().frozen.first() {
            Some(mt) => Arc::clone(mt),
            None => return Ok(false),
        };

        let range_tombstones = memtable.clone_range_tombstones();

        if memtable.is_empty() && range_tombstones.is_empty() {
            self.retire(&memtable);
            self.remove_sealed_wal(&memtable);
            return Ok(true);
        }

        let file_id = self.versions.take_file_id()?;

        let sst_path = self.sst_dir.join(sst_filename(file_id));
        // Until the edit below is offered to the manifest, nothing else
        // knows this file exists: every early return must unlink it, or a
        // flush that keeps failing leaks a memtable-sized file per retry.
        let mut pending = PendingOutputs::new(Arc::clone(&self.env));
        pending.track(sst_path.clone());

        // Memtable flushes always land at L0 - pick L0's codec.
        let mut writer = SsTableWriter::new_in(
            &self.env,
            &sst_path,
            self.options.block_size,
            self.options.bloom_bits_per_key,
            self.options.compression_for_level(0),
            self.options.prefix_extractor.clone(),
            self.options.partitioned_index,
            self.options.metadata_block_size,
            self.options.keyring.as_deref(),
        )?;

        // The table carries the memtable's range tombstones, so the entries
        // they hide are counted with its deletions as they are written.
        if !range_tombstones.is_empty() {
            writer.hidden_by(Arc::new(RangeTombstoneSet::from_vec(
                range_tombstones.clone(),
            )));
        }

        // Walk the memtable in internal-key order and copy every version
        // and tombstone into the SSTable unchanged, preserving MVCC.
        // The walk streams straight out of the arena: a flush holds one
        // entry plus the block builder, never a second copy of the
        // whole memtable.
        let mut walk =
            || memtable.try_for_each_entry(|internal_key, value| writer.add(internal_key, value));
        // Only a prefix extractor runs caller code in the walk, so only then
        // is there a panic to catch.
        if self.options.prefix_extractor.is_some() {
            callback::contain("PrefixExtractor", walk).map_err(crate::Error::into_io_error)??;
        } else {
            walk()?;
        }

        // Persist range tombstones alongside the point entries.
        for rt in &range_tombstones {
            writer.add_range_tombstone(&rt.start, &rt.end, rt.seq);
        }

        let summary = match writer.finish()? {
            Some(s) => s,
            None => {
                self.retire(&memtable);
                self.remove_sealed_wal(&memtable);
                let _ = self.env.remove_file(&sst_path);
                return Ok(true);
            }
        };

        let file_size = self.env.metadata(&sst_path)?.len;
        let num_entries = summary.num_entries;

        // Throttle background I/O so bursts of flush writes don't
        // starve foreground traffic: on a worker only, never on a
        // caller's call. Rate-limiting is opt-in via
        // `Options::rate_limiter`; a `None` limiter is a no-op.
        crate::rate_limiter::throttle_background(self.options.rate_limiter.as_ref(), file_size)?;

        let reader = Arc::new(SsTableReader::open_with(
            &self.env,
            &sst_path,
            file_id,
            self.options.metadata_policy(),
            self.options.keyring.as_deref(),
        )?);
        let file = LiveSst::new(
            SsTableMeta {
                file_id,
                smallest_key: summary.smallest_user_key,
                largest_key: summary.largest_user_key,
                file_size,
                num_entries,
                global_seq: None,
                deletions: Some(summary.deletions),
            },
            reader,
            summary.newest_seq,
        );

        // The sequence this memtable was sealed at, not the engine's
        // current one. `last_seq` bounds table versions and names the
        // flushed prefix for checkpoints. A checkpoint copies tables
        // and no WAL, so stamping the global counter here would make a
        // checkpoint claim writes that are still only in a memtable it
        // did not flush.
        let seq = memtable
            .sealed_seq()
            .ok_or_else(|| std::io::Error::other("a memtable was flushed before it was sealed"))?;
        let mut edits = vec![
            VersionEdit::AddFile { level: 0, file },
            VersionEdit::SetLastSeq(seq),
        ];
        // In the table's batch, so the log is skipped by recovery exactly
        // when the table is durable (E30).
        edits.extend(log_retirement::min_wal_id_after(&memtable).map(VersionEdit::SetMinWalId));
        pending.offered_to_manifest();
        self.versions.run(|versions| -> std::io::Result<()> {
            versions.apply(&edits)?;
            // Retired in the publication of the version holding its table,
            // not after it: until that version is published the flushed data
            // lives in this memtable alone, and a view holding both would
            // apply the memtable's merge operands twice.
            versions.retire_with_version(&memtable);
            Ok(())
        })?;
        #[cfg(test)]
        if let Some(hook) = AFTER_FLUSH_INSTALL.with(|slot| slot.borrow_mut().take()) {
            hook();
        }
        self.remove_sealed_wal(&memtable);

        // Publish flush statistics before the listener dispatch
        // so callers that react to `on_flush_completed` can
        // already see the updated tickers.
        if let Some(s) = self.options.statistics.as_deref() {
            s.add(crate::statistics::Ticker::FlushCount, 1);
            s.add(crate::statistics::Ticker::FlushBytesWritten, file_size);
            if s.timed()
                && let Some(micros) = crate::env::elapsed_micros(&*self.env, flush_start)
            {
                s.record(crate::statistics::Histogram::FlushTime, micros);
            }
        }

        // Dispatch lifecycle events to any registered listeners.
        // Two callbacks fire per flush: `on_table_file_created`
        // for the new SSTable and `on_flush_completed` with
        // memtable-level aggregates.
        if !self.options.listeners.is_empty() {
            let (smallest, largest) = {
                // Version was just applied; pull the newly-added
                // file's metadata back out so listeners see the
                // exact bounds the engine committed.
                let ver = self.versions.current();
                if let Some(added) = ver.levels[0].iter().find(|f| f.meta.file_id == file_id) {
                    (
                        added.meta.smallest_key.clone(),
                        added.meta.largest_key.clone(),
                    )
                } else {
                    (Vec::new(), Vec::new())
                }
            };
            // Zero where the platform has no monotonic clock; the
            // `FlushJobInfo::duration` doc says so.
            let duration = std::time::Duration::from_micros(
                crate::env::elapsed_micros(&*self.env, flush_start).unwrap_or(0),
            );
            let create_info = event_listener::TableFileCreationInfo {
                file_id,
                file_path: sst_path.clone(),
                level: 0,
                reason: event_listener::TableFileCreationReason::Flush,
                file_size,
                num_entries,
            };
            let flush_info = event_listener::FlushJobInfo {
                file_id,
                file_path: sst_path.clone(),
                file_size,
                num_entries,
                smallest_key: smallest,
                largest_key: largest,
                duration,
            };
            // The table is installed and the memtable retired: a listener
            // that panics now cannot undo the flush, so it does not fail it
            // and latches nothing. Caught where callbacks are caught (a
            // background flush, or one a rotation runs under back-pressure)
            // and logged; an explicit flush still unwinds into its caller.
            let _contained = InBackground::where_contained();
            let told = event_listener::dispatch_contained(&self.options.listeners, |l| {
                l.on_table_file_created(&create_info)
            })
            .and_then(|()| {
                event_listener::dispatch_contained(&self.options.listeners, |l| {
                    l.on_flush_completed(&flush_info)
                })
            });
            if let Err(err) = told {
                tracing::error!(error = %err, file_id, "a listener failed after a flush completed");
            }
        }

        tracing::info!(
            file_id,
            entries = num_entries,
            size = file_size,
            "Flushed memtable to L0 SSTable"
        );

        Ok(true)
    }
}

#[cfg(test)]
std::thread_local! {
    /// Test seam: runs once, on this thread, right after the next flush
    /// published the version holding its table.
    static AFTER_FLUSH_INSTALL: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Test-only: run `hook` once, on this thread, right after the next flush on
/// it published the version holding its table.
#[cfg(test)]
pub(crate) fn after_next_flush_install(hook: impl FnOnce() + 'static) {
    AFTER_FLUSH_INSTALL.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
}

#[cfg(test)]
#[path = "flush_window_tests.rs"]
mod window_tests;
