//! Non-blocking reads: per-thread I/O queues (plan 4.10, D21, D42, D53).
//!
//! A read handle runs in one of two [`ReadMode`]s. A `Blocking` handle reads
//! the device itself when the block cache misses, as every read always has. A
//! `CacheOnly` handle never touches the device: when a read needs a block the
//! cache does not hold, it returns [`WouldBlock::Io`] at once, with an
//! [`IoWait`] for the block, and records the read on the [`IoQueue`] its
//! mode names.
//!
//! Each thread that reads this way holds its own queue, from
//! [`Db::io_queue`](crate::Db::io_queue), and polls it when it has nothing
//! else to run:
//!
//! ```
//! # use regolith::{Db, Error, IoBudget, Options, ReadMode, WouldBlock};
//! # let db = Db::open("rows", Options::memory())?;
//! # db.put(b"key", b"value")?;
//! # db.flush()?;
//! let mut queue = db.io_queue();
//! let snapshot = db.snapshot().with_read_mode(ReadMode::CacheOnly(queue.id()));
//! let value = loop {
//!     match snapshot.get(b"key") {
//!         Err(Error::WouldBlock(WouldBlock::Io(_wait))) => {
//!             // Nothing else to run: do this thread's I/O, then read again.
//!             queue.poll(IoBudget::ALL);
//!         }
//!         other => break other?,
//!     }
//! };
//! # assert_eq!(value.as_deref(), Some(&b"value"[..]));
//! # Ok::<(), regolith::Error>(())
//! ```
//!
//! # What the queues guarantee
//!
//! - **Every completion goes to the queue that asked.** A miss is recorded on
//!   the queue its handle names and on no other; the completion of its read
//!   is pushed to that queue and to no other. The [`IoWait`] is woken only by
//!   that queue's [`IoQueue::poll`], on its owner's thread.
//! - **A block is read once however many queues miss it.** The read is one
//!   *unit* in a table shared by every queue of the database. The first queue
//!   to poll claims it with one compare-and-swap and runs it; a queue that
//!   finds it claimed leaves it to the thread that claimed it. The unit fills
//!   the block cache and pushes one completion to every queue waiting on it.
//! - **A busy thread is never woken.** [`IoQueue::idle_waker`] is the only
//!   way a queue wakes its owner, and only the next push to a queue whose
//!   owner registered it as about to idle does so, once.
//! - **No thread is added.** Each thread does its own I/O when it polls; the
//!   blocking calls read inline as before.
//! - **A timed wait is ended by its queue's poll and by nothing else.** The
//!   pause a `transact_async` attempt takes before a re-run, when its policy
//!   asks for one, is over at the first poll that finds the database's clock
//!   at its due time. No thread sleeps for it and none is woken for it: each
//!   poll says how long until the earliest one ([`IoProgress::wake_in`]), and
//!   an owner that idles polls again by then.
//! - **A poll never waits on another thread.** It reads the device for the
//!   units it runs, and nothing else. Under
//!   [`Options::max_open_files`](crate::Options::max_open_files) a unit that
//!   must reopen its table while every descriptor is in use by other
//!   threads' reads stays pending, and runs again at the poll after one
//!   frees (D60).
//!
//! A read that returns `WouldBlock` is run again after its queue's poll, and
//! then finds its block: in the cache, or, when the cache did not keep it,
//! among the reads that landed for its queue. A scan resumes from where it
//! stopped ([`OwnedSnapshotIter::resume`](crate::OwnedSnapshotIter::resume),
//! [`TxnCursor::next_page`](crate::TxnCursor::next_page)), without skipping
//! or repeating an entry.

pub(crate) mod completion;
mod ledger;
mod queue;
mod ticket;
mod timers;
pub(crate) mod wait;

pub use queue::IoQueue;
pub use ticket::{CommitTicket, JobTicket};
#[cfg(not(target_arch = "wasm32"))]
pub use wait::through_stalls;
pub use wait::{IoUnit, IoWait, LockWait, StallWait, WouldBlock};

use std::num::NonZeroU64;

use crate::engine::io::scope::{self, Scope};

/// Names one [`IoQueue`]. Read handles carry it in
/// [`ReadMode::CacheOnly`]; only the thread holding the queue polls it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct QueueId(NonZeroU64);

impl QueueId {
    pub(crate) fn new(id: NonZeroU64) -> Self {
        Self(id)
    }

    pub(crate) fn get(self) -> u64 {
        self.0.get()
    }
}

/// Where a read handle's block-cache misses go.
///
/// Set per handle: [`TxnOptions::read_mode`](crate::TxnOptions::read_mode)
/// for a transaction and [`Snapshot::with_read_mode`](crate::Snapshot::with_read_mode)
/// for a snapshot, and inherited by the iterators, cursors and streams made
/// from them. It is checked where a read would touch the device: data block,
/// index, index leaf and filter reads, the readahead of a scan, and so the
/// reopening of a file `max_open_files` closed, which happens only inside a
/// device read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ReadMode {
    /// A miss reads the device on the calling thread, and the call waits for
    /// it. The default.
    #[default]
    Blocking,
    /// A miss never touches the device: the read returns
    /// [`WouldBlock::Io`] and the block is read when the named queue is
    /// polled. Naming a queue that is not open on the handle's database makes
    /// such a read fail with [`Error::InvalidArgument`](crate::Error::InvalidArgument).
    ///
    /// A transaction's [`commit_nowait`](crate::Transaction::commit_nowait)
    /// through it validates from the cache on the calling thread too; a block
    /// its validation misses is read at a poll of the queue, and the commit
    /// goes on there. A miss never ends the commit.
    ///
    /// Neither the read nor the poll ever waits on another thread. Under
    /// [`Options::max_open_files`](crate::Options::max_open_files), a block
    /// read whose table must be reopened while every descriptor is in use by
    /// reads on other threads stays pending on the queue instead of waiting
    /// for one: the read that frees a descriptor tells the queue, and its
    /// next poll reads the block.
    CacheOnly(QueueId),
}

impl ReadMode {
    /// The queue a `CacheOnly` mode names.
    pub fn queue(self) -> Option<QueueId> {
        match self {
            Self::Blocking => None,
            Self::CacheOnly(queue) => Some(queue),
        }
    }

    /// Route this thread's device misses as the mode says, for as long as
    /// the returned scope lives. `Blocking` touches nothing.
    #[inline]
    pub(crate) fn scope(self) -> Option<Scope> {
        match self {
            Self::Blocking => None,
            Self::CacheOnly(queue) => Some(scope::cache_only(queue)),
        }
    }

    pub(crate) fn is_cache_only(self) -> bool {
        matches!(self, Self::CacheOnly(_))
    }
}

/// How much device I/O one [`IoQueue::poll`] may run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IoBudget {
    units: usize,
}

impl IoBudget {
    /// Run every unit this thread waits on and nobody else is running.
    pub const ALL: Self = Self { units: usize::MAX };

    /// Run at most `units` reads in this poll. `0` runs none and only takes
    /// in what other threads completed.
    pub const fn units(units: usize) -> Self {
        Self { units }
    }

    pub(crate) fn limit(self) -> usize {
        self.units
    }
}

/// What one [`IoQueue::poll`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct IoProgress {
    /// Landed bytes protected by completed waits with live callers at the
    /// latest landing poll. Shared blocks count once, using their newest landing charge.
    /// This is part of the memory that can exceed the queue's byte bound;
    /// pending calls can also hold blocks. A live wait expires after 64
    /// landing polls, after which its caller may need to reread.
    pub retained_live_bytes: usize,
    /// Bytes of every block this queue keeps landed after the poll: the
    /// landings within its byte bound plus those pending and retained calls
    /// hold past it, so this may exceed the bound. A rising value with few
    /// calls in progress names those holds as what keeps the memory.
    pub landed_bytes: usize,
    /// Reads this queue waited on that finished during the poll, each counted
    /// once however many of the queue's reads waited on it, whichever thread
    /// ran it; and the timed waits the poll found due.
    pub completed: usize,
    /// The queue still waits on reads. Poll again, or register
    /// [`IoQueue::idle_waker`] and idle: a completion from another thread
    /// wakes the owner, and a read only this thread can run wakes it at once.
    pub more_pending: bool,
    /// How long until the earliest timed wait on this queue is due, by the
    /// clock of the database's [`Env`](crate::Env); `None` when it holds
    /// none. A timed wait is the pause a
    /// [`transact_async`](crate::OptimisticTransactionDb::transact_async)
    /// attempt takes before a re-run when its
    /// [`RetryPolicy`](crate::RetryPolicy) asks for one.
    ///
    /// Nothing wakes an idle owner for it: only a poll completes a timed
    /// wait. An owner that idles must poll again within this long. A wait
    /// recorded while the owner idles wakes it, once, so the poll after
    /// that reports it.
    pub wake_in: Option<std::time::Duration>,
}

#[cfg(test)]
mod tests;
#[cfg(all(test, not(loom)))]
mod timer_tests;
