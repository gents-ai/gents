//! What a non-blocking call hands back instead of waiting.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Weak};
use std::task::{Context, Poll};

use super::QueueId;
use crate::engine::RegolithEngine;
use crate::engine::io::atomic_waker::AtomicWaker;
use crate::engine::io::job::Job;
use crate::engine::io::shared::{QueueShared, WaitSlot};
use crate::engine::io::unit::UnitKey;

/// Why a call returned instead of waiting, and what to wait on.
///
/// Every call that would otherwise wait returns one of these at once,
/// carried by [`Error::WouldBlock`](crate::Error::WouldBlock) or
/// [`TransactionError::WouldBlock`](crate::TransactionError::WouldBlock).
/// Wait on what it holds, then run the call again.
#[derive(Clone, Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WouldBlock {
    /// A `CacheOnly` read needs a block the cache does not hold. The read is
    /// recorded on the queue the handle's [`ReadMode`](crate::ReadMode)
    /// names; poll that queue, then run the read again. Also what
    /// [`Db::allocate_nowait`](crate::Db::allocate_nowait) hands back when
    /// device work is in its way: the wait is recorded on the queue it names.
    #[error("the call needs I/O that is not done yet; poll its I/O queue and run the call again")]
    Io(IoWait),
    /// A write met a write stall: too many level-0 tables, too many memtables
    /// waiting to be flushed, or too much data waiting to be compacted. The
    /// write applied nothing. Wait for the stall to clear, then run the write
    /// again.
    #[error("{}", .0.describe())]
    Stall(StallWait),
    /// A pessimistic transaction's
    /// [`try_get_for_update`](crate::Transaction::try_get_for_update) found
    /// the key locked by another transaction. The call took nothing; wait
    /// for the lock, then run the call again.
    #[error("the key is locked by another transaction; wait for the lock and run the call again")]
    Locked(LockWait),
}

/// Names the device read an [`IoWait`] waits on. Two waits name the same
/// unit exactly when one read of the device serves them both.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct IoUnit {
    file: u64,
    offset: u64,
    guard: Option<usize>,
}

impl From<UnitKey> for IoUnit {
    fn from(key: UnitKey) -> Self {
        Self {
            file: key.file_id,
            offset: key.offset,
            guard: key.guard,
        }
    }
}

/// One wait on a [`WaitSlot`]: the slot, and this wait's own registration on
/// it, made by its first pending poll and reused after. Shared by every wait
/// a call hands back.
struct Listen {
    slot: Arc<WaitSlot>,
    listener: Option<Arc<AtomicWaker>>,
}

impl Listen {
    fn new(slot: Arc<WaitSlot>) -> Self {
        Self {
            slot,
            listener: None,
        }
    }

    fn is_ready(&self) -> bool {
        self.slot.is_ready()
    }

    fn poll(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        if self.slot.is_ready() {
            return Poll::Ready(());
        }
        match &self.listener {
            Some(listener) => listener.register(cx.waker()),
            None => {
                let listener = Arc::new(AtomicWaker::new());
                listener.register(cx.waker());
                // Refused only when the wait is ready already.
                if !self.slot.listen(Arc::clone(&listener)) {
                    return Poll::Ready(());
                }
                self.listener = Some(listener);
            }
        }
        // The slot may have completed between the check above and the
        // registration: it then woke the old waker or none, so look again.
        if self.slot.is_ready() {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

impl Clone for Listen {
    fn clone(&self) -> Self {
        Self::new(Arc::clone(&self.slot))
    }
}

/// A wait for one `CacheOnly` read, ready once the queue it was recorded on
/// has finished the read.
///
/// It is a [`Future`] with output `()`, and is woken only by its own queue's
/// [`IoQueue::poll`](crate::IoQueue::poll), on the thread that owns the
/// queue: a task that awaits it resumes once that thread polls. Ready means
/// the read can be run again; it does not mean the read will now succeed (the
/// database may have closed, or the block may have failed its checksum, which
/// the read run again reports).
///
/// A clone waits on the same read and is woken with it; each clone is woken
/// through its own registration, so several tasks may await clones at once.
/// Dropping a wait never cancels the read: the queue completes it all the
/// same. A retained ready wait protects the blocks its call needs across
/// up to 64 landing polls; keep it until rerunning the call when delaying
/// that run. After that finite window, the call may reread expired blocks.
/// A clone kept after the call finishes also retains blocks until it drops
/// or reaches that limit.
#[derive(Clone)]
pub struct IoWait {
    listen: Listen,
    queue: QueueId,
    unit: Option<IoUnit>,
}

impl IoWait {
    /// A wait on `slot`, recorded on `queue` for `unit` (`None` for a wait
    /// that names no block: room in the queue's byte bound, or work of the
    /// commit pipeline in an allocation's way).
    pub(crate) fn new(queue: &QueueShared, unit: Option<UnitKey>, slot: Arc<WaitSlot>) -> Self {
        Self {
            listen: Listen::new(slot),
            queue: queue.id(),
            unit: unit.map(IoUnit::from),
        }
    }

    /// The queue this wait was recorded on: the only one whose poll makes it
    /// ready.
    pub fn queue(&self) -> QueueId {
        self.queue
    }

    /// The device read this wait waits on. `None` for a read that found its
    /// queue's byte bound full: it waits until the queue owes less, and then
    /// records its read when it is run again. `None` too for a
    /// [`Db::allocate_nowait`](crate::Db::allocate_nowait) that found other
    /// work of the commit pipeline in its way: it waits until that work is
    /// done.
    pub fn unit(&self) -> Option<IoUnit> {
        self.unit
    }

    /// Whether the queue finished this read. Never waits.
    pub fn is_ready(&self) -> bool {
        self.listen.is_ready()
    }

    /// This wait as the `io::Error` the engine's read paths carry it in; the
    /// public error types take it back out by value.
    pub(crate) fn into_io_error(self) -> std::io::Error {
        std::io::Error::new(std::io::ErrorKind::WouldBlock, WouldBlock::Io(self))
    }
}

impl std::fmt::Debug for IoWait {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IoWait")
            .field("queue", &self.queue)
            .field("unit", &self.unit)
            .field("ready", &self.is_ready())
            .finish()
    }
}

impl Future for IoWait {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        self.get_mut().listen.poll(cx)
    }
}

/// A wait for a write stall to clear, handed back by a write the stall
/// stopped ([`WouldBlock::Stall`]).
///
/// The stall clears when background work catches up: a flush that writes a
/// memtable out, a compaction that merges level-0 tables. Whoever's work
/// clears it completes this wait. A write from a thread with an
/// [`IoQueue`](crate::IoQueue) open records the wait on that queue, and it
/// becomes ready only at that queue's [`poll`](crate::IoQueue::poll), on
/// that thread; a write from a thread with none is completed directly, on
/// the thread whose work cleared the stall, and [`StallWait::wait`] blocks
/// for it. Ready means the write can run again; the stall may have come back
/// by then, and the write run again says so.
///
/// On a database with no compaction worker, nothing clears a stall unless a
/// caller compacts: with [`Options::inline_compaction`](crate::Options::inline_compaction)
/// on, as it is there unless it was turned off, the stopped write also
/// leaves the step that clears it on its thread's queue (or runs it inline
/// when the thread has none); with it turned off, call
/// [`Db::compact_step`](crate::Db::compact_step) or [`Db::flush`](crate::Db::flush),
/// which this wait's message says too.
///
/// It is a [`Future`] with output `()`. Clones wait on the same stall.
/// Dropping a wait cancels nothing.
#[derive(Clone)]
pub struct StallWait {
    listen: Listen,
    /// The stall's unit, which lands when the stall clears. Read only by the
    /// blocking wait, which `wasm32` lacks.
    #[cfg_attr(target_arch = "wasm32", expect(dead_code))]
    stall: Arc<Job>,
    queue: Option<QueueId>,
    reason: &'static str,
    /// Nothing relieves the stall but the caller: no compaction worker, and
    /// inline compaction turned off.
    self_driven: bool,
    /// The database, for the blocking wait: it steps the calling thread's
    /// own jobs through it, and counts the time blocked in its statistics.
    /// Read only by that wait, which `wasm32` lacks: there it is set and
    /// never read. The compiler raises no dead-code lint for it there, so
    /// it carries no expectation of one, unlike `stall`.
    engine: Weak<RegolithEngine>,
}

impl StallWait {
    pub(crate) fn new(
        slot: Arc<WaitSlot>,
        stall: Arc<Job>,
        queue: Option<QueueId>,
        reason: &'static str,
    ) -> Self {
        Self {
            listen: Listen::new(slot),
            stall,
            queue,
            reason,
            self_driven: false,
            engine: Weak::new(),
        }
    }

    /// Say whether nothing relieves this stall but the caller, which the
    /// wait's message then says how to do.
    pub(crate) fn left_to_the_caller(mut self, self_driven: bool) -> Self {
        self.self_driven = self_driven;
        self
    }

    /// Name the database the stall is on, for [`StallWait::wait`].
    pub(crate) fn on(mut self, engine: Weak<RegolithEngine>) -> Self {
        self.engine = engine;
        self
    }

    /// What a write this stall stopped is told: what holds it, and what to
    /// do.
    pub(crate) fn describe(&self) -> String {
        let reason = self.reason;
        if self.self_driven {
            format!(
                "writes are stalled behind background work ({reason}) that this database \
                 leaves to its caller; call compact_step until the stall clears, or turn \
                 inline_compaction on, then run the write again"
            )
        } else {
            format!(
                "writes are stalled behind background work ({reason}); wait for the stall to \
                 clear and run the write again"
            )
        }
    }

    /// The queue this wait was recorded on, whose poll makes it ready; `None`
    /// for a write from a thread with no queue.
    pub fn queue(&self) -> Option<QueueId> {
        self.queue
    }

    /// Which stall stopped the write.
    pub fn reason(&self) -> &'static str {
        self.reason
    }

    /// Whether the stall cleared. Never waits.
    pub fn is_ready(&self) -> bool {
        self.listen.is_ready()
    }

    /// Block this thread until the stall clears: a blocking caller's way to
    /// wait. It waits for the stall itself, so it returns whether or not the
    /// wait was recorded on a queue; a wait on a queue still becomes ready
    /// only at that queue's poll.
    ///
    /// On a thread that owns an [`IoQueue`](crate::IoQueue), the jobs that
    /// queue holds are stepped here while the thread waits, as its poll
    /// would step them: the step that relieves the stall, or a
    /// [`JobTicket`](crate::JobTicket)'s job that holds what that step
    /// needs, may be among them, and nothing else would run it. A block
    /// read pending on the queue is not run here; it holds nothing a stall
    /// waits for.
    ///
    /// Not on `wasm32`, whose one thread would wait for work only it can do.
    ///
    /// The time spent blocked here is counted in
    /// [`Ticker::WriteStallMicros`](crate::Ticker::WriteStallMicros).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn wait(&self) {
        let blocked = std::time::Instant::now();
        let engine = self.engine.upgrade();
        match &engine {
            Some(engine) => engine.io().wait_landed(&self.stall),
            // Every handle on the database is gone: there is no queue of it
            // to step, and the wait is for the stall alone.
            None => self.stall.wait_landed(),
        }
        if let Some(stats) = engine.as_ref().and_then(|engine| engine.statistics()) {
            let micros = u64::try_from(blocked.elapsed().as_micros()).unwrap_or(u64::MAX);
            stats.add(crate::Ticker::WriteStallMicros, micros);
        }
    }
}

impl std::fmt::Debug for StallWait {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StallWait")
            .field("queue", &self.queue)
            .field("reason", &self.reason)
            .field("ready", &self.is_ready())
            .finish()
    }
}

impl Future for StallWait {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        self.get_mut().listen.poll(cx)
    }
}

/// A wait for a key lock another transaction holds, handed back by
/// [`Transaction::try_get_for_update`](crate::Transaction::try_get_for_update)
/// ([`WouldBlock::Locked`]).
///
/// It becomes ready when the lock may be the waiting transaction's: the
/// holder released it, or it was handed to this transaction because others
/// took it past this one too many times. Ready says only that: the call run
/// again takes the lock, finds it handed over, or, when another transaction
/// took it first, returns a new wait. The transaction keeps its place among
/// the key's waiters across those runs, and a transaction passed over a
/// bounded number of times ([`MAX_BYPASS`](crate::sync::MAX_BYPASS)) is
/// handed the lock, so no waiter starves.
///
/// A transaction with an [`IoQueue`](crate::IoQueue)
/// ([`TxnOptions::io_queue`](crate::TxnOptions::io_queue), or the queue its
/// [`ReadMode::CacheOnly`](crate::ReadMode::CacheOnly) names) records the
/// wait on that queue: it becomes ready only at that queue's
/// [`poll`](crate::IoQueue::poll), on its owner's thread, which an idle
/// owner is woken for once and a busy one never is. A transaction with no
/// queue is completed directly, on the thread that released the lock. A wait
/// whose queue was dropped meanwhile is completed directly too. When the
/// database closes, every wait becomes ready, and the call run again fails
/// with [`Error::Closed`](crate::Error::Closed).
///
/// It is a [`Future`] with output `()`. Clones wait on the same lock.
/// Dropping a wait cancels nothing: the transaction keeps its place until it
/// runs the call again or ends.
#[derive(Clone)]
pub struct LockWait {
    listen: Listen,
    queue: Option<QueueId>,
}

impl LockWait {
    pub(crate) fn new(slot: Arc<WaitSlot>, queue: Option<QueueId>) -> Self {
        Self {
            listen: Listen::new(slot),
            queue,
        }
    }

    /// The queue this wait was recorded on, whose poll makes it ready; `None`
    /// for a transaction with no queue.
    pub fn queue(&self) -> Option<QueueId> {
        self.queue
    }

    /// Whether the lock may be free now. Never waits.
    pub fn is_ready(&self) -> bool {
        self.listen.is_ready()
    }
}

impl std::fmt::Debug for LockWait {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LockWait")
            .field("queue", &self.queue)
            .field("ready", &self.is_ready())
            .finish()
    }
}

impl Future for LockWait {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        self.get_mut().listen.poll(cx)
    }
}

/// A timed wait recorded on a queue ([`IoRuntime::timer`]): ready once the
/// queue's poll finds the database's clock at the wait's due time. Not
/// handed to a caller: it is the pause a
/// [`transact_async`](crate::OptimisticTransactionDb::transact_async)
/// attempt takes before a re-run.
///
/// [`IoRuntime::timer`]: crate::engine::io::IoRuntime::timer
pub(crate) struct Pause {
    listen: Listen,
}

impl Pause {
    pub(crate) fn new(slot: Arc<WaitSlot>) -> Self {
        Self {
            listen: Listen::new(slot),
        }
    }
}

impl Future for Pause {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        self.get_mut().listen.poll(cx)
    }
}

/// Run a write `call` until no write stall stops it: on
/// [`WouldBlock::Stall`], block this thread until the stall clears
/// ([`StallWait::wait`]) and call again. Any other outcome is returned.
///
/// For a blocking caller that would rather wait out a stall than handle it:
/// the write itself never waits, so this is where the waiting is
/// chosen, by the caller, on its own thread. On a database with no
/// compaction worker the stall clears only when something compacts: inline
/// compaction ([`Options::inline_compaction`](crate::Options::inline_compaction))
/// on a thread with no queue relieves it inside the write itself, and
/// otherwise this waits until another thread compacts.
///
/// ```
/// # use regolith::{Db, Options, through_stalls};
/// # let db = Db::open("rows", Options::memory())?;
/// through_stalls(|| db.put(b"key", b"value"))?;
/// # assert_eq!(db.get(b"key")?.as_deref(), Some(&b"value"[..]));
/// # Ok::<(), regolith::Error>(())
/// ```
#[cfg(not(target_arch = "wasm32"))]
pub fn through_stalls<T>(mut call: impl FnMut() -> crate::Result<T>) -> crate::Result<T> {
    loop {
        match call() {
            Err(crate::Error::WouldBlock(WouldBlock::Stall(stall))) => stall.wait(),
            other => return other,
        }
    }
}

/// Whether `err` carries a [`WouldBlock`].
pub(crate) fn is_would_block(err: &std::io::Error) -> bool {
    err.kind() == std::io::ErrorKind::WouldBlock
        && err.get_ref().is_some_and(|inner| inner.is::<WouldBlock>())
}

/// The [`WouldBlock`] `err` carries, taken out by value, or `err` back.
pub(crate) fn take_would_block(err: std::io::Error) -> Result<WouldBlock, std::io::Error> {
    if !is_would_block(&err) {
        return Err(err);
    }
    match err.into_inner().map(|inner| inner.downcast::<WouldBlock>()) {
        Some(Ok(would_block)) => Ok(*would_block),
        Some(Err(inner)) => Err(std::io::Error::new(std::io::ErrorKind::WouldBlock, inner)),
        None => Err(std::io::Error::from(std::io::ErrorKind::WouldBlock)),
    }
}

/// A copy of the [`WouldBlock`] `err` carries: a clone of its wait.
pub(crate) fn copy_would_block(err: &std::io::Error) -> Option<WouldBlock> {
    if err.kind() != std::io::ErrorKind::WouldBlock {
        return None;
    }
    err.get_ref()?.downcast_ref::<WouldBlock>().cloned()
}
