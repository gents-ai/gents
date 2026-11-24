//! The commit path: how a write or a transaction's commit becomes durable,
//! applied and visible.
//!
//! Every commit that writes goes through the lock-free pipeline
//! (`pipeline.rs`, plan 4.7): it takes a slot of the commit ring with one
//! atomic add, and from
//! there whichever thread reaches the slot decides it, applies it, writes
//! its record, syncs the log and publishes it. A committer helps every
//! commit ahead of its own and never waits for another thread to be
//! scheduled; a blocking call waits only for device I/O under way.
//!
//! A blocking call that finds a round, an ordered unit or the version
//! store's step claimed by another thread waits for that thread, so one
//! descheduled for good while it holds such a claim holds up the blocking
//! commits behind it. A queue's poll never waits for another thread: the
//! job it steps parks instead (`park.rs`). A unit holds the version store's
//! step and the flush exclusion for the length of its body: a poll that
//! needs either parks until the body ends, and a blocking call waits for it.
//!
//! The calls that block on another thread by their contract, and so are
//! never steps of a job: a blocking write or commit (for the round, a unit
//! or the step, as above, and a commit for a write stall to clear), `close`
//! (for the commits and the background work in flight), and
//! `wait_for_snapshots` (for the last snapshot to be released).
//!
//! A transaction that writes nothing takes no slot at any level: its commit
//! is checked at the published horizon on the committing thread
//! (`RegolithEngine::commit_write_free`, plan 3.8, E50).
//!
//! An optimistic transaction's commit is a slot like any write (E10). It is
//! decided in ring order against the view, which by then holds every commit
//! decided before it (`NoLostUpdate` in `CommitPipeline.tla`,
//! `later_slot_aborts` in `PipelineCheck.lean`), and only a commit that
//! validates takes sequences and a record. Each commit learns its own outcome.
//!
//! Commits whose records are in the log together share one sync: a sync
//! round covers every record written when it starts, so many concurrent
//! durable writers cost far fewer syncs than writes. A thread decides,
//! writes and publishes only slots at or below its own, each at most once,
//! so no writer serves an unbounded queue of others (E21).
//!
//! Three invariants outrank throughput here, and every design choice in this
//! module is subordinate to them.
//!
//! * **the lost-update fix - the horizon trails durability.** `visible_seq`
//!   passes a commit only after its record is in the log, on stable storage
//!   when it asked for `Immediate`, and its operations are in the memtable,
//!   and always before its writer returns. Commits are published in ring
//!   order, so a snapshot cannot observe a later commit without every
//!   earlier one, nor part of a batch.
//! * **G2 - a failure fails every unfinished commit as one unit.** When a
//!   log write or sync fails, every commit not yet published is discarded:
//!   its record is cut out of the log, its operations out of the memtable,
//!   nothing of it is published, and its writer receives the error
//!   (`ordered.rs`). No writer can believe it committed when the sync did
//!   not. Commits published before the failure are untouched.
//! * **G3 - an abandoned writer cannot wedge the ring.** The ring owns a
//!   reference to every installed descriptor and the request is moved into
//!   it, so a helper never touches a frame that may have unwound; and a
//!   slot reserved by a thread that never installed is voided by the first
//!   thread that reaches it.
//!
//! An ingest takes its sequence in the ordered step and holds it until its
//! table is installed and the sequence published, so no commit is published
//! past an ingest.

use std::io;
use std::sync::Arc;

use super::{CommitOutcome, DurabilityMode, RegolithEngine, ValidationSet, grouped_batch_ops};
use crate::io_queue::wait::take_would_block;
use crate::statistics::Ticker;
use crate::sync::internal::Arc as RingArc;
use crate::{Conflict, IoWait, WouldBlock, WriteBatchOp};

mod allocate;
mod append;
mod content;
mod counter;
mod counters;
mod descriptor;
mod early;
mod families;
pub(crate) mod hook;
#[cfg(loom)]
pub mod loom_model;
#[cfg(loom)]
pub mod loom_turn_wait;
#[cfg(loom)]
pub mod loom_write_unit;
mod ordered;
mod park;
mod pipeline;
mod range_rule;
mod read_mode;
mod read_rules;
mod replaced;
mod request;
mod resume;
mod ring;
mod round;
mod spare;
mod stall;
mod terminator;
mod turn;
mod turn_wait;
mod txn;
mod write_check;
mod write_free;
mod write_unit;

use super::wal::MAX_RECORD_LEN;
pub(crate) use append::PendingAppend;
pub(crate) use counters::Counters;
pub(crate) use descriptor::After;
use descriptor::{Descriptor, Work};
use early::{EarlyProgress, EarlyVerdict};
pub(crate) use ordered::{Ordered, OrderedTicket};
pub(crate) use park::{Help, Needs};
pub(crate) use pipeline::Pipeline;
use replaced::Replaced;
pub(crate) use request::WriteRequest;
pub(crate) use stall::StallSignal;
use terminator::Landed;
pub(crate) use txn::Settled;
use txn::TxnRequest;
pub(crate) use write_check::EarlyWrite;

/// Bytes of record buffer a thread keeps between commits at the least.
///
/// A thread stages each record it writes in a buffer of its own, reserved to
/// the record's exact framed length and kept between commits up to a byte
/// ceiling, so a stream of records at or under this size stages its bytes
/// without touching the allocator once warmed up. See [`trim_stage`].
const MAX_GROUP_BYTES: usize = 1024 * 1024;

/// Ceiling on how much stage capacity `trim_stage` ever keeps.
///
/// Without a ceiling a single one-off giant record parks its whole peak
/// allocation for as long as the thread lives and stays idle. 16x the floor
/// still leaves a steady `BatchWrite`/4KiB stream (about 4.1 MB)
/// allocation-free; only a record larger than this is trimmed back down to
/// it in the same call that wrote it.
const MAX_KEPT_STAGE_BYTES: usize = 16 * MAX_GROUP_BYTES;

/// Slots of the commit ring, sized from the machine's parallelism and
/// clamped so a huge core count cannot balloon it. It bounds how many
/// commits may be decided and not yet published at once; a commit that
/// finds it full brings the oldest of them through first.
pub(crate) fn commit_ring_capacity() -> usize {
    crate::per_thread::parallelism()
        .saturating_mul(4)
        .clamp(256, 1024)
}

/// An optimistic transaction's commit, as the transaction hands it over.
pub(crate) struct TxnCommit {
    pub(crate) checks: ValidationSet,
    pub(crate) point_ops: std::collections::BTreeMap<Vec<u8>, Option<Vec<u8>>>,
    pub(crate) range_deletes: Vec<(Vec<u8>, Vec<u8>)>,
    pub(crate) merges: Vec<(Vec<u8>, Vec<u8>)>,
    pub(crate) appends: Vec<PendingAppend>,
    pub(crate) durability: DurabilityMode,
    /// The queue of the `CacheOnly` handle a `commit_nowait` was made
    /// through: the commit's validation reads then never read the device.
    /// `None` for every other commit.
    pub(crate) read_queue: Option<crate::QueueId>,
}

/// A transaction's commit that passed every check that may refuse it, its
/// early check still to run.
struct Unchecked {
    checks: ValidationSet,
    progress: Option<EarlyProgress>,
    ops: Vec<WriteBatchOp>,
    appends: Vec<PendingAppend>,
    durability: DurabilityMode,
    read_queue: Option<crate::QueueId>,
    /// It carries a write: it passes the write-stall admission before it
    /// takes a slot.
    writes: bool,
}

/// A commit checked and ready for the pipeline, or decided before it.
enum Requested {
    /// The early check found a conflict: nothing is queued.
    Decided(CommitOutcome),
    /// The request the pipeline decides, and whether it carries a write: a
    /// commit that does passes the write-stall admission before it takes a
    /// slot.
    Queued { request: WriteRequest, writes: bool },
    /// The early check, run through a `CacheOnly` handle, needs a block the
    /// cache does not hold. Nothing of the commit was taken; `wait` is the
    /// read, recorded on the handle's queue.
    Owed { commit: OwedCommit, wait: IoWait },
}

/// What `commit_nowait` learns from the pipeline.
pub(crate) enum NowaitCommit {
    /// The outcome: decided, published or failed.
    Done(CommitOutcome),
    /// Decided, not yet published: it waits for a sync, for a unit ahead of
    /// it, or for the write unit another thread holds to let go before its
    /// record is written. The outcome is in the commit once it is told.
    Pending(PendingCommit),
    /// Its early check needs a block the cache does not hold: nothing of it
    /// was taken. Once `wait` is ready, [`RegolithEngine::resubmit_nowait`]
    /// checks it again and goes on (`NowaitMiss.tla`).
    Owed { commit: OwedCommit, wait: IoWait },
}

/// A `commit_nowait` commit whose early check missed, waiting to be checked
/// again: a transaction's commit is made once, so a miss never ends it.
/// Boxed, so only a miss pays for it and the answers that carry it stay the
/// size they were.
pub(crate) struct OwedCommit(Box<Unchecked>);

/// A commit made by `commit_nowait` that is not published yet.
pub(crate) struct PendingCommit {
    desc: RingArc<Descriptor>,
}

impl PendingCommit {
    /// Tell `waiter` when the commit lands (it is visible to readers, and
    /// durable at `Immediate`, or it was discarded), on the thread that
    /// lands it. `false` when it already landed: the caller tells `waiter`.
    pub(crate) fn listen(&self, waiter: Arc<dyn super::io::job::Delivery>) -> bool {
        self.desc.listen(waiter)
    }

    /// What the commit learned, once it was told it is published or
    /// discarded, counted in `stats` as a write when it landed. Called once.
    pub(crate) fn settled(&self, stats: Option<&crate::Statistics>) -> io::Result<Settled> {
        RegolithEngine::settled(stats, &self.desc)
    }
}

impl std::fmt::Debug for PendingCommit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingCommit")
            .field("slot", &self.desc.index())
            .finish()
    }
}

/// Give staging memory back when neither of the last two records needed it,
/// and never keep more than `MAX_KEPT_STAGE_BYTES` regardless.
///
/// Policy: within that ceiling, the stage keeps enough capacity for the
/// larger of the record that just ran and the one before it, and never less
/// than `MAX_GROUP_BYTES`. A record at or under the ceiling touches the
/// allocator only when it needs to grow past the kept capacity, or when
/// the record two back was the largest of the three and is now shed, so a
/// stream of equal-sized records at or under the ceiling is allocation-free
/// once warmed up, and a stage the previous record grew is not shrunk by a
/// small record that happens to follow it (the common interleave of a bulk
/// batch and a single put) until two smaller records have run. A record
/// larger than the ceiling is trimmed straight back down to it in the same
/// call that ran it, so a one-off giant record can never park more than
/// `MAX_KEPT_STAGE_BYTES` for the rest of the engine's life, whether writes
/// go idle or the engine closes; the cost is that a steady stream of records
/// above the ceiling reallocates on every record instead of holding its
/// peak. Bound, in bytes: on return, `stage.capacity() <=
/// max(MAX_GROUP_BYTES, min(max(prev_staged, staged), MAX_KEPT_STAGE_BYTES))`,
/// which is itself at most `MAX_KEPT_STAGE_BYTES`.
///
/// `shrink_to` cannot go below `len`, so the bytes have to go first.
fn trim_stage(stage: &mut Vec<u8>, prev_staged: usize, staged: usize) {
    let keep = MAX_GROUP_BYTES.max(prev_staged.max(staged).min(MAX_KEPT_STAGE_BYTES));
    if stage.capacity() > keep {
        stage.clear();
        stage.shrink_to(keep);
    }
}

impl RegolithEngine {
    /// Apply an ordered batch of writes atomically.
    ///
    /// Operations are assigned consecutive sequence numbers in the order
    /// the caller recorded them. That order matters when a batch mixes
    /// range tombstones with puts, deletes and merges for keys inside the
    /// range.
    ///
    /// `durability` controls WAL fsync semantics; `disable_wal` skips the
    /// WAL entirely, so the caller accepts that a crash before the next
    /// memtable flush loses the write.
    pub(crate) fn apply_batch(
        &self,
        ops: Vec<WriteBatchOp>,
        durability: DurabilityMode,
        disable_wal: bool,
    ) -> io::Result<u64> {
        self.validate_ops_sizes(&ops, disable_wal)?;
        self.submit_batch(ops, durability, disable_wal)
    }

    /// [`Self::apply_batch`] for a caller that has already checked every
    /// key and value against the configured limits and the write's log
    /// record against the record limit. `Db::write` does so at the API
    /// boundary, before the stall wait and the statistics, so repeating
    /// the pass here would compare the same lengths against the same
    /// numbers a second time.
    pub(crate) fn submit_batch(
        &self,
        ops: Vec<WriteBatchOp>,
        durability: DurabilityMode,
        disable_wal: bool,
    ) -> io::Result<u64> {
        self.ensure_writable()?;
        if ops.is_empty() {
            return Ok(self.visible_seq.visible());
        }
        self.submit(WriteRequest::Batch {
            ops,
            durability,
            disable_wal,
        })
    }

    /// Apply grouped writes from internal callers that do not preserve a
    /// single operation log.
    pub(crate) fn apply_grouped_batch(
        &self,
        point_ops: std::collections::BTreeMap<Vec<u8>, Option<Vec<u8>>>,
        range_deletes: Vec<(Vec<u8>, Vec<u8>)>,
        merges: Vec<(Vec<u8>, Vec<u8>)>,
        durability: DurabilityMode,
        disable_wal: bool,
    ) -> io::Result<u64> {
        let ops = grouped_batch_ops(point_ops, range_deletes, merges);
        self.apply_batch(ops, durability, disable_wal)
    }

    /// Fast path for a single put: skips the batch vector so the most
    /// common write allocates nothing beyond the key and value the caller
    /// already owns.
    pub(crate) fn apply_single_put(
        &self,
        key: Vec<u8>,
        value: Vec<u8>,
        durability: DurabilityMode,
        disable_wal: bool,
    ) -> io::Result<u64> {
        self.ensure_writable()?;
        self.validate_prefixed_key_size(&key)?;
        self.validate_value_size(&value)?;
        let request = WriteRequest::Put {
            key,
            value,
            durability,
            disable_wal,
        };
        request.check_record_len(MAX_RECORD_LEN as usize)?;
        self.submit(request)
    }

    /// Attempt to commit an optimistic transaction's buffered writes.
    ///
    /// The commit takes a slot of the ring plain writes use (E10) and shares
    /// their sync rounds. Its slot is decided in ring order against the
    /// view, which holds the writes of every slot before it (`txn.rs`), so
    /// it gets the verdict committing them one at a time would give it. A
    /// conflict, and every failure of its own check, are its own; a log
    /// write or sync that fails fails every commit not yet published, a
    /// transaction's as a plain write's.
    pub(crate) fn commit_optimistic(
        &self,
        checks: ValidationSet,
        point_ops: std::collections::BTreeMap<Vec<u8>, Option<Vec<u8>>>,
        range_deletes: Vec<(Vec<u8>, Vec<u8>)>,
        merges: Vec<(Vec<u8>, Vec<u8>)>,
        appends: Vec<PendingAppend>,
        durability: DurabilityMode,
    ) -> io::Result<CommitOutcome> {
        let txn = TxnCommit {
            checks,
            point_ops,
            range_deletes,
            merges,
            appends,
            durability,
            read_queue: None,
        };
        match self.check_request(self.unchecked(txn)?)? {
            Requested::Decided(outcome) => Ok(outcome),
            Requested::Queued { request, writes } => {
                // A blocking commit waits a stop out here, before it takes a
                // slot: the transaction cannot be committed again, so it is
                // not handed the stall's wait as a plain write is.
                if writes {
                    self.admit_commit().map_err(crate::Error::into_io_error)?;
                }
                self.conclude(self.submit_settled(request)?)
            }
            // A miss of a `CacheOnly` scope this call runs in: its caller is
            // handed the wait, as before the commit took anything.
            Requested::Owed { wait, .. } => Err(wait.into_io_error()),
        }
    }

    /// [`Self::commit_optimistic`] for `commit_nowait`: returns once the
    /// commit is decided and written, or at the first unit in its way. When
    /// it is not published yet (it waits for a write stall to clear, for its
    /// sync, for a unit ahead of it, for room in a full ring, or for the
    /// device to decide its slot), [`NowaitCommit::Pending`] holds it;
    /// [`Self::await_commit`] then leaves it on the committer's queue. When
    /// its early check missed, [`NowaitCommit::Owed`] holds it. The caller
    /// never waits here, and with `txn.read_queue` set never reads the
    /// device.
    pub(crate) fn commit_optimistic_nowait(&self, txn: TxnCommit) -> io::Result<NowaitCommit> {
        let _scope = txn.read_queue.map(super::io::scope::cache_only);
        self.submit_nowait(self.unchecked(txn)?)
    }

    /// Submit `owed` again once the read its early check waited for is done:
    /// the unfinished early check resumes at its original horizon
    /// (`ValidationProgress.tla`, `PipelineCheck.early_check_cover`), and the
    /// commit goes on as [`Self::commit_optimistic_nowait`], owed if another
    /// block is missing. `on_device`: its queue is gone and polls no more, so
    /// it reads the device on this thread, as a blocking commit does. The
    /// database may have closed or latched meanwhile, which refuses it as it
    /// refuses a commit made now.
    pub(crate) fn resubmit_nowait(
        &self,
        owed: OwedCommit,
        on_device: bool,
    ) -> io::Result<NowaitCommit> {
        self.ensure_writable()?;
        let mut unchecked = *owed.0;
        let _scope = match unchecked.read_queue.filter(|_| !on_device) {
            Some(queue) => super::io::scope::cache_only(queue),
            None => {
                unchecked.read_queue = None;
                super::io::scope::blocking()
            }
        };
        self.submit_nowait(unchecked)
    }

    /// The early check of `unchecked`, then the slot: what
    /// [`Self::commit_optimistic_nowait`] and [`Self::resubmit_nowait`]
    /// share.
    fn submit_nowait(&self, unchecked: Unchecked) -> io::Result<NowaitCommit> {
        let (request, writes) = match self.check_request(unchecked)? {
            Requested::Decided(outcome) => return Ok(NowaitCommit::Done(outcome)),
            Requested::Queued { request, writes } => (request, writes),
            Requested::Owed { commit, wait } => return Ok(NowaitCommit::Owed { commit, wait }),
        };
        let pipe = self.pipe()?;
        let desc = spare::descriptor(Work::Request(request), false);
        if !writes {
            // It takes no capacity, so no stall holds it.
            desc.mark(descriptor::ADMITTED);
        }
        let published = self.advance_nowait(pipe, &desc).is_ok();
        self.run_owed_step();
        if published {
            let settled = Self::settled(self.statistics(), &desc);
            spare::keep_descriptor(desc);
            return self.conclude(settled?).map(NowaitCommit::Done);
        }
        Ok(NowaitCommit::Pending(PendingCommit { desc }))
    }

    /// Leave `pending` on `queue`: `told` is delivered there, once, by the
    /// queue's own poll, when the commit is published or discarded (D53).
    /// Until then the queue holds the unit the commit waits on, which its
    /// owner's poll claims and runs when nobody has. A queue that was dropped
    /// is told here instead, after the commit was brought through on this
    /// thread. Called on the queue owner's thread.
    pub(crate) fn await_commit(
        &self,
        pending: &PendingCommit,
        queue: &Arc<super::io::shared::QueueShared>,
        told: Arc<dyn super::io::job::Delivery>,
    ) {
        self.resume_nowait(&pending.desc, queue, &told);
    }

    /// Check what may refuse a commit (the handle, the sizes) before its
    /// early check. The write-stall admission comes after the early check, in
    /// the caller: [`RegolithEngine::admit_commit`] for a commit that blocks,
    /// [`RegolithEngine::admit_commit_nowait`] when a `commit_nowait` commit
    /// is about to take its slot.
    #[inline]
    fn unchecked(&self, txn: TxnCommit) -> io::Result<Unchecked> {
        let TxnCommit {
            checks,
            point_ops,
            range_deletes,
            merges,
            appends,
            durability,
            read_queue,
        } = txn;
        self.ensure_writable()?;
        let ops = grouped_batch_ops(point_ops, range_deletes, merges);
        // The sizes are refused here, before the commit takes a slot: the
        // record with every append counted at the bound its layout declares.
        self.validate_ops_sizes(&ops, false)?;
        if !appends.is_empty() {
            self.validate_append_sizes(&ops, &appends)?;
        }

        // A commit that writes passes the write-stall admission, in the
        // order a plain write does: closed/WAL-failed/read-only above, then
        // size validation above, then the stall, and only then the commit
        // pipeline. Never inside it: relieving a stop runs a compaction pass
        // on this thread, and that pass must not run in the pipeline's
        // ordered step. A commit that writes nothing (`ops` empty, e.g. a
        // `get_for_update` with no write) takes no capacity and is not held,
        // though its conflict check still takes a slot like any other
        // commit. An append is a write. The early check (`check_request`)
        // comes first: a conflict that is already certain needs no capacity,
        // and returns without waiting for a stall.
        let writes = !ops.is_empty() || !appends.is_empty();
        Ok(Unchecked {
            checks,
            progress: None,
            ops,
            appends,
            durability,
            read_queue,
            writes,
        })
    }

    /// Run the early check of `unchecked` and turn it into the request the
    /// pipeline decides, into the outcome the check already decided, or,
    /// when the check needs a block a `CacheOnly` scope may not read, into
    /// the commit owed that read.
    #[inline]
    fn check_request(&self, mut unchecked: Unchecked) -> io::Result<Requested> {
        // Checked up to the horizon here, on this thread and before the
        // commit takes a slot; deciding the slot checks only what landed
        // above it.
        let early = match self.check_early_with_progress(
            &unchecked.checks,
            &unchecked.ops,
            &mut unchecked.progress,
        ) {
            Ok(EarlyVerdict::Conflict(conflict)) => {
                return Ok(Requested::Decided(CommitOutcome::Conflict(conflict)));
            }
            Ok(EarlyVerdict::Marks(early)) => early,
            Err(err) => {
                return match take_would_block(err) {
                    Ok(WouldBlock::Io(wait)) => Ok(Requested::Owed {
                        commit: OwedCommit(Box::new(unchecked)),
                        wait,
                    }),
                    Ok(other) => Err(io::Error::new(io::ErrorKind::WouldBlock, other)),
                    Err(err) => Err(err),
                };
            }
        };
        let Unchecked {
            checks,
            progress: _,
            ops,
            appends,
            durability,
            read_queue,
            writes,
        } = unchecked;
        Ok(Requested::Queued {
            request: WriteRequest::Txn(Box::new(TxnRequest {
                checks,
                ops,
                appends,
                durability,
                perf: crate::PerfContext::level(),
                early,
                read_queue,
            })),
            writes,
        })
    }

    /// What a transaction's commit learns from its landed group, as the
    /// commit's outcome (see [`conclude_settled`]).
    pub(crate) fn conclude(&self, settled: Settled) -> io::Result<CommitOutcome> {
        conclude_settled(self.statistics(), settled)
    }
}

/// What a transaction's commit learns once its slot is published, as the
/// commit's outcome: the perf deciding the slot counted for it absorbed into
/// this thread's context, and the counts its validation made recorded in
/// `stats`. Called on the committing thread, or on the thread that delivers
/// its ticket.
pub(crate) fn conclude_settled(
    stats: Option<&crate::Statistics>,
    settled: Settled,
) -> io::Result<CommitOutcome> {
    {
        match settled {
            Settled::Committed {
                seq,
                merges_commuted,
                writes_elided,
                perf,
            } => {
                if let Some(counted) = perf {
                    crate::perf_context::absorb(&counted);
                }
                // Counted while validating, recorded here, off the commit
                // pipeline and only once the commit landed.
                if let Some(s) = stats {
                    if merges_commuted > 0 {
                        s.add(Ticker::PolicyBlindMergesCommuted, merges_commuted);
                    }
                    if writes_elided > 0 {
                        s.add(Ticker::CommitWritesElided, writes_elided);
                    }
                }
                Ok(CommitOutcome::Ok { seq })
            }
            Settled::Conflict { conflict, perf } => {
                if let Some(counted) = perf {
                    crate::perf_context::absorb(&counted);
                }
                Ok(CommitOutcome::Conflict(conflict))
            }
            Settled::Write(_) => Err(io::Error::other(
                "a transaction's commit settled as a plain write",
            )),
        }
    }
}

impl RegolithEngine {
    /// Hand a plain write to the commit pipeline and block until it is
    /// durable and applied, or until the pipeline discards it. Returns the
    /// last sequence its operations took.
    fn submit(&self, request: WriteRequest) -> io::Result<u64> {
        self.submit_settled(request).and_then(Settled::into_seq)
    }

    /// Hand `request` to the commit pipeline and block until it is
    /// published, or until the pipeline discards it, and return what was
    /// decided for it. Then, on a database with no worker, run the bounded
    /// step of background work the writes left owing, if any, now that this
    /// commit holds nothing of the pipeline.
    fn submit_settled(&self, request: WriteRequest) -> io::Result<Settled> {
        let settled = self.commit_through_pipeline(request);
        self.run_owed_step();
        settled
    }

    /// Tell the registered listeners a commit lost a race. Called by the
    /// committing thread once its commit is settled, never while a slot is
    /// being decided.
    pub(crate) fn notify_conflict(&self, conflict: &Conflict) {
        crate::event_listener::dispatch(&self.options.listeners, |l| l.on_conflict(conflict));
    }

    /// Tell the registered listeners a transaction callback panicked after its
    /// transaction's outcome was decided.
    pub(crate) fn notify_callback_panic(&self, callback: &'static str) {
        crate::event_listener::dispatch(&self.options.listeners, |l| l.on_callback_panic(callback));
    }

    /// Tell the registered listeners a write-ahead-log operation failed.
    /// `Err` is a listener's panic, caught because this runs in the ordered
    /// step.
    pub(super) fn notify_wal_error(&self, cause: &io::Error) -> io::Result<()> {
        if self.options.listeners.is_empty() {
            return Ok(());
        }
        let err = crate::Error::from(crate::Error::clone_io(cause));
        crate::event_listener::dispatch_contained(&self.options.listeners, |l| {
            l.on_background_error(
                crate::event_listener::BackgroundErrorReason::WriteAheadLog,
                &err,
            )
        })
        .map_err(crate::Error::into_io_error)
    }
}

#[cfg(test)]
mod append_tests;

#[cfg(test)]
mod exempt_tests;

#[cfg(test)]
mod early_tests;

#[cfg(test)]
mod limit_tests;

#[cfg(test)]
mod landing_tests;

#[cfg(test)]
mod park_tests;

#[cfg(test)]
mod pipeline_order_tests;

#[cfg(test)]
mod overlap_tests;

#[cfg(test)]
mod txn_overlap_tests;

#[cfg(test)]
mod written_tests;

#[cfg(test)]
mod pipeline_tests;

#[cfg(test)]
mod write_unit_tests;

#[cfg(test)]
mod turn_wait_tests;

#[cfg(test)]
mod allocate_tests;

#[cfg(test)]
mod write_free_tests;

#[cfg(test)]
mod write_free_window_tests;

#[cfg(test)]
mod helping_tests;
#[cfg(all(test, target_os = "linux"))]
mod late_copy_tests;
