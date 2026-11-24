//! When a callback registered from inside another callback takes effect.
//!
//! An `on_abort` callback that a running `before_commit` callback or hook
//! registers counts as registered when that callback returns `Ok`, and a
//! callback that fails takes its registrations back. `close` on another
//! thread takes the `on_abort` callbacks registered so far and runs them, so
//! the half-done registrations of a callback still running are never in what
//! it takes. The same rules for every other kind are in
//! `tests/transaction_callbacks.rs`.

// Native-only. wasm-pack builds every test target for wasm32, and this uses
// the filesystem.
#![cfg(not(target_arch = "wasm32"))]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use regolith::{
    AbortReason, CommitInfo, Db, OptimisticTransactionDb, Options, Transaction, TransactionError,
    TransactionHooks, TxResult, TxnOptions,
};
use tempfile::TempDir;

type Log = Arc<Mutex<Vec<String>>>;

fn note(log: &Log, entry: impl Into<String>) {
    log.lock().unwrap().push(entry.into());
}

fn entries(log: &Log) -> Vec<String> {
    log.lock().unwrap().clone()
}

fn reason(reason: &AbortReason<'_>) -> &'static str {
    match reason {
        AbortReason::Rollback => "rollback",
        AbortReason::Dropped => "dropped",
        AbortReason::Conflict(_) => "conflict",
        AbortReason::Error(_) => "error",
        AbortReason::Closed => "closed",
        AbortReason::CallbackPanicked => "panicked",
        _ => "other",
    }
}

struct Fixture {
    db: Arc<OptimisticTransactionDb>,
    _dir: TempDir,
}

impl Fixture {
    fn open(options: Options) -> Self {
        let dir = TempDir::new().unwrap();
        let db = OptimisticTransactionDb::open(dir.path(), options).unwrap();
        Self {
            db: Arc::new(db),
            _dir: dir,
        }
    }

    fn db(&self) -> &Db {
        self.db.db()
    }

    fn begin(&self) -> Transaction {
        self.db.begin(&TxnOptions::new())
    }
}

/// Registers an `on_abort` callback that notes `name` and why.
fn abort_noting(txn: &mut Transaction, log: &Log, name: &'static str) {
    let log = Arc::clone(log);
    txn.on_abort(move |why| note(&log, format!("{name}:{}", reason(why))));
}

#[test]
fn a_registration_made_by_a_callback_joins_the_claim_when_the_callback_returns() {
    let fixture = Fixture::open(Options::default());
    let log = Log::default();
    let mut txn = fixture.begin();
    abort_noting(&mut txn, &log, "outside");
    let (l, db) = (Arc::clone(&log), Arc::clone(&fixture.db));
    txn.before_commit(move |txn| {
        abort_noting(txn, &l, "inside");
        note(&l, "registered");
        // `close` takes the callbacks registered so far, on this thread.
        db.db().close().unwrap();
        note(&l, "closed");
        Ok(())
    });
    txn.prepare().unwrap();
    assert_eq!(
        entries(&log),
        ["registered", "outside:closed", "closed"],
        "close must not see what the running callback registered"
    );
    let result = txn.commit();
    assert!(
        matches!(
            result,
            Err(TransactionError::Engine(regolith::Error::Closed))
        ),
        "{result:?}"
    );
    assert_eq!(
        entries(&log),
        ["registered", "outside:closed", "closed", "inside:closed"],
        "what the callback registered runs once, at the owner's end"
    );
}

#[test]
fn a_callback_that_succeeds_keeps_its_registrations_when_the_commit_fails() {
    let fixture = Fixture::open(Options::default());
    let log = Log::default();
    let mut txn = fixture.begin();
    let l = Arc::clone(&log);
    txn.before_commit(move |txn| {
        abort_noting(txn, &l, "first");
        Ok(())
    });
    txn.before_commit(|_| Err(TransactionError::NoSavepoint));
    let result = txn.commit();
    assert!(matches!(result, Err(TransactionError::NoSavepoint)));
    assert_eq!(entries(&log), ["first:error"]);
}

#[test]
fn nested_registrations_run_in_the_order_made_and_a_failing_outer_callback_takes_them_all_back() {
    for outer_fails in [false, true] {
        let fixture = Fixture::open(Options::default());
        let log = Log::default();
        let mut txn = fixture.begin();
        let l = Arc::clone(&log);
        txn.before_commit(move |txn| {
            abort_noting(txn, &l, "outer");
            let inner_log = Arc::clone(&l);
            txn.before_commit(move |txn| {
                abort_noting(txn, &inner_log, "inner");
                Ok(())
            });
            // Runs the inner callback inside this one.
            txn.prepare()?;
            if outer_fails {
                return Err(TransactionError::NoSavepoint);
            }
            Ok(())
        });
        assert_eq!(txn.prepare().is_err(), outer_fails);
        txn.rollback();
        assert_eq!(
            entries(&log),
            if outer_fails {
                Vec::<String>::new()
            } else {
                vec!["outer:rollback".to_string(), "inner:rollback".into()]
            },
            "outer_fails: {outer_fails}"
        );
    }
}

struct RegisteringHook {
    log: Log,
}

impl TransactionHooks for RegisteringHook {
    fn before_commit(&self, txn: &mut Transaction) -> TxResult<()> {
        abort_noting(txn, &self.log, "hook");
        Ok(())
    }

    fn on_commit(&self, _: &CommitInfo) {}
}

#[test]
fn a_registration_made_by_the_hook_runs_when_the_commit_conflicts() {
    let log = Log::default();
    let fixture = Fixture::open(
        Options::default().transaction_hooks(Arc::new(RegisteringHook {
            log: Arc::clone(&log),
        })),
    );
    fixture.db().put(b"k", b"0").unwrap();
    let txn = fixture.begin();
    txn.get_for_update(b"k").unwrap();
    txn.put(b"k", b"1").unwrap();
    fixture.db().put(b"k", b"other").unwrap();
    let result = txn.commit();
    assert!(
        matches!(result, Err(TransactionError::Conflict(_))),
        "{result:?}"
    );
    assert_eq!(entries(&log), ["hook:conflict"]);
}

#[test]
fn close_racing_the_owners_registrations_runs_each_callback_exactly_once() {
    const REGISTERED: usize = 200;
    for round in 0..40 {
        let fixture = Fixture::open(Options::default());
        let ran = Arc::new(AtomicUsize::new(0));
        let mut txn = fixture.begin();
        std::thread::scope(|scope| {
            let closer = scope.spawn(|| fixture.db().close().unwrap());
            for i in 0..REGISTERED {
                let ran = Arc::clone(&ran);
                txn.on_abort(move |_| {
                    ran.fetch_add(1, Ordering::SeqCst);
                });
                if (i + round) % 50 == 0 {
                    std::thread::yield_now();
                }
            }
            closer.join().unwrap();
        });
        drop(txn);
        assert_eq!(ran.load(Ordering::SeqCst), REGISTERED, "round {round}");
    }
}
