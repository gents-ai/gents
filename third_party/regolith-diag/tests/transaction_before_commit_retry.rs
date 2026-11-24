//! A `before_commit` callback is `FnMut`, and one that fails stays queued.
//!
//! A callback may mutate what it captured. When it returns an error its
//! effects (writes, appends, savepoints and registrations) are taken back and
//! it keeps its place at the front of the queue, so a later
//! [`Transaction::prepare`], or the commit, runs it again; one that panics is
//! dropped. `on_commit` and `on_abort` callbacks run exactly once whichever
//! way the commit goes; `tests/transaction_callbacks.rs` has their laws.

// Native-only. wasm-pack builds every test target for wasm32, and this uses
// the filesystem.
#![cfg(not(target_arch = "wasm32"))]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use regolith::{
    AbortReason, Db, Error, LogLayout, OptimisticTransactionDb, Options, Transaction,
    TransactionDb, TransactionError, TransactionHooks, TxResult, TxnOptions,
};
use tempfile::TempDir;

type Log = Arc<Mutex<Vec<String>>>;

fn note(log: &Log, entry: impl Into<String>) {
    log.lock().unwrap().push(entry.into());
}

fn entries(log: &Log) -> Vec<String> {
    log.lock().unwrap().clone()
}

/// Both transaction flavors behind one handle.
enum Fixture {
    Optimistic {
        db: OptimisticTransactionDb,
        _dir: TempDir,
    },
    Pessimistic {
        db: TransactionDb,
        _dir: TempDir,
    },
}

impl Fixture {
    fn both() -> [Self; 2] {
        let (a, b) = (TempDir::new().unwrap(), TempDir::new().unwrap());
        [
            Self::Optimistic {
                db: OptimisticTransactionDb::open(a.path(), Options::default()).unwrap(),
                _dir: a,
            },
            Self::Pessimistic {
                db: TransactionDb::open(b.path(), Options::default())
                    .unwrap()
                    .with_lock_timeout(Duration::from_millis(20)),
                _dir: b,
            },
        ]
    }

    fn db(&self) -> &Db {
        match self {
            Self::Optimistic { db, .. } => db.db(),
            Self::Pessimistic { db, .. } => db.db(),
        }
    }

    fn begin(&self) -> Transaction {
        match self {
            Self::Optimistic { db, .. } => db.begin(&TxnOptions::new()),
            Self::Pessimistic { db, .. } => db.begin(&TxnOptions::new()),
        }
    }
}

#[test]
fn a_before_commit_that_fails_once_commits_on_the_second_prepare() {
    for fixture in Fixture::both() {
        let runs = Arc::new(AtomicUsize::new(0));
        let mut txn = fixture.begin();
        let counted = Arc::clone(&runs);
        let mut failures_left = 1;
        txn.before_commit(move |txn| {
            counted.fetch_add(1, Ordering::SeqCst);
            txn.put(b"callback", b"wrote")?;
            if failures_left > 0 {
                failures_left -= 1;
                return Err(TransactionError::NoSavepoint);
            }
            Ok(())
        });

        assert!(matches!(txn.prepare(), Err(TransactionError::NoSavepoint)));
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        assert_eq!(
            txn.get(b"callback").unwrap(),
            None,
            "the failed run's write was taken back"
        );

        txn.prepare().unwrap();
        assert_eq!(runs.load(Ordering::SeqCst), 2);
        txn.prepare().unwrap();
        assert_eq!(runs.load(Ordering::SeqCst), 2, "a success is not repeated");

        txn.commit().unwrap();
        assert_eq!(
            runs.load(Ordering::SeqCst),
            2,
            "commit does not run it again"
        );
        assert_eq!(
            fixture.db().get(b"callback").unwrap().as_deref(),
            Some(&b"wrote"[..])
        );
    }
}

#[test]
fn a_failed_callback_keeps_its_place_ahead_of_the_ones_queued_behind_it() {
    for fixture in Fixture::both() {
        let log = Log::default();
        let mut txn = fixture.begin();
        let mut failures_left = 2;
        let l = Arc::clone(&log);
        txn.before_commit(move |_| {
            if failures_left > 0 {
                failures_left -= 1;
                note(&l, "first:fails");
                return Err(TransactionError::NoSavepoint);
            }
            note(&l, "first");
            Ok(())
        });
        for name in ["second", "third"] {
            let l = Arc::clone(&log);
            txn.before_commit(move |_| {
                note(&l, name);
                Ok(())
            });
        }
        assert!(txn.prepare().is_err());
        assert!(txn.prepare().is_err());
        assert!(entries(&log).iter().all(|entry| entry == "first:fails"));
        txn.prepare().unwrap();
        assert_eq!(
            entries(&log),
            ["first:fails", "first:fails", "first", "second", "third"]
        );
    }
}

#[test]
fn a_failed_callbacks_registrations_are_taken_back_before_it_runs_again() {
    for fixture in Fixture::both() {
        let log = Log::default();
        let mut txn = fixture.begin();
        let mut failures_left = 3;
        let l = Arc::clone(&log);
        txn.before_commit(move |txn| {
            let (commit, abort, nested) = (Arc::clone(&l), Arc::clone(&l), Arc::clone(&l));
            txn.on_commit(move |_| note(&commit, "on_commit"));
            txn.on_abort(move |_| note(&abort, "on_abort"));
            txn.before_commit(move |_| {
                note(&nested, "nested");
                Ok(())
            });
            txn.set_savepoint();
            if failures_left > 0 {
                failures_left -= 1;
                return Err(TransactionError::NoSavepoint);
            }
            Ok(())
        });
        for _ in 0..3 {
            assert!(txn.prepare().is_err());
        }
        txn.prepare().unwrap();
        assert_eq!(entries(&log), ["nested"], "one nested callback, not four");
        txn.release_savepoint().unwrap();
        assert!(
            matches!(txn.release_savepoint(), Err(TransactionError::NoSavepoint)),
            "one savepoint, not four"
        );
        txn.commit().unwrap();
        assert_eq!(entries(&log), ["nested", "on_commit"]);
    }
}

#[test]
fn a_commit_runs_a_callback_a_failed_prepare_left_queued() {
    for fixture in Fixture::both() {
        let mut txn = fixture.begin();
        let mut failures_left = 1;
        txn.before_commit(move |txn| {
            txn.put(b"k", b"v")?;
            if failures_left > 0 {
                failures_left -= 1;
                return Err(TransactionError::NoSavepoint);
            }
            Ok(())
        });
        assert!(txn.prepare().is_err());
        txn.commit().unwrap();
        assert_eq!(fixture.db().get(b"k").unwrap().as_deref(), Some(&b"v"[..]));
    }
}

#[test]
fn a_callback_that_keeps_failing_fails_the_commit_and_is_dropped_with_the_transaction() {
    for fixture in Fixture::both() {
        let log = Log::default();
        let held = Arc::new(());
        let mut txn = fixture.begin();
        let (keep, l) = (Arc::clone(&held), Arc::clone(&log));
        txn.before_commit(move |_| {
            let _ = &keep;
            note(&l, "before");
            Err(TransactionError::NoSavepoint)
        });
        let l = Arc::clone(&log);
        txn.on_abort(move |why| {
            note(
                &l,
                match why {
                    AbortReason::Error(_) => "abort:error",
                    _ => "abort:other",
                },
            )
        });
        let l = Arc::clone(&log);
        txn.on_commit(move |_| note(&l, "commit"));

        assert!(txn.prepare().is_err());
        assert_eq!(Arc::strong_count(&held), 2, "still queued after a failure");
        assert!(matches!(txn.commit(), Err(TransactionError::NoSavepoint)));
        assert_eq!(entries(&log), ["before", "before", "abort:error"]);
        assert_eq!(Arc::strong_count(&held), 1, "dropped with the transaction");
    }
}

#[test]
fn a_callback_that_panics_runs_once_and_is_dropped() {
    for fixture in Fixture::both() {
        let runs = Arc::new(AtomicUsize::new(0));
        let held = Arc::new(());
        let mut txn = fixture.begin();
        let (counted, keep) = (Arc::clone(&runs), Arc::clone(&held));
        txn.before_commit(move |_| {
            let _ = &keep;
            counted.fetch_add(1, Ordering::SeqCst);
            panic!("boom");
        });
        for _ in 0..3 {
            assert!(matches!(
                txn.prepare(),
                Err(TransactionError::Engine(Error::CallbackPanicked { .. }))
            ));
        }
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        assert_eq!(Arc::strong_count(&held), 1, "dropped when it panicked");
    }
}

#[test]
fn a_callback_that_could_not_take_a_lock_runs_again_once_it_is_free() {
    let dir = TempDir::new().unwrap();
    let db = TransactionDb::open(dir.path(), Options::default())
        .unwrap()
        .with_lock_timeout(Duration::from_millis(20));
    let holder = db.begin(&TxnOptions::new());
    holder.put(b"contended", b"holder").unwrap();

    let mut txn = db.begin(&TxnOptions::new());
    let attempts = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&attempts);
    txn.before_commit(move |txn| {
        counted.fetch_add(1, Ordering::SeqCst);
        txn.put(b"contended", b"callback")
    });
    assert!(matches!(txn.prepare(), Err(TransactionError::Busy(_))));
    holder.commit().unwrap();

    txn.prepare().unwrap();
    txn.commit().unwrap();
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert_eq!(
        db.db().get(b"contended").unwrap().as_deref(),
        Some(&b"callback"[..])
    );
}

#[test]
fn a_callback_that_fails_after_running_the_ones_behind_it_runs_them_again() {
    for fixture in Fixture::both() {
        let log = Log::default();
        let mut txn = fixture.begin();
        let mut failures_left = 1;
        let l = Arc::clone(&log);
        txn.before_commit(move |txn| {
            note(&l, "first");
            // Runs `second`, queued behind this callback, inside it.
            txn.prepare()?;
            if failures_left > 0 {
                failures_left -= 1;
                return Err(TransactionError::NoSavepoint);
            }
            Ok(())
        });
        let l = Arc::clone(&log);
        txn.before_commit(move |txn| {
            note(&l, "second");
            txn.put(b"second", b"wrote")
        });

        assert!(matches!(txn.prepare(), Err(TransactionError::NoSavepoint)));
        assert_eq!(
            txn.get(b"second").unwrap(),
            None,
            "what ran inside the failed callback was taken back"
        );
        txn.commit().unwrap();
        assert_eq!(entries(&log), ["first", "second", "first", "second"]);
        assert_eq!(
            fixture.db().get(b"second").unwrap().as_deref(),
            Some(&b"wrote"[..])
        );
    }
}

struct CountingHook(AtomicUsize);

impl TransactionHooks for CountingHook {
    fn before_commit(&self, txn: &mut Transaction) -> TxResult<()> {
        self.0.fetch_add(1, Ordering::SeqCst);
        txn.put(b"hook", b"wrote")
    }
}

#[test]
fn a_hook_that_ran_inside_a_failed_callback_runs_again() {
    let hook = Arc::new(CountingHook(AtomicUsize::new(0)));
    let dir = TempDir::new().unwrap();
    let db = OptimisticTransactionDb::open(
        dir.path(),
        Options::default().transaction_hooks(Arc::clone(&hook) as Arc<dyn TransactionHooks>),
    )
    .unwrap();
    let mut txn = db.begin(&TxnOptions::new());
    let mut failures_left = 1;
    txn.before_commit(move |txn| {
        // The hook runs inside this callback, since nothing ran it yet.
        txn.prepare()?;
        if failures_left > 0 {
            failures_left -= 1;
            return Err(TransactionError::NoSavepoint);
        }
        Ok(())
    });
    assert!(txn.prepare().is_err());
    assert_eq!(txn.get(b"hook").unwrap(), None);
    txn.commit().unwrap();
    assert_eq!(hook.0.load(Ordering::SeqCst), 2);
    assert_eq!(
        db.db().get(b"hook").unwrap().as_deref(),
        Some(&b"wrote"[..])
    );
}

/// The transaction is poisoned once a callback panics, whether or not the
/// callback that met the panic passed it on.
fn assert_poisoned(mut txn: Transaction, log: &Log, fixture: &Fixture) {
    for _ in 0..2 {
        assert!(
            matches!(
                txn.prepare(),
                Err(TransactionError::Engine(Error::CallbackPanicked {
                    latched: false,
                    ..
                }))
            ),
            "a later prepare must fail"
        );
    }
    let result = txn.commit();
    assert!(
        matches!(
            result,
            Err(TransactionError::Engine(Error::CallbackPanicked {
                latched: false,
                ..
            }))
        ),
        "{result:?}"
    );
    assert_eq!(
        entries(log),
        ["abort:panicked"],
        "on_abort, once, and no on_commit"
    );
    assert_eq!(fixture.db().get(b"k").unwrap(), None, "nothing committed");
}

fn register_outcomes(txn: &mut Transaction, log: &Log) {
    let l = Arc::clone(log);
    txn.on_abort(move |why| {
        note(
            &l,
            if matches!(why, AbortReason::CallbackPanicked) {
                "abort:panicked"
            } else {
                "abort:other"
            },
        )
    });
    let l = Arc::clone(log);
    txn.on_commit(move |_| note(&l, "commit"));
    txn.put(b"k", b"v").unwrap();
}

#[test]
fn a_panic_a_callback_ignored_still_poisons_the_transaction() {
    for fixture in Fixture::both() {
        let log = Log::default();
        let mut txn = fixture.begin();
        register_outcomes(&mut txn, &log);
        txn.before_commit(move |txn| {
            txn.before_commit(|_| panic!("injected"));
            // The nested prepare reports the panic and this callback drops it.
            let _ = txn.prepare();
            Ok(())
        });
        let first = txn.prepare();
        assert!(
            matches!(
                first,
                Err(TransactionError::Engine(Error::CallbackPanicked {
                    latched: false,
                    ..
                }))
            ),
            "{first:?}"
        );
        assert_poisoned(txn, &log, &fixture);
    }
}

#[test]
fn a_panic_ignored_two_levels_up_poisons_the_commit_too() {
    for fixture in Fixture::both() {
        let log = Log::default();
        let mut txn = fixture.begin();
        register_outcomes(&mut txn, &log);
        txn.before_commit(move |txn| {
            txn.before_commit(move |txn| {
                txn.before_commit(|_| panic!("injected"));
                let _ = txn.prepare();
                Ok(())
            });
            let _ = txn.prepare();
            Ok(())
        });
        // No `prepare` of its own: the commit is the first to meet it.
        let result = txn.commit();
        assert!(
            matches!(
                result,
                Err(TransactionError::Engine(Error::CallbackPanicked {
                    latched: false,
                    ..
                }))
            ),
            "{result:?}"
        );
        assert_eq!(entries(&log), ["abort:panicked"]);
        assert_eq!(fixture.db().get(b"k").unwrap(), None);
    }
}

struct PanicsBeforeCommit;

impl TransactionHooks for PanicsBeforeCommit {
    fn before_commit(&self, _: &mut Transaction) -> TxResult<()> {
        panic!("injected hook panic");
    }
}

#[test]
fn a_hook_panic_a_callback_ignored_poisons_the_transaction() {
    let dir = TempDir::new().unwrap();
    let db = OptimisticTransactionDb::open(
        dir.path(),
        Options::default().transaction_hooks(Arc::new(PanicsBeforeCommit)),
    )
    .unwrap();
    let log = Log::default();
    let mut txn = db.begin(&TxnOptions::new());
    register_outcomes(&mut txn, &log);
    txn.before_commit(move |txn| {
        // The hook has not run, so the nested prepare runs it, and it panics.
        let _ = txn.prepare();
        Ok(())
    });
    let result = txn.commit();
    assert!(
        matches!(
            result,
            Err(TransactionError::Engine(Error::CallbackPanicked {
                latched: false,
                ..
            }))
        ),
        "{result:?}"
    );
    assert_eq!(entries(&log), ["abort:panicked"]);
    assert_eq!(db.db().get(b"k").unwrap(), None);
}

/// A one-key log for the append cases: the head at `journal-head`, entry `p`
/// at `journal/` and `p` in eight big-endian bytes.
struct Journal;

impl LogLayout for Journal {
    fn head_key(&self) -> &[u8] {
        b"journal-head"
    }

    fn entry_key(&self, position: u64, out: &mut Vec<u8>) {
        out.extend_from_slice(b"journal/");
        out.extend_from_slice(&position.to_be_bytes());
    }

    fn max_entry_key_len(&self) -> usize {
        b"journal/".len() + 8
    }
}

fn refused() -> TransactionError {
    Error::InvalidArgument("refused".into()).into()
}

fn is_refused(result: TxResult<()>) -> bool {
    matches!(
        result,
        Err(TransactionError::Engine(Error::InvalidArgument(_)))
    )
}

/// The entry `Journal` holds at `position`, if any.
fn journal_entry(db: &Db, position: u64) -> Option<Vec<u8>> {
    let mut key = Vec::new();
    Journal.entry_key(position, &mut key);
    db.get(&key).unwrap().map(|value| value.to_vec())
}

#[test]
fn a_failed_callback_that_rolled_back_the_callers_savepoint_puts_it_all_back() {
    for fixture in Fixture::both() {
        let journal: Arc<dyn LogLayout> = Arc::new(Journal);
        let mut txn = fixture.begin();
        // The caller's savepoint is set on an empty transaction, so the
        // callback's rollback takes the caller's write and append with it.
        txn.set_savepoint();
        txn.put(b"kept", b"1").unwrap();
        txn.append(&journal, b"entry", None).unwrap();
        let mut failures_left = 1;
        txn.before_commit(move |txn| {
            if failures_left > 0 {
                failures_left -= 1;
                txn.rollback_to_savepoint()?;
                txn.put(b"doomed", b"x")?;
                return Err(refused());
            }
            Ok(())
        });

        assert!(is_refused(txn.prepare()));
        assert_eq!(
            txn.get(b"kept").unwrap().as_deref(),
            Some(&b"1"[..]),
            "the failed callback's rollback took the caller's write"
        );
        assert_eq!(txn.get(b"doomed").unwrap(), None);

        // The retry succeeds without touching anything, so the commit carries
        // exactly what the caller buffered.
        txn.prepare().unwrap();
        txn.commit().unwrap();
        assert_eq!(
            fixture.db().get(b"kept").unwrap().as_deref(),
            Some(&b"1"[..])
        );
        assert_eq!(fixture.db().get(b"doomed").unwrap(), None);
        assert_eq!(
            journal_entry(fixture.db(), 1).as_deref(),
            Some(&b"entry"[..]),
            "the failed callback's rollback took the caller's append"
        );
    }
}

#[test]
fn a_failed_callback_that_rolled_back_the_callers_savepoint_leaves_it_set() {
    for fixture in Fixture::both() {
        let mut txn = fixture.begin();
        txn.put(b"before", b"1").unwrap();
        txn.set_savepoint();
        txn.put(b"after", b"2").unwrap();
        let mut failures_left = 1;
        txn.before_commit(move |txn| {
            if failures_left > 0 {
                failures_left -= 1;
                txn.rollback_to_savepoint()?;
                return Err(refused());
            }
            Ok(())
        });
        assert!(is_refused(txn.prepare()));
        assert_eq!(txn.get(b"after").unwrap().as_deref(), Some(&b"2"[..]));

        // The caller's savepoint is back where the caller set it: rolling back
        // to it takes `after` and keeps `before`, and it was the only one.
        txn.rollback_to_savepoint().unwrap();
        assert!(matches!(
            txn.rollback_to_savepoint(),
            Err(TransactionError::NoSavepoint)
        ));
        txn.prepare().unwrap();
        txn.commit().unwrap();
        assert_eq!(
            fixture.db().get(b"before").unwrap().as_deref(),
            Some(&b"1"[..])
        );
        assert_eq!(fixture.db().get(b"after").unwrap(), None);
    }
}

#[test]
fn a_failed_callback_that_released_the_callers_savepoint_leaves_it_set() {
    for fixture in Fixture::both() {
        let mut txn = fixture.begin();
        txn.put(b"before", b"1").unwrap();
        txn.set_savepoint();
        txn.put(b"after", b"2").unwrap();
        let mut failures_left = 1;
        txn.before_commit(move |txn| {
            if failures_left > 0 {
                failures_left -= 1;
                txn.release_savepoint()?;
                txn.put(b"doomed", b"x")?;
                return Err(refused());
            }
            Ok(())
        });
        assert!(is_refused(txn.prepare()));

        // The released savepoint is back, at the mark the caller set it at.
        txn.rollback_to_savepoint().unwrap();
        assert_eq!(txn.get(b"after").unwrap(), None);
        assert_eq!(txn.get(b"before").unwrap().as_deref(), Some(&b"1"[..]));
        txn.prepare().unwrap();
        txn.commit().unwrap();
        assert_eq!(
            fixture.db().get(b"before").unwrap().as_deref(),
            Some(&b"1"[..])
        );
        assert_eq!(fixture.db().get(b"after").unwrap(), None);
        assert_eq!(fixture.db().get(b"doomed").unwrap(), None);
    }
}

#[test]
fn a_rollback_inside_a_callback_that_succeeded_is_undone_when_the_outer_one_fails() {
    for fixture in Fixture::both() {
        let mut txn = fixture.begin();
        txn.set_savepoint();
        txn.put(b"kept", b"1").unwrap();
        let mut failures_left = 1;
        txn.before_commit(move |txn| {
            // Runs `inner`, queued behind this callback, inside it.
            txn.prepare()?;
            if failures_left > 0 {
                failures_left -= 1;
                return Err(refused());
            }
            Ok(())
        });
        let mut rollbacks_left = 1;
        txn.before_commit(move |txn| {
            if rollbacks_left > 0 {
                rollbacks_left -= 1;
                txn.rollback_to_savepoint()?;
            }
            txn.put(b"inner", b"2")
        });

        assert!(is_refused(txn.prepare()));
        assert_eq!(txn.get(b"kept").unwrap().as_deref(), Some(&b"1"[..]));
        assert_eq!(txn.get(b"inner").unwrap(), None);
        txn.release_savepoint().unwrap();
        assert!(matches!(
            txn.release_savepoint(),
            Err(TransactionError::NoSavepoint)
        ));

        txn.prepare().unwrap();
        txn.commit().unwrap();
        assert_eq!(
            fixture.db().get(b"kept").unwrap().as_deref(),
            Some(&b"1"[..])
        );
        assert_eq!(
            fixture.db().get(b"inner").unwrap().as_deref(),
            Some(&b"2"[..])
        );
    }
}
