//! `close` on another thread leaves a transaction whose commit began alone.
//!
//! The committing thread is held inside a `before_commit` callback, after an
//! earlier callback registered the transaction's first `on_abort`, while a
//! second thread closes the database. Every wait is bounded, so a hang fails
//! the test instead of the run. The same race swept from inside the commit,
//! where the commit then goes on to succeed, is in
//! `src/transaction/commit_claim_tests.rs`.

// Native-only. wasm-pack builds every test target for wasm32, and this uses
// threads and the filesystem.
#![cfg(not(target_arch = "wasm32"))]

use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use regolith::{
    AbortReason, Error, OptimisticTransactionDb, Options, TransactionError, TxnOptions,
};
use tempfile::TempDir;

const BOUND: Duration = Duration::from_secs(30);

type Log = Arc<Mutex<Vec<String>>>;

fn note(log: &Log, entry: impl Into<String>) {
    log.lock().unwrap().push(entry.into());
}

fn entries(log: &Log) -> Vec<String> {
    log.lock().unwrap().clone()
}

#[test]
fn close_does_not_abort_a_transaction_that_is_committing() {
    let dir = TempDir::new().unwrap();
    let db = Arc::new(OptimisticTransactionDb::open(dir.path(), Options::default()).unwrap());
    let log = Log::default();
    // What the log held when the pause ended, after `close` returned.
    let seen = Log::default();

    let mut txn = db.begin(&TxnOptions::new());
    txn.put(b"k", b"v").unwrap();
    let l = Arc::clone(&log);
    txn.before_commit(move |txn| {
        let abort = Arc::clone(&l);
        txn.on_abort(move |why| {
            note(
                &abort,
                match why {
                    AbortReason::Closed => "abort:closed",
                    AbortReason::Error(_) => "abort:error",
                    _ => "abort:other",
                },
            )
        });
        Ok(())
    });
    let (ready_tx, ready_rx) = channel();
    let (closed_tx, closed_rx) = channel();
    let (l, s) = (Arc::clone(&log), Arc::clone(&seen));
    txn.before_commit(move |_| {
        let _ = ready_tx.send(());
        let _ = closed_rx.recv_timeout(BOUND);
        *s.lock().unwrap() = entries(&l);
        Ok(())
    });
    let l = Arc::clone(&log);
    txn.on_commit(move |_| note(&l, "commit"));

    let closer = {
        let db = Arc::clone(&db);
        std::thread::spawn(move || {
            ready_rx.recv_timeout(BOUND).expect("the commit paused");
            db.db().close().unwrap();
            closed_tx.send(()).unwrap();
        })
    };
    let result = txn.commit();
    closer.join().unwrap();

    assert_eq!(
        entries(&seen),
        Vec::<String>::new(),
        "close ran the abort callbacks of a transaction that was committing"
    );
    // The database is closed under the commit, so the commit fails, and fails
    // once: its abort callbacks run once, on this thread.
    assert!(
        matches!(result, Err(TransactionError::Engine(Error::Closed))),
        "{result:?}"
    );
    assert_eq!(entries(&log), ["abort:closed"]);
}
