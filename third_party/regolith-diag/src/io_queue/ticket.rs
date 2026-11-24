//! Tickets: what a call that does not wait for its I/O hands back.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Weak};
use std::task::{Context, Poll};

use super::completion::Completion;
use crate::engine::RegolithEngine;
use crate::engine::io::job::Job;
use crate::{CommitReceipt, Error, TxResult};

/// The outcome of a [`Transaction::commit_nowait`](crate::Transaction::commit_nowait).
///
/// Ready once the commit is visible, and durable when its durability is
/// [`Immediate`](crate::DurabilityMode::Immediate), or with the reason it
/// did not commit: a conflict, a failed log write, `Closed`. Every outcome
/// comes through the ticket, a conflict included.
///
/// - **Where it completes.** A commit on a transaction with an
///   [`IoQueue`](crate::IoQueue) (named by
///   [`TxnOptions::io_queue`](crate::TxnOptions::io_queue), or by its
///   [`ReadMode::CacheOnly`](crate::ReadMode::CacheOnly)) whose outcome is
///   still owed an fsync when `commit_nowait` returns is completed on that
///   queue: at the [`IoQueue::poll`](crate::IoQueue::poll) that takes the
///   completion in, on the thread that owns the queue, and nowhere else. A
///   task awaiting the ticket is woken by that poll only. A commit whose
///   outcome is known when `commit_nowait` returns (a conflict, an
///   `Eventual` commit, a transaction with no queue, which syncs inline)
///   returns a ticket that is ready already.
/// - **Callbacks.** The transaction's own `on_commit` or `on_abort`
///   callbacks, then the database's hooks, then each [`on_complete`](Self::on_complete)
///   callback run once, on the thread that delivers the outcome (the queue's
///   owner at that poll, or the committing thread for a ticket ready at
///   return), in that order.
/// - **Dropping** a ticket never cancels the commit: it is decided and
///   written before the ticket exists, and its callbacks still run when the
///   queue delivers it. Dropping the queue instead delivers every ticket it
///   holds on the dropping thread (see [`IoQueue`](crate::IoQueue)).
///
/// It is a [`Future`] whose output is the commit's outcome; awaiting it again
/// after it was ready returns a copy of the same outcome.
///
/// # Example
///
/// ```
/// use std::sync::Arc;
/// use std::sync::atomic::{AtomicU64, Ordering};
///
/// use regolith::{IoBudget, OptimisticTransactionDb, Options, TxnOptions};
///
/// let db = OptimisticTransactionDb::open("rows", Options::memory())?;
/// let mut queue = db.db().io_queue();
/// let on_queue = TxnOptions::new().io_queue(queue.id());
///
/// let txn = db.begin(&on_queue);
/// txn.put(b"row", b"1")?;
/// let ticket = txn.commit_nowait();
///
/// // The callback gets the outcome once: at the poll that delivers it, or
/// // at once when the ticket is ready already, as it is on this in-memory
/// // database, which has no sync to wait for.
/// let delivered = Arc::new(AtomicU64::new(0));
/// let seen = Arc::clone(&delivered);
/// ticket.on_complete(move |outcome| {
///     if let Ok(receipt) = outcome {
///         seen.store(receipt.seq(), Ordering::Relaxed);
///     }
/// });
///
/// // The queue's owner polls it when it has nothing else to run. On a
/// // durable database the poll that takes the outcome in completes the
/// // ticket.
/// while !ticket.is_ready() {
///     queue.poll(IoBudget::ALL);
/// }
/// let receipt = queue.block_on(ticket)?;
/// assert_eq!(delivered.load(Ordering::Relaxed), receipt.seq());
///
/// // Dropping a ticket cancels nothing: the commit is already decided.
/// let txn = db.begin(&on_queue);
/// txn.put(b"row", b"2")?;
/// drop(txn.commit_nowait());
/// queue.poll(IoBudget::ALL);
/// assert_eq!(db.db().get(b"row")?.as_deref(), Some(&b"2"[..]));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub struct CommitTicket {
    completion: Arc<Completion<TxResult<CommitReceipt>>>,
}

impl CommitTicket {
    pub(crate) fn new(completion: Arc<Completion<TxResult<CommitReceipt>>>) -> Self {
        Self { completion }
    }

    /// A completion for a commit on `engine`, with nothing decided.
    pub(crate) fn completion(
        engine: Weak<RegolithEngine>,
    ) -> Arc<Completion<TxResult<CommitReceipt>>> {
        Completion::new(engine, "CommitTicket::on_complete")
    }

    /// Whether the outcome has been delivered. Never waits.
    pub fn is_ready(&self) -> bool {
        self.completion.is_ready()
    }

    /// Run `f` once with the commit's outcome: at the poll that delivers it,
    /// on the thread that owns the ticket's queue, or at once, on this
    /// thread, if the ticket is ready already. A registration that races the
    /// delivery, from any thread, is run by that delivery before the ticket
    /// is ready: this thread runs `f` only when [`is_ready`](Self::is_ready)
    /// already answers `true`. `f` runs after the
    /// transaction's own callbacks and the database hooks. It must not block
    /// or re-enter the database; a panic in it is caught and reported to
    /// [`EventListener::on_callback_panic`](crate::EventListener::on_callback_panic),
    /// and the outcome stands.
    pub fn on_complete(&self, f: impl FnOnce(&TxResult<CommitReceipt>) + Send + 'static) {
        self.completion.on_complete(Box::new(f));
    }
}

impl Future for CommitTicket {
    type Output = TxResult<CommitReceipt>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.completion.poll_ready(cx).map(|outcome| match outcome {
            Ok(receipt) => Ok(*receipt),
            Err(err) => Err(err.duplicate()),
        })
    }
}

impl std::fmt::Debug for CommitTicket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommitTicket")
            .field("ready", &self.is_ready())
            .finish()
    }
}

/// The outcome of a foreground job: [`Db::compact_range`](crate::Db::compact_range),
/// [`Db::ingest_external_files`](crate::Db::ingest_external_files) or
/// [`Db::checkpoint`](crate::Db::checkpoint).
///
/// The call returns it at once and never waits on the compaction gate. The
/// job runs on the compaction worker when the database has one, and
/// otherwise as a job on the calling thread's [`IoQueue`](crate::IoQueue),
/// run at that queue's poll; a caller with neither runs it inline, before
/// the call returns. A poll never waits for the gate either: while a
/// compaction pass or another job holds it, the job stays pending on the
/// queue, and the queue's owner is woken once it may go on. When it is done, the ticket is completed on the queue
/// of the thread that made the call (at that queue's poll), or, for a caller
/// with no queue, directly by whichever thread finished the job.
///
/// It is a [`Future`] whose output is the job's result; [`JobTicket::wait`]
/// blocks for it instead. Dropping a ticket cancels nothing: the job still
/// runs and its `on_complete` callbacks still run.
pub struct JobTicket {
    completion: Arc<Completion<Result<(), Error>>>,
    /// The job, when it waits on the caller's queue: [`JobTicket::wait`]
    /// runs it itself if nobody has claimed it.
    job: Option<Arc<Job>>,
}

impl JobTicket {
    pub(crate) fn new(completion: Arc<Completion<Result<(), Error>>>) -> Self {
        Self {
            completion,
            job: None,
        }
    }

    /// The ticket of `job`, a unit left on the caller's queue.
    pub(crate) fn of_job(completion: Arc<Completion<Result<(), Error>>>, job: Arc<Job>) -> Self {
        Self {
            completion,
            job: Some(job),
        }
    }

    /// A completion for a job on `engine`, with nothing decided.
    pub(crate) fn completion(engine: Weak<RegolithEngine>) -> Arc<Completion<Result<(), Error>>> {
        Completion::new(engine, "JobTicket::on_complete")
    }

    /// A ticket that is ready already with `result`.
    pub(crate) fn ready(engine: Weak<RegolithEngine>, result: Result<(), Error>) -> Self {
        let completion = Self::completion(engine);
        completion.decide(result);
        completion.deliver_decided();
        Self::new(completion)
    }

    /// Whether the job is done and its result delivered. Never waits.
    pub fn is_ready(&self) -> bool {
        self.completion.is_ready()
    }

    /// Run `f` once with the job's result: when it is delivered, or at once
    /// if the ticket is ready already. The same rules as
    /// [`CommitTicket::on_complete`] apply.
    pub fn on_complete(&self, f: impl FnOnce(&Result<(), Error>) + Send + 'static) {
        self.completion.on_complete(Box::new(f));
    }

    /// Block this thread until the job is done, and return its result: a
    /// blocking caller's way to use the ticket.
    ///
    /// A job left on this thread's [`IoQueue`](crate::IoQueue) that no poll
    /// has run to its end yet is brought to its end here, on this thread,
    /// together with the other jobs the queue holds: each is stepped as the
    /// queue's poll steps it, and the thread sleeps while another thread
    /// holds what they need (a compaction pass under the gate, a flush). An
    /// older job of the same queue that holds the gate, or was handed it,
    /// is therefore run here too, in the gate's order. A block read pending
    /// on the queue is not run here; it holds nothing a job waits for.
    ///
    /// A job on another thread's queue that nobody is running is run here
    /// to its end, waiting on this thread for whatever it needs; one a
    /// worker or a poll runs is waited for. The ticket's delivery is
    /// unchanged: on a ticket completed on a queue, the `on_complete`
    /// callbacks still run at that queue's poll.
    pub fn wait(self) -> Result<(), Error> {
        if let Some(job) = &self.job {
            match self.completion.report().upgrade() {
                // One of this thread's own queue's jobs: stepped with the
                // rest of them until it lands.
                Some(engine) if engine.io().is_own(job) => engine.io().wait_landed(job),
                Some(engine) if job.claim() => engine.run_job(job),
                None if job.claim() => job.run(),
                _ => {}
            }
        }
        match self.completion.wait_decided() {
            Ok(()) => Ok(()),
            Err(err) => Err(err.duplicate()),
        }
    }
}

impl Future for JobTicket {
    type Output = Result<(), Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.completion.poll_ready(cx).map(|outcome| match outcome {
            Ok(()) => Ok(()),
            Err(err) => Err(err.duplicate()),
        })
    }
}

impl std::fmt::Debug for JobTicket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobTicket")
            .field("ready", &self.is_ready())
            .finish()
    }
}
