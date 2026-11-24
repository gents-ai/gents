//! Which queue is this thread's, per database (D53).
//!
//! I/O regolith starts itself (the flush a write left owing with no worker,
//! the step a stall needs, a foreground job with no worker) goes to the
//! queue of the thread whose call started it. A plain call such as
//! `Db::put` carries no handle that names a queue, so the thread remembers
//! its queue: [`IoQueue`](crate::IoQueue) binds itself to the thread that
//! made it, and again to each thread that polls it, so a queue moved to the
//! thread that owns it is found there.
//!
//! The binding is a short list per thread, one entry per database (keyed by
//! the address of its unit table), holding at most [`MAX_BINDINGS`] entries:
//! a thread that binds more databases than that forgets the one it bound
//! longest ago, and a call on it for that database runs its I/O inline, as a
//! call with no queue does. An entry for a database that is gone, or a queue
//! that was dropped, names a queue its database no longer finds, so it reads
//! as no queue.

use std::cell::RefCell;

use crate::io_queue::QueueId;

/// Databases one thread remembers a queue for.
const MAX_BINDINGS: usize = 8;

thread_local! {
    /// `(database, queue)` pairs, most recently bound last.
    static BOUND: RefCell<Vec<(usize, QueueId)>> = const { RefCell::new(Vec::new()) };
}

/// Bind `queue` to this thread for the database whose unit table lives at
/// `database`.
pub(crate) fn bind(database: usize, queue: QueueId) {
    BOUND.with(|bound| {
        let mut bound = bound.borrow_mut();
        if let Some(at) = bound.iter().position(|(db, _)| *db == database) {
            if bound[at].1 == queue {
                return;
            }
            bound.remove(at);
        } else if bound.len() == MAX_BINDINGS {
            bound.remove(0);
        }
        bound.push((database, queue));
    });
}

/// The queue this thread bound for `database`, if any.
pub(crate) fn bound(database: usize) -> Option<QueueId> {
    BOUND.with(|bound| {
        bound
            .borrow()
            .iter()
            .find(|(db, _)| *db == database)
            .map(|(_, queue)| *queue)
    })
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;
    use std::num::NonZeroU64;

    fn id(n: u64) -> QueueId {
        QueueId::new(NonZeroU64::new(n).unwrap())
    }

    #[test]
    fn a_thread_finds_the_queue_it_bound_last_per_database() {
        bind(1, id(10));
        bind(2, id(20));
        bind(1, id(11));
        assert_eq!(bound(1), Some(id(11)));
        assert_eq!(bound(2), Some(id(20)));
        assert_eq!(bound(3), None);
        std::thread::spawn(|| assert_eq!(bound(1), None))
            .join()
            .unwrap();
    }

    #[test]
    fn the_list_forgets_the_oldest_database_past_its_bound() {
        for db in 100..100 + MAX_BINDINGS + 1 {
            bind(db, id(db as u64));
        }
        assert_eq!(bound(100), None);
        assert_eq!(bound(101), Some(id(101)));
        assert_eq!(
            bound(100 + MAX_BINDINGS),
            Some(id((100 + MAX_BINDINGS) as u64))
        );
    }
}
