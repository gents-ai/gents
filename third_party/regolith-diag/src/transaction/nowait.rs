//! `commit_nowait`: a commit that is decided and written, then hands back a
//! ticket instead of waiting for its fsync (plan 3.0, 3.16, 4.10; D43, D53).
//!
//! The commit runs exactly as [`Transaction::commit`] runs it up to the log
//! write: its `before_commit` callbacks, its validation when its slot of the
//! commit ring is decided, its record's write. What differs is the end:
//!
//! - With no queue (neither [`TxnOptions::io_queue`](super::TxnOptions::io_queue)
//!   nor a [`ReadMode::CacheOnly`](crate::ReadMode::CacheOnly) names one) the
//!   commit syncs inline, as `commit` does, and the ticket is ready.
//! - With a queue, a commit that waits for no fsync, or one decided without
//!   the log (a conflict, a failed callback), is published before the call
//!   returns, and the ticket is ready.
//! - With a queue, a commit that waits for an fsync is left waiting for it:
//!   the sync is one unit every commit it covers waits on, on each
//!   committer's own queue (`engine::commit::round`). The ticket completes
//!   on this transaction's queue once the commit is published: the outcome
//!   is resolved, and the transaction's callbacks run, at the poll that
//!   delivers it, on the queue's thread.
//! - Through a `CacheOnly` handle, a commit whose validation needs a block
//!   the cache does not hold is never ended by the miss: the transaction
//!   was consumed and cannot commit a second time. A miss before its slot
//!   leaves it owed on its queue, checked again at the poll that finds the
//!   block read (`Committed::Owed`); a miss deciding its slot leaves the
//!   decision on the queue, where the poll makes it with the device
//!   (`NowaitMiss.tla`). Either way the ticket completes on the queue.
//!
//! A pessimistic commit's key locks are released when the commit lands, by
//! the thread that lands it: the commit is applied and visible by then (and
//! durable at `Immediate`), so the next holder reads it, and no transaction
//! waits for this one's owner to poll its queue (`LockTable.tla`,
//! `IsolationAtLanding`).
//!
//! The transaction's snapshot stays pinned until the commit lands too, and
//! is let go by the same thread. A commit left behind a write stall has not
//! taken its slot, so the check of what landed above its early horizon is
//! still to come, and it reads every version above the snapshot. Unpinned,
//! a compaction meanwhile may retire a deletion newer than the snapshot
//! (there is no live snapshot below it), and the check would let through a
//! commit that conflicts with it.
//!
//! Exactly one outcome per transaction, whichever way it ends: the commit
//! claim is won here as `commit` wins it, and the callbacks of the outcome
//! run once, at return for a ticket ready then, else at delivery, through the
//! same function `commit` uses (`callbacks::Ending`).

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::{Arc, Weak};
use std::task::{Context, Poll, Waker};

use super::{Transaction, TransactionError, TxResult, policy, receipt_of};
use crate::engine::callback::InCommit;
use crate::engine::io::job::{Delivery, Job, JobBody, Step};
use crate::engine::io::scope;
use crate::engine::io::shared::QueueShared;
use crate::engine::take::Take;
use crate::engine::{
    CommitOutcome, NowaitCommit, OwedCommit, PendingCommit, RegolithEngine, SnapshotPin,
    ValidationSet, conclude_settled,
};
use crate::io_queue::completion::Completion;
use crate::io_queue::wait::copy_would_block;
use crate::{CommitReceipt, CommitTicket, Error, IoWait, QueueId, WouldBlock};

/// The shared half of a commit's ticket.
type Ticket = Completion<TxResult<CommitReceipt>>;

/// The snapshot pin of a commit that lands later, let go by whoever lands
/// it (see the module docs).
struct LandingSnapshot {
    engine: Weak<RegolithEngine>,
    /// Taken once: a `try_lock` that finds it held means the other caller is
    /// letting it go.
    pin: crate::sync::Mutex<SnapshotPin>,
}

impl LandingSnapshot {
    fn release(&self) {
        let Some(mut pin) = self.pin.try_lock() else {
            return;
        };
        let pin = pin.take();
        // A database that is gone took its snapshot registry with it.
        if let Some(engine) = self.engine.upgrade() {
            engine.release_snapshot(pin);
        }
    }
}

impl Delivery for LandingSnapshot {
    fn deliver(&self) {
        self.release();
    }
}

/// How far `commit_nowait` got before it returned.
enum Committed {
    /// The outcome is known now.
    Done(TxResult<CommitReceipt>),
    /// Not published yet: it waits for a write stall to clear before it
    /// takes its slot, or, decided and written, for its fsync. The outcome
    /// is in `pending` once it is delivered on `queue`.
    Pending {
        pending: PendingCommit,
        queue: Arc<QueueShared>,
        scan_runs_dropped: u64,
    },
    /// Its check needs a block the cache does not hold, and nothing of it
    /// was taken (`NowaitMiss.tla`). Boxed, so only a miss pays for it.
    Owed(Box<Owing>),
}

/// A commit owed a read: it waits on `queue` for `wait`, and is submitted
/// again there once the read is done.
struct Owing {
    owed: Owed,
    wait: IoWait,
    queue: Arc<QueueShared>,
    scan_runs_dropped: u64,
}

/// What an owed commit submits again.
enum Owed {
    /// A commit that writes, owed its early check.
    Writes(OwedCommit),
    /// A commit that writes nothing, owed its check; it takes no slot
    /// (`RegolithEngine::commit_write_free`).
    WriteFree {
        checks: ValidationSet,
        read_queue: Option<QueueId>,
    },
}

/// The read an owed commit waits for, as a job of the commit's queue: the
/// queue's poll steps it, and it parks until the queue finished the read.
struct ReadOwed(IoWait);

impl JobBody for ReadOwed {
    /// The read is the queue's to do, at its poll: nothing is left to run
    /// here, and the commit submitted again reports what it finds.
    fn run(self: Box<Self>) {}

    fn step(mut self: Box<Self>, waker: &Waker) -> Step {
        match Pin::new(&mut self.0).poll(&mut Context::from_waker(waker)) {
            Poll::Ready(()) => Step::Done,
            Poll::Pending => Step::Parked(self),
        }
    }

    fn release(self: Box<Self>) {}
}

/// An owed commit waiting for its read, told on its queue's thread when the
/// read is done, or on the thread that dropped the queue.
struct Resubmit {
    /// The transaction, resolved, its snapshot pinned and its locks held
    /// until the commit lands, and what it submits again.
    waiting: Take<(Transaction, Owed)>,
    claimed: bool,
    queue: Arc<QueueShared>,
    scan_runs_dropped: u64,
    completion: Arc<Ticket>,
}

impl Delivery for Resubmit {
    fn deliver(&self) {
        if let Some((txn, owed)) = self.waiting.take() {
            txn.resubmit(
                owed,
                self.claimed,
                &self.queue,
                self.scan_runs_dropped,
                &self.completion,
            );
        }
    }
}

impl Transaction {
    /// Commit without waiting for durability: decide and write the commit,
    /// then return a [`CommitTicket`] that completes once the commit is
    /// durable (at [`crate::DurabilityMode::Immediate`]) and visible. Every
    /// outcome comes through the ticket, a conflict included.
    ///
    /// The commit runs as [`Transaction::commit`] runs it until its record
    /// is written: [`Transaction::prepare`] first, then validation in the
    /// commit pipeline, in order with every concurrent commit and write. It
    /// never waits for the log's fsync: the sync is one unit shared by the
    /// queue of every commit it covers, which the first of their threads to
    /// poll runs, and the ticket completes on this transaction's queue when
    /// it lands. A commit made with a blocking call runs it itself. Nor does
    /// it wait for anything else under way, a memtable rotation or a full
    /// commit ring: the commit is left on its queue behind that and goes on
    /// at the poll that finds it done.
    ///
    /// A write stall does not end the commit either. A commit that meets a
    /// stop trigger has not taken its place in the pipeline yet: it is left
    /// on its queue behind the stall, nothing of it is stored, and at the
    /// poll that finds the stop relieved it goes on and commits, so its
    /// ticket completes then. On a database with no worker and
    /// [`crate::Options::inline_compaction`] on, the queue's polls run the
    /// steps that relieve the stop too. A commit waiting there is told what
    /// a write arriving then is told when the stop will not clear (the
    /// database closed, the engine latched, the relief keeps failing), as
    /// its outcome.
    ///
    /// - **The queue.** The one [`TxnOptions::io_queue`](super::TxnOptions::io_queue)
    ///   names, else the one this transaction's
    ///   [`ReadMode::CacheOnly`](crate::ReadMode::CacheOnly) names. With
    ///   neither, the commit syncs inline, as `commit` does, and the ticket is
    ///   ready when this returns. A queue that is not open on this database
    ///   ends the commit with [`Error::InvalidArgument`].
    /// - **Ready at return.** A commit that waits for no fsync (it, and every
    ///   commit decided before it and not yet visible, at `Eventual`
    ///   durability), and one decided before the pipeline (a conflict the
    ///   early check found, a failed `before_commit` callback), returns a
    ///   ready ticket: its callbacks ran on this thread
    ///   before this returned.
    /// - **Completed on the queue.** Otherwise the outcome is delivered at
    ///   the [`IoQueue::poll`](crate::IoQueue::poll) of this transaction's
    ///   queue that takes it in, on that queue's thread: the transaction's
    ///   own `on_commit` or `on_abort` callbacks run then, then the database
    ///   hooks, then the ticket's `on_complete` callbacks. A conflict is
    ///   reported to the [`crate::EventListener`]s then too.
    /// - **Reads.** On a `CacheOnly` transaction the `before_commit`
    ///   callbacks' reads stay non-blocking: one that misses ends the commit
    ///   with [`TransactionError::WouldBlock`] in the ticket, and the
    ///   `on_abort` callbacks run. [`OptimisticTransactionDb::transact_async`](super::OptimisticTransactionDb::transact_async)
    ///   prepares first and waits out such a miss instead. The commit's own
    ///   validation reads only the cache on this thread too, but a miss there
    ///   never ends the commit, which could not be made again: the block is
    ///   read at a poll of this transaction's queue, and the commit goes on
    ///   there, checked again before it takes a slot or decided with the
    ///   device once it has one. Its ticket then completes on the queue.
    /// - **Locks and the snapshot.** The snapshot is released when this
    ///   returns. A pessimistic transaction's key locks are held until the
    ///   commit lands (applied and visible, and durable at `Immediate`), so
    ///   no transaction that takes one reads past a commit written but not
    ///   yet visible. They are released by the thread that lands the commit,
    ///   whether or not this transaction's queue was polled since: another
    ///   transaction never waits for this one's outcome to be delivered.
    ///
    /// Dropping the ticket never cancels the commit. Dropping the queue
    /// delivers its tickets on the dropping thread (see
    /// [`IoQueue`](crate::IoQueue)).
    ///
    /// # Example
    ///
    /// ```
    /// use regolith::{OptimisticTransactionDb, Options, TransactionError, TxnOptions};
    ///
    /// let db = OptimisticTransactionDb::open("rows", Options::memory())?;
    /// // This thread's queue: the outcomes of its commits are delivered here.
    /// let mut queue = db.db().io_queue();
    /// let on_queue = TxnOptions::new().io_queue(queue.id());
    ///
    /// let txn = db.begin(&on_queue);
    /// txn.put(b"row", b"1")?;
    /// let ticket = txn.commit_nowait();
    /// // The commit is decided and written, and the call did not wait for a
    /// // sync. `block_on` polls the queue until the ticket completes. (An
    /// // in-memory database has no sync to wait for, so here it already has.)
    /// let receipt = queue.block_on(ticket)?;
    /// assert!(receipt.seq() > 0);
    /// assert_eq!(db.db().get(b"row")?.as_deref(), Some(&b"1"[..]));
    ///
    /// // A lost race comes through the ticket too, never out of the call.
    /// let (one, two) = (db.begin(&on_queue), db.begin(&on_queue));
    /// one.put(b"row", b"one")?;
    /// two.put(b"row", b"two")?;
    /// let (one, two) = (one.commit_nowait(), two.commit_nowait());
    /// assert!(queue.block_on(one).is_ok());
    /// assert!(matches!(queue.block_on(two), Err(TransactionError::Conflict(_))));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn commit_nowait(mut self) -> CommitTicket {
        let engine = Arc::clone(&self.engine);
        let completion = CommitTicket::completion(Arc::downgrade(&engine));
        // `close` may have aborted the transaction since it began: the commit
        // is then refused, and `close` ran the abort callbacks.
        let claimed = self.claim.as_ref().is_none_or(|claim| claim.begin_commit());
        self.committing = claimed;
        self.nowait = true;
        let committed = if claimed {
            self.commit_nowait_inner(&engine)
        } else {
            Committed::Done(Err(Error::Closed.into()))
        };
        self.resolved = true;
        self.conclude_nowait(&engine, claimed, committed, &completion);
        CommitTicket::new(completion)
    }

    /// End the commit as far as `committed` got: tell `completion` the
    /// outcome now, leave the commit to tell it on its queue once published,
    /// or leave it owed on its queue. What `commit_nowait` does at return,
    /// and what an owed commit does once it was submitted again. Inlined so
    /// the transaction is not moved again on the path that does not miss.
    #[inline(always)]
    fn conclude_nowait(
        mut self,
        engine: &Arc<RegolithEngine>,
        claimed: bool,
        committed: Committed,
        completion: &Arc<Ticket>,
    ) {
        match committed {
            Committed::Done(result) => {
                // As `commit` ends: locks and snapshot first, so a callback
                // holds nothing of this transaction.
                self.release_resources();
                if let Err(TransactionError::Conflict(conflict)) = &result {
                    engine.notify_conflict(conflict);
                }
                self.finish(claimed, &result);
                completion.decide(result);
                completion.deliver_decided();
            }
            Committed::Pending {
                pending,
                queue,
                scan_runs_dropped,
            } => {
                let locks = self.take_landing_locks();
                let snapshot = Arc::new(LandingSnapshot {
                    engine: Arc::downgrade(engine),
                    pin: crate::sync::Mutex::new(self.snapshot_pin.take()),
                });
                self.release_resources();
                let ending = self.ending();
                let stats = engine.statistics_arc();
                let snapshot_seq = self.snapshot_seq;
                let pending = Arc::new(pending);
                let waited = Arc::clone(&pending);
                completion.resolve_with(Box::new(move || {
                    let settled = waited
                        .settled(stats.as_deref())
                        .and_then(|settled| conclude_settled(stats.as_deref(), settled));
                    receipt_of(stats.as_deref(), settled, snapshot_seq, scan_runs_dropped)
                }));
                let report = Arc::downgrade(engine);
                completion.first(Box::new(move |result| {
                    if let Err(TransactionError::Conflict(conflict)) = result
                        && let Some(engine) = report.upgrade()
                    {
                        engine.notify_conflict(conflict);
                    }
                    ending.finish(true, result);
                }));
                // Told by whichever thread lands the commit, on that thread;
                // a commit that landed already refuses, and they go now.
                if let Some(locks) = locks
                    && !pending.listen(Arc::clone(&locks) as Arc<dyn Delivery>)
                {
                    locks.release();
                }
                if !pending.listen(Arc::clone(&snapshot) as Arc<dyn Delivery>) {
                    snapshot.release();
                }
                let waiter: Arc<dyn Delivery> = Arc::clone(completion) as Arc<dyn Delivery>;
                // Told on the queue once the commit is published. A queue
                // dropped since this commit looked it up would have
                // delivered the ticket on its thread, so this one does,
                // once the commit was brought through.
                engine.await_commit(&pending, &queue, waiter);
            }
            Committed::Owed(owing) => {
                let Owing {
                    owed,
                    wait,
                    queue,
                    scan_runs_dropped,
                } = *owing;
                // Nothing but this commit decides its ticket, so what the
                // outcome owes is set once it was submitted again, before
                // that decision.
                let resubmit = Arc::new(Resubmit {
                    waiting: Take::empty(),
                    claimed,
                    queue: Arc::clone(&queue),
                    scan_runs_dropped,
                    completion: Arc::clone(completion),
                });
                resubmit.waiting.put((self, owed));
                let job = Job::new(Box::new(ReadOwed(wait)));
                let told = Arc::clone(&resubmit) as Arc<dyn Delivery>;
                if !engine.io().wait_on(&queue, job, Some(told)) {
                    // The queue is gone and polls no more.
                    resubmit.deliver();
                }
            }
        }
    }

    /// Submit an owed commit again, once the read it waited for is done, and
    /// end it as far as that gets. On its queue's thread, or on the thread
    /// that dropped the queue: a queue that is gone polls no more, so the
    /// commit then reads the device here, as a blocking one does.
    fn resubmit(
        self,
        owed: Owed,
        claimed: bool,
        queue: &Arc<QueueShared>,
        scan_runs_dropped: u64,
        completion: &Arc<Ticket>,
    ) {
        let engine = Arc::clone(&self.engine);
        let on_device = engine.io().queue(queue.id()).is_none();
        let committed = {
            // A panic in the caller's classifier is caught in this
            // validation too, as `drain_for_commit` arranges for the first.
            let _contain =
                policy::classifier_for(&self.policy, self.isolation).map(|_| InCommit::enter());
            match owed {
                Owed::Writes(commit) => self.submitted(
                    engine.resubmit_nowait(commit, on_device),
                    Arc::clone(queue),
                    scan_runs_dropped,
                ),
                Owed::WriteFree { checks, read_queue } => {
                    let _device = on_device.then(scope::blocking);
                    self.write_free(
                        &engine,
                        checks,
                        read_queue.filter(|_| !on_device),
                        Some(Arc::clone(queue)),
                        scan_runs_dropped,
                    )
                }
            }
        };
        self.conclude_nowait(&engine, claimed, committed, completion);
    }

    /// The commit up to the point it returns: the claim was won.
    fn commit_nowait_inner(&mut self, engine: &Arc<RegolithEngine>) -> Committed {
        let queue = match self.io_queue() {
            None => None,
            Some(id) => match engine.io().queue(id) {
                Some(queue) => Some(queue),
                None => {
                    return Committed::Done(Err(Error::invalid_argument(
                        "the I/O queue this transaction names is not open on this database",
                    )
                    .into()));
                }
            },
        };
        if let Err(err) = self.prepare() {
            return Committed::Done(Err(err));
        }
        let super::Drained {
            mut commit,
            write_free,
            scan_runs_dropped,
            _contain,
        } = match self.drain_for_commit() {
            Ok(drained) => drained,
            Err(err) => return Committed::Done(Err(err)),
        };
        // A `CacheOnly` handle never reads the device, in its commit's own
        // validation either, written or not.
        let read_queue = self.read_mode().queue();
        if write_free {
            return self.write_free(engine, commit.checks, read_queue, queue, scan_runs_dropped);
        }
        commit.read_queue = read_queue;
        let Some(queue) = queue else {
            // No queue to complete on: the commit syncs inline.
            let outcome = engine.commit_with_conflict_check(
                commit.checks,
                commit.point_ops,
                commit.range_deletes,
                commit.merges,
                commit.appends,
                commit.durability,
            );
            return Committed::Done(self.receipt(outcome, scan_runs_dropped));
        };
        self.submitted(
            engine.commit_optimistic_nowait(commit),
            queue,
            scan_runs_dropped,
        )
    }

    /// What the engine's answer to a commit that writes makes of it.
    fn submitted(
        &self,
        answer: io::Result<NowaitCommit>,
        queue: Arc<QueueShared>,
        scan_runs_dropped: u64,
    ) -> Committed {
        match answer {
            Ok(NowaitCommit::Done(outcome)) => {
                Committed::Done(self.receipt(Ok(outcome), scan_runs_dropped))
            }
            Ok(NowaitCommit::Pending(pending)) => Committed::Pending {
                pending,
                queue,
                scan_runs_dropped,
            },
            Ok(NowaitCommit::Owed { commit, wait }) => Committed::Owed(Box::new(Owing {
                owed: Owed::Writes(commit),
                wait,
                queue,
                scan_runs_dropped,
            })),
            Err(err) => Committed::Done(self.receipt(Err(err), scan_runs_dropped)),
        }
    }

    /// Check a commit that writes nothing, which takes no slot, owed its
    /// check when it needs a block a `CacheOnly` handle may not read.
    fn write_free(
        &self,
        engine: &RegolithEngine,
        checks: ValidationSet,
        read_queue: Option<QueueId>,
        queue: Option<Arc<QueueShared>>,
        scan_runs_dropped: u64,
    ) -> Committed {
        let outcome = engine.commit_write_free(&checks, read_queue);
        if let (Err(err), Some(queue)) = (&outcome, queue)
            && let Some(WouldBlock::Io(wait)) = copy_would_block(err)
        {
            return Committed::Owed(Box::new(Owing {
                owed: Owed::WriteFree { checks, read_queue },
                wait,
                queue,
                scan_runs_dropped,
            }));
        }
        Committed::Done(self.receipt(outcome, scan_runs_dropped))
    }

    /// The commit's receipt, or why it failed, from what the engine decided.
    fn receipt(
        &self,
        outcome: io::Result<CommitOutcome>,
        scan_runs_dropped: u64,
    ) -> TxResult<CommitReceipt> {
        receipt_of(
            self.engine.statistics(),
            outcome,
            self.snapshot_seq,
            scan_runs_dropped,
        )
    }
}
