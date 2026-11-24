//! `Transaction::take`: the transaction moves out whole, and what it leaves
//! in its place holds nothing and runs nothing when it ends.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use regolith::{
    AbortReason, CommitInfo, OptimisticTransactionDb, Options, TransactionHooks, TxnOptions,
};

#[derive(Default)]
struct Ends {
    commits: AtomicUsize,
    aborts: AtomicUsize,
}

struct Counting(Arc<Ends>);

impl TransactionHooks for Counting {
    fn on_commit(&self, _info: &CommitInfo) {
        self.0.commits.fetch_add(1, Ordering::SeqCst);
    }

    fn on_abort(&self, _reason: &AbortReason<'_>) {
        self.0.aborts.fetch_add(1, Ordering::SeqCst);
    }
}

fn open() -> (OptimisticTransactionDb, Arc<Ends>) {
    let ends = Arc::new(Ends::default());
    let options = Options::memory().transaction_hooks(Arc::new(Counting(Arc::clone(&ends))));
    (
        OptimisticTransactionDb::open("take", options).unwrap(),
        ends,
    )
}

/// The taken transaction keeps its writes, its snapshot and its one end; the
/// stand-in ends without a hook, whether rolled back or dropped.
#[test]
fn a_taken_transaction_keeps_everything_and_its_stand_in_runs_nothing() {
    let (db, ends) = open();
    db.db().put(b"seen", b"before").unwrap();
    let mut held = db.begin(&TxnOptions::new());
    held.put(b"written", b"one").unwrap();
    db.db().put(b"seen", b"after").unwrap();

    let taken = held.take();
    held.rollback();
    assert_eq!(ends.aborts.load(Ordering::SeqCst), 0);
    assert_eq!(taken.get(b"seen").unwrap().as_deref(), Some(&b"before"[..]));
    assert_eq!(taken.get(b"written").unwrap().as_deref(), Some(&b"one"[..]));
    taken.commit().unwrap();
    assert_eq!(
        db.db().get(b"written").unwrap().as_deref(),
        Some(&b"one"[..])
    );
    assert_eq!(ends.commits.load(Ordering::SeqCst), 1);

    let mut held = db.begin(&TxnOptions::new());
    let taken = held.take();
    drop(held);
    taken.rollback();
    assert_eq!(ends.aborts.load(Ordering::SeqCst), 1);
}

/// Putting the taken transaction back drops the stand-in silently, and the
/// transaction commits from where it was put back.
#[test]
fn a_transaction_put_back_commits_from_its_place() {
    let (db, ends) = open();
    let mut held = db.begin(&TxnOptions::new());
    let taken = held.take();
    taken.put(b"key", b"value").unwrap();
    held = taken;
    held.commit().unwrap();
    assert_eq!(db.db().get(b"key").unwrap().as_deref(), Some(&b"value"[..]));
    assert_eq!(
        (
            ends.commits.load(Ordering::SeqCst),
            ends.aborts.load(Ordering::SeqCst)
        ),
        (1, 0)
    );
}
