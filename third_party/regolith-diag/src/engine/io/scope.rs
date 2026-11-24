//! How a handle's [`ReadMode`](crate::ReadMode) reaches the device seam.
//!
//! A `CacheOnly` handle names a queue, and every read path below it has to
//! know that at the one place it would touch the device: the four block
//! reads of `SsTableReader` and the scan readahead. Those sit far below the
//! handle, behind every read path's own signatures, so the mode travels in a
//! thread-local set for the length of one call instead of as a parameter.
//!
//! That is exact because a read call runs synchronously on the thread that
//! made it: nothing between the handle and the seam hands work to another
//! thread, and a scope ends (on return or unwind) before the call does. A
//! `Blocking` handle sets nothing, so the default path costs nothing at all:
//! the seam reads the thread-local only after the block cache missed, where
//! a device read would cost far more than the read of one word.
//!
//! A `CacheOnly` scope is also one call, which a miss leaves to be run
//! again. A call that reads a block from what landed for its queue notes
//! it ([`note_hit`]), a wait it records carries the blocks the call read so
//! far ([`note_wait`]), and a call that ends having read landed blocks
//! without waiting tells its queue which ([`Message::Finished`]). The queue
//! keeps those blocks landed while the call may need them again
//! (`io_queue::ledger`), so a call that needs more blocks at once than the
//! queue's bound still finishes. A call that touches no landing pays one
//! bit test when its scope ends.
//!
//! [`unit_run`] clears the mode for the stretch a unit run by
//! `IoQueue::poll` reads the device, whoever calls it, and marks that
//! stretch as one that may not wait for an open-file slot (D60): a reopen
//! under `max_open_files` that finds every slot busy parks on the queue
//! running the unit instead, and the run sees that it did.
//!
//! [`no_wait`] marks the stretch a queue's poll steps a job, which parks
//! instead of waiting for another thread (`engine::commit::park`). Every
//! path of the engine that waits for another thread calls [`waits`] first:
//! where a thread goes to sleep or spins for one, and where a call that
//! chose to block takes something another thread may hold (a round or a
//! unit in its way, the version store's step, the flush exclusion, the
//! compaction gate), whether or not anything is in its way this time. So
//! the check does not depend on a race being lost while a test runs. Such
//! a path inside the stretch is a defect: the job should have parked. In a
//! build with debug assertions the stretch fails when it ends, naming the
//! path. Not at the wait itself, which may sit where unwinding is not safe
//! (a section handed to `engine::step` borrows from the frame that waits
//! for it).

use std::cell::{Cell, RefCell};
use std::num::NonZeroU64;
use std::sync::Arc;

use super::shared::{Message, QueueShared};
use super::unit::UnitKey;
use crate::env::open_file_limit::slots::SlotWaiter;
use crate::io_queue::QueueId;

/// Set in `QUEUE` once the call in this thread's `CacheOnly` scope read a
/// landed block or recorded a wait, so its end has something to tell. Queue
/// ids count up from one and never reach it.
const TOUCHED: u64 = 1 << 63;

thread_local! {
    /// The queue misses go to on this thread right now; `0` is `Blocking`.
    /// [`TOUCHED`] rides in the top bit.
    static QUEUE: Cell<u64> = const { Cell::new(0) };
    /// What the `CacheOnly` call on this thread did with its queue, once
    /// [`TOUCHED`] is set.
    static CALL: RefCell<Call> = const {
        RefCell::new(Call {
            queue: None,
            read: Vec::new(),
            waits: false,
        })
    };
    /// The queue whose unit this thread is running, while it runs one.
    static RUNNING: RefCell<Option<Running>> = const { RefCell::new(None) };
    /// This thread is inside a [`no_wait`] stretch.
    static NO_WAIT: Cell<bool> = const { Cell::new(false) };
    /// What this thread waited for inside its [`no_wait`] stretch, if it
    /// did: the first such wait.
    static WAITED: Cell<Option<&'static str>> = const { Cell::new(None) };
}

/// A stretch that must not wait for another thread, until it is dropped.
#[must_use = "the stretch lasts only while the scope is alive"]
pub(crate) struct NoWait {
    previous: bool,
}

impl Drop for NoWait {
    fn drop(&mut self) {
        NO_WAIT.with(|no_wait| no_wait.set(self.previous));
        if self.previous {
            // The outermost stretch reports.
            return;
        }
        let waited = WAITED.with(Cell::take);
        if cfg!(debug_assertions)
            && !std::thread::panicking()
            && let Some(what) = waited
        {
            panic!("a queue's poll took a path that waits for another thread: {what}");
        }
    }
}

/// Mark what this thread does until the scope drops as a stretch that must
/// not wait for another thread: a job stepped by a queue's poll
/// (`Job::step`).
pub(crate) fn no_wait() -> NoWait {
    NoWait {
        previous: NO_WAIT.with(|no_wait| no_wait.replace(true)),
    }
}

/// This thread is on a path that waits for another one, for `what`: about
/// to sleep or spin, or taking, as a call that blocks, something another
/// thread may hold. Inside a [`no_wait`] stretch that is a defect, noted
/// here and reported when the stretch ends. A loom model's threads share
/// the host's thread-locals, so nothing is noted there.
#[inline]
pub(crate) fn waits(what: &'static str) {
    if cfg!(all(debug_assertions, not(loom))) && NO_WAIT.with(Cell::get) {
        WAITED.with(|waited| {
            if waited.get().is_none() {
                waited.set(Some(what));
            }
        });
    }
}

/// One `CacheOnly` call's dealings with its queue.
struct Call {
    /// The queue it read landed blocks from.
    queue: Option<Arc<QueueShared>>,
    /// The landed blocks it read, in the order it read them. Kept across
    /// calls for its allocation, and cleared when a call ends.
    read: Vec<UnitKey>,
    /// It recorded a wait: it will be run again, and the wait carries what
    /// it read.
    waits: bool,
}

/// A unit run in progress on this thread.
struct Running {
    queue: Arc<QueueShared>,
    /// A reopen in this run found every open-file slot busy and parked.
    parked: bool,
}

/// A mode set on this thread until it is dropped, which restores the one
/// before it.
#[must_use = "the mode holds only while the scope is alive"]
pub(crate) struct Scope {
    previous: u64,
}

impl Drop for Scope {
    fn drop(&mut self) {
        let ended = QUEUE.with(|queue| queue.replace(self.previous));
        if ended & TOUCHED != 0 {
            if self.previous & !TOUCHED == 0 {
                // The outermost `CacheOnly` scope: its call is over.
                end_call();
            } else {
                // A nested scope: the call around it goes on, and carries it.
                QUEUE.with(|queue| queue.set(self.previous | TOUCHED));
            }
        }
    }
}

/// Mark the call in this thread's `CacheOnly` scope as one with something
/// to tell when it ends. `false` outside such a scope.
fn touch() -> bool {
    QUEUE.with(|queue| {
        let mode = queue.get();
        if mode & !TOUCHED == 0 {
            return false;
        }
        queue.set(mode | TOUCHED);
        true
    })
}

/// The call in this thread's `CacheOnly` scope read `key` from what landed
/// for `queue`.
pub(crate) fn note_hit(queue: &Arc<QueueShared>, key: UnitKey) {
    if !touch() {
        return;
    }
    CALL.with(|call| {
        let mut call = call.borrow_mut();
        if call.queue.is_none() {
            call.queue = Some(Arc::clone(queue));
        }
        // A block read twice in a row (an index walked per entry) is noted
        // once; `held` drops the other repeats.
        if call.read.last() != Some(&key) {
            call.read.push(key);
        }
    });
}

/// The call in this thread's `CacheOnly` scope records a wait, and will be
/// run again: the landed blocks it read so far, sorted, which the wait
/// carries so its queue keeps them until then. Empty outside such a scope.
pub(crate) fn note_wait() -> Box<[UnitKey]> {
    if !touch() {
        return Box::default();
    }
    CALL.with(|call| {
        let mut call = call.borrow_mut();
        call.waits = true;
        held(&call.read)
    })
}

/// `read`, sorted and without repeats.
fn held(read: &[UnitKey]) -> Box<[UnitKey]> {
    if read.is_empty() {
        return Box::default();
    }
    let mut held = read.to_vec();
    held.sort_unstable();
    held.dedup();
    held.into_boxed_slice()
}

/// The end of a `CacheOnly` call that read a landed block or recorded a
/// wait. One that read landed blocks and finished without a wait tells its
/// queue which, so the queue lets go of what it kept for the call.
#[cold]
#[inline(never)]
fn end_call() {
    CALL.with(|call| {
        let mut call = call.borrow_mut();
        let queue = call.queue.take();
        let waits = std::mem::take(&mut call.waits);
        // Only when the queue keeps a wait the call may have been: one
        // that keeps nothing has nothing to let go.
        if !waits
            && let Some(queue) = queue
            && queue.keeps()
        {
            // A queue dropped since refuses it, and keeps nothing.
            let _ = queue.deliver(Message::Finished(held(&call.read)));
        }
        call.read.clear();
    });
}

/// Send this thread's device misses to queue `id` until the scope drops.
pub(crate) fn cache_only(id: QueueId) -> Scope {
    Scope {
        previous: QUEUE.with(|queue| queue.replace(id.get())),
    }
}

/// Let this thread read the device until the scope drops: a job run by
/// `IoQueue::poll`, whoever calls it. A job never parks, so a reopen inside
/// it is a `Blocking` one.
pub(crate) fn blocking() -> Scope {
    Scope {
        previous: QUEUE.with(|queue| queue.replace(0)),
    }
}

/// The queue this thread's misses go to, or `None` when it may read the
/// device. The seam calls this only after the block cache missed.
pub(crate) fn current() -> Option<QueueId> {
    NonZeroU64::new(QUEUE.with(Cell::get) & !TOUCHED).map(QueueId::new)
}

/// The run of one unit on this thread, from [`unit_run`]: the device may be
/// read, but no slot may be waited for.
#[must_use = "the run holds only while the scope is alive"]
pub(crate) struct UnitRun {
    _device: Scope,
    previous: Option<Running>,
}

impl UnitRun {
    /// Whether a reopen in this run parked instead of reading.
    pub(crate) fn parked(&self) -> bool {
        RUNNING.with(|running| running.borrow().as_ref().is_some_and(|run| run.parked))
    }
}

impl Drop for UnitRun {
    fn drop(&mut self) {
        RUNNING.with(|running| *running.borrow_mut() = self.previous.take());
    }
}

/// Run a unit of `queue`'s on this thread until the scope drops: read the
/// device, and park a reopen that finds no open-file slot on `queue`.
pub(crate) fn unit_run(queue: &Arc<QueueShared>) -> UnitRun {
    let device = Scope {
        previous: QUEUE.with(|queue| queue.replace(0)),
    };
    let run = Running {
        queue: Arc::clone(queue),
        parked: false,
    };
    UnitRun {
        _device: device,
        previous: RUNNING.with(|running| running.borrow_mut().replace(run)),
    }
}

/// Who a reopen on this thread parks, when it may not wait for a slot: the
/// queue whose unit runs here. `None` outside a unit run, where the reopen
/// belongs to a `Blocking` read and may wait.
pub(crate) fn reopen_waiter() -> Option<Arc<dyn SlotWaiter>> {
    RUNNING.with(|running| {
        running
            .borrow()
            .as_ref()
            .map(|run| Arc::clone(&run.queue) as Arc<dyn SlotWaiter>)
    })
}

/// A reopen in the unit run on this thread parked.
pub(crate) fn note_parked() {
    RUNNING.with(|running| {
        if let Some(run) = running.borrow_mut().as_mut() {
            run.parked = true;
        }
    });
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;
    use crate::engine::io::shared::test_id;

    fn id(n: u64) -> QueueId {
        QueueId::new(NonZeroU64::new(n).unwrap())
    }

    fn queue() -> Arc<QueueShared> {
        Arc::new(QueueShared::new(test_id(), 1 << 20))
    }

    #[test]
    fn scopes_nest_and_restore_what_they_replaced() {
        assert_eq!(current(), None);
        {
            let _outer = cache_only(id(7));
            assert_eq!(current(), Some(id(7)));
            {
                let _inner = unit_run(&queue());
                assert_eq!(current(), None);
            }
            assert_eq!(current(), Some(id(7)));
        }
        assert_eq!(current(), None);
    }

    #[test]
    fn an_unwind_restores_the_mode() {
        let caught = std::panic::catch_unwind(|| {
            let _scope = cache_only(id(3));
            let _run = unit_run(&queue());
            panic!("unwind through the scope");
        });
        assert!(caught.is_err());
        assert_eq!(current(), None);
        assert!(reopen_waiter().is_none());
    }

    fn unit_key(offset: u64) -> UnitKey {
        UnitKey {
            file_id: 1,
            offset,
            guard: None,
        }
    }

    /// What `queue`'s inbox says the calls that ended told it.
    fn finished(queue: &QueueShared) -> Vec<Vec<UnitKey>> {
        queue
            .take_inbox()
            .filter_map(|message| match message {
                Message::Finished(read) => Some(read.into_vec()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_call_carries_what_it_read_into_its_wait_and_no_further() {
        let q = queue();
        let (a, b) = (unit_key(0), unit_key(9));
        // Outside a `CacheOnly` scope nothing is noted.
        note_hit(&q, a);
        {
            let _scope = cache_only(id(4));
            note_hit(&q, b);
            note_hit(&q, a);
            note_hit(&q, a);
            // Sorted, once each, and only this call's.
            assert_eq!(&*note_wait(), &[a, b]);
        }
        {
            let _scope = cache_only(id(4));
            assert!(note_wait().is_empty(), "a call starts with nothing read");
        }
        assert_eq!(current(), None);
    }

    #[test]
    fn a_call_that_finishes_tells_its_queue_only_when_it_keeps_a_wait() {
        let q = queue();
        let a = unit_key(0);
        {
            let _scope = cache_only(id(4));
            note_hit(&q, a);
        }
        assert!(finished(&q).is_empty(), "nothing kept, nothing told");
        q.set_kept(1);
        {
            let _scope = cache_only(id(4));
            note_hit(&q, a);
        }
        assert_eq!(finished(&q), vec![vec![a]]);
        {
            let _scope = cache_only(id(4));
            note_hit(&q, a);
            let _ = note_wait();
        }
        assert!(finished(&q).is_empty(), "a call that waits will run again");
    }

    #[test]
    fn a_nested_scope_hands_what_it_did_to_the_call_around_it() {
        let q = queue();
        q.set_kept(1);
        let (a, b) = (unit_key(0), unit_key(9));
        {
            let _outer = cache_only(id(4));
            {
                let _device = blocking();
                // A unit run inside the call reads the device: nothing noted.
                note_hit(&q, b);
            }
            {
                let _inner = cache_only(id(4));
                note_hit(&q, a);
            }
            assert!(finished(&q).is_empty(), "the call is not over");
            assert_eq!(current(), Some(id(4)));
        }
        assert_eq!(finished(&q), vec![vec![a]]);
    }

    #[test]
    fn the_mode_is_per_thread() {
        let _scope = cache_only(id(9));
        let _run = unit_run(&queue());
        std::thread::spawn(|| {
            assert_eq!(current(), None);
            assert!(reopen_waiter().is_none());
        })
        .join()
        .unwrap();
        assert!(reopen_waiter().is_some());
    }

    #[test]
    fn only_a_unit_run_parks_a_reopen_and_sees_that_it_did() {
        assert!(reopen_waiter().is_none(), "a Blocking read may wait");
        note_parked();
        let run = unit_run(&queue());
        assert!(reopen_waiter().is_some());
        assert!(!run.parked());
        note_parked();
        assert!(run.parked());
        drop(run);
        assert!(reopen_waiter().is_none());
        // A new run starts unparked.
        assert!(!unit_run(&queue()).parked());
    }
}
