//! Transaction callbacks (`before_commit`, `on_commit`, `on_abort`, `prepare`)
//! and the database-wide `TransactionHooks`, at every isolation level for both
//! transaction flavors.
//!
//! The rules under test are the protocol of `proofs/tla/TxnCallbacks.tla` and
//! `proofs/lean/Regolith/Callbacks.lean`: exactly one outcome per transaction,
//! exactly once per callback; before_commit callbacks, then the hook, then
//! validation; the transaction's outcome callbacks, then the hook's; an
//! attempt's callbacks belong to that attempt; a panic before the outcome
//! aborts the commit; `close` aborts what is still open.

// Native-only. wasm-pack builds every test target for wasm32, and these use
// threads, the filesystem or proptest, none of which exist there.
#![cfg(not(target_arch = "wasm32"))]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use proptest::prelude::*;
use regolith::{
    AbortReason, CommitInfo, CommitReceipt, Db, Error, EventListener, IsolationLevel,
    OptimisticTransactionDb, Options, RetryPolicy, TransactError, Transaction, TransactionDb,
    TransactionError, TransactionHooks, TxResult, TxnOptions,
};
use tempfile::TempDir;

mod common;

const LEVELS: [IsolationLevel; 5] = [
    IsolationLevel::ReadCommitted,
    IsolationLevel::SnapshotIsolation,
    IsolationLevel::RepeatableRead,
    IsolationLevel::Serializable,
    IsolationLevel::DefraLevel,
];

/// Both transaction flavors behind one handle, sharable into a `'static`
/// callback.
#[derive(Clone)]
enum Fixture {
    Optimistic {
        db: Arc<OptimisticTransactionDb>,
        _dir: Arc<TempDir>,
    },
    Pessimistic {
        db: Arc<TransactionDb>,
        _dir: Arc<TempDir>,
    },
}

impl Fixture {
    fn optimistic(options: Options) -> Self {
        let dir = TempDir::new().unwrap();
        let db = OptimisticTransactionDb::open(dir.path(), options).unwrap();
        Self::Optimistic {
            db: Arc::new(db),
            _dir: Arc::new(dir),
        }
    }

    fn pessimistic(options: Options) -> Self {
        let dir = TempDir::new().unwrap();
        let db = TransactionDb::open(dir.path(), options).unwrap();
        Self::Pessimistic {
            db: Arc::new(db),
            _dir: Arc::new(dir),
        }
    }

    fn db(&self) -> &Db {
        match self {
            Self::Optimistic { db, .. } => db.db(),
            Self::Pessimistic { db, .. } => db.db(),
        }
    }

    fn begin(&self, level: IsolationLevel) -> Transaction {
        let options = TxnOptions::new().isolation(level);
        match self {
            Self::Optimistic { db, .. } => db.begin(&options),
            Self::Pessimistic { db, .. } => db.begin(&options),
        }
    }

    /// `transact` under the database's default isolation level.
    fn transact<T, E>(
        &self,
        max_attempts: u32,
        f: impl FnMut(&mut Transaction, Option<&regolith::Conflict>) -> Result<T, E>,
    ) -> Result<(T, CommitReceipt), TransactError<E>> {
        let policy = RetryPolicy::new(max_attempts);
        match self {
            Self::Optimistic { db, .. } => db.transact(&policy, f),
            Self::Pessimistic { db, .. } => db.transact(&policy, f),
        }
    }
}

/// Run `test` against a fresh database of each flavor at each level.
fn each_config(options: impl Fn() -> Options, mut test: impl FnMut(&Fixture, IsolationLevel)) {
    for fixture in [
        Fixture::optimistic(options()),
        Fixture::pessimistic(options()),
    ] {
        for level in LEVELS {
            test(&fixture, level);
        }
    }
}

type Log = Arc<Mutex<Vec<String>>>;

fn log() -> Log {
    Log::default()
}

fn note(log: &Log, entry: impl Into<String>) {
    log.lock().unwrap().push(entry.into());
}

fn entries(log: &Log) -> Vec<String> {
    log.lock().unwrap().clone()
}

fn reason_name(reason: &AbortReason<'_>) -> &'static str {
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

type Before = Box<dyn Fn(&mut Transaction) -> TxResult<()> + Send + Sync>;

/// What `on_commit` saw: the receipt's sequence and the value read back.
type Seen = Mutex<Option<(u64, Option<Vec<u8>>)>>;

/// A hook that records every point it is called at.
struct Recorder {
    log: Log,
    before: Before,
}

impl Recorder {
    fn new(log: &Log) -> Arc<Self> {
        Self::with_before(log, |_| Ok(()))
    }

    fn with_before(
        log: &Log,
        before: impl Fn(&mut Transaction) -> TxResult<()> + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            log: Arc::clone(log),
            before: Box::new(before),
        })
    }
}

impl TransactionHooks for Recorder {
    fn before_commit(&self, txn: &mut Transaction) -> TxResult<()> {
        note(&self.log, "hook:before");
        (self.before)(txn)
    }

    fn on_commit(&self, info: &CommitInfo) {
        note(&self.log, format!("hook:commit:{:?}", info.isolation()));
    }

    fn on_abort(&self, reason: &AbortReason<'_>) {
        note(&self.log, format!("hook:abort:{}", reason_name(reason)));
    }
}

fn with_hook(hook: Arc<Recorder>) -> impl Fn() -> Options {
    move || Options::default().transaction_hooks(hook.clone())
}

/// Registers one of each callback, each noting itself in `log`.
fn register_all(txn: &mut Transaction, log: &Log) {
    let l = Arc::clone(log);
    txn.before_commit(move |_| {
        note(&l, "before");
        Ok(())
    });
    let l = Arc::clone(log);
    txn.on_commit(move |_| note(&l, "commit"));
    let l = Arc::clone(log);
    txn.on_abort(move |reason| note(&l, format!("abort:{}", reason_name(reason))));
}

// ---- exactly one outcome ---------------------------------------------------

#[test]
fn a_commit_runs_on_commit_once_in_order_and_drops_on_abort() {
    each_config(Options::default, |fixture, level| {
        let log = log();
        let mut txn = fixture.begin(level);
        for n in 1..=6 {
            let l = Arc::clone(&log);
            txn.on_commit(move |_| note(&l, format!("commit:{n}")));
            let l = Arc::clone(&log);
            txn.on_abort(move |_| note(&l, format!("abort:{n}")));
        }
        txn.put(b"k", b"v").unwrap();
        txn.commit().unwrap();
        let expected: Vec<String> = (1..=6).map(|n| format!("commit:{n}")).collect();
        assert_eq!(entries(&log), expected, "{level:?}");
    });
}

#[test]
fn on_commit_sees_the_commit_visible_and_receives_its_receipt() {
    each_config(Options::default, |fixture, level| {
        let seen: Arc<Seen> = Arc::default();
        let mut txn = fixture.begin(level);
        txn.put(b"visible", b"yes").unwrap();
        let (slot, db) = (Arc::clone(&seen), fixture.clone());
        txn.on_commit(move |receipt| {
            *slot.lock().unwrap() = Some((receipt.seq(), db.db().get(b"visible").unwrap()));
        });
        let receipt = txn.commit().unwrap();
        let (seq, value) = seen.lock().unwrap().take().expect("on_commit ran");
        assert_eq!(seq, receipt.seq(), "{level:?}");
        assert_eq!(value.as_deref(), Some(&b"yes"[..]), "{level:?}");
    });
}

#[test]
fn a_transaction_that_wrote_nothing_hands_on_commit_its_snapshot_receipt() {
    each_config(Options::default, |fixture, level| {
        fixture.db().put(b"seed", b"v").unwrap();
        let snapshot = fixture.db().snapshot().sequence();
        let seen = Arc::new(AtomicUsize::new(usize::MAX));
        let mut txn = fixture.begin(level);
        txn.get(b"seed").unwrap();
        let slot = Arc::clone(&seen);
        txn.on_commit(move |receipt| slot.store(receipt.seq() as usize, Ordering::SeqCst));
        let receipt = txn.commit().unwrap();
        assert_eq!(receipt.seq(), snapshot, "{level:?}");
        assert_eq!(seen.load(Ordering::SeqCst), snapshot as usize, "{level:?}");
    });
}

#[test]
fn rollback_runs_on_abort_with_rollback_and_drops_the_rest() {
    each_config(Options::default, |fixture, level| {
        let log = log();
        let mut txn = fixture.begin(level);
        register_all(&mut txn, &log);
        txn.put(b"k", b"v").unwrap();
        txn.rollback();
        assert_eq!(entries(&log), ["abort:rollback"], "{level:?}");
        assert_eq!(fixture.db().get(b"k").unwrap(), None);
    });
}

#[test]
fn dropping_a_transaction_runs_on_abort_with_dropped() {
    each_config(Options::default, |fixture, level| {
        let log = log();
        let mut txn = fixture.begin(level);
        register_all(&mut txn, &log);
        drop(txn);
        assert_eq!(entries(&log), ["abort:dropped"], "{level:?}");
    });
}

#[test]
fn a_conflict_runs_on_abort_with_the_conflict_and_drops_on_commit() {
    each_config(Options::default, |fixture, level| {
        fixture.db().put(b"k", b"v0").unwrap();
        let log = log();
        let seen_key: Arc<Mutex<Vec<u8>>> = Arc::default();
        let mut txn = fixture.begin(level);
        register_all(&mut txn, &log);
        let slot = Arc::clone(&seen_key);
        txn.on_abort(move |reason| {
            if let AbortReason::Conflict(conflict) = reason {
                *slot.lock().unwrap() = conflict.key().to_vec();
            }
        });
        txn.get_for_update(b"k").unwrap();
        fixture.db().put(b"k", b"v1").unwrap();
        txn.put(b"k", b"mine").unwrap();
        assert!(matches!(txn.commit(), Err(TransactionError::Conflict(_))));
        assert_eq!(entries(&log), ["before", "abort:conflict"], "{level:?}");
        assert_eq!(&*seen_key.lock().unwrap(), b"k");
    });
}

#[test]
fn a_failed_commit_runs_on_abort_with_the_error() {
    each_config(Options::default, |fixture, level| {
        let log = log();
        let mut txn = fixture.begin(level);
        register_all(&mut txn, &log);
        let l = Arc::clone(&log);
        txn.before_commit(move |_| {
            note(&l, "fails");
            Err(TransactionError::NoSavepoint)
        });
        txn.put(b"k", b"v").unwrap();
        assert!(matches!(txn.commit(), Err(TransactionError::NoSavepoint)));
        assert_eq!(
            entries(&log),
            ["before", "fails", "abort:error"],
            "{level:?}"
        );
        assert_eq!(fixture.db().get(b"k").unwrap(), None, "{level:?}");
    });
}

#[test]
fn at_immediate_on_commit_runs_only_after_the_commit_is_durable() {
    let stats = Arc::new(regolith::Statistics::new());
    let options = {
        let stats = stats.clone();
        move || {
            Options::default()
                .durability(regolith::DurabilityMode::Immediate)
                .statistics(Some(stats.clone()))
        }
    };
    each_config(options, |fixture, level| {
        let synced_at_callback = Arc::new(AtomicUsize::new(0));
        let before = stats.get_ticker(regolith::Ticker::WalSyncCount);
        let mut txn = fixture.begin(level);
        txn.put(b"k", b"v").unwrap();
        let (slot, stats) = (Arc::clone(&synced_at_callback), Arc::clone(&stats));
        txn.on_commit(move |_| {
            slot.store(
                stats.get_ticker(regolith::Ticker::WalSyncCount) as usize,
                Ordering::SeqCst,
            );
        });
        txn.commit().unwrap();
        assert!(
            synced_at_callback.load(Ordering::SeqCst) as u64 > before,
            "{level:?}: on_commit ran before the log was synced"
        );
    });
}

// ---- order -----------------------------------------------------------------

#[test]
fn own_callbacks_run_before_the_hook_before_and_after_the_outcome() {
    let log = log();
    let hook = Recorder::new(&log);
    each_config(with_hook(hook), |fixture, level| {
        log.lock().unwrap().clear();
        let mut txn = fixture.begin(level);
        for n in 1..=2 {
            let l = Arc::clone(&log);
            txn.before_commit(move |_| {
                note(&l, format!("before:{n}"));
                Ok(())
            });
            let l = Arc::clone(&log);
            txn.on_commit(move |_| note(&l, format!("commit:{n}")));
            let l = Arc::clone(&log);
            txn.on_abort(move |_| note(&l, format!("abort:{n}")));
        }
        txn.put(b"k", b"v").unwrap();
        txn.commit().unwrap();
        assert_eq!(
            entries(&log),
            [
                "before:1".to_string(),
                "before:2".into(),
                "hook:before".into(),
                "commit:1".into(),
                "commit:2".into(),
                format!("hook:commit:{level:?}"),
            ],
            "{level:?}"
        );

        log.lock().unwrap().clear();
        let mut txn = fixture.begin(level);
        for n in 1..=2 {
            let l = Arc::clone(&log);
            txn.on_commit(move |_| note(&l, format!("commit:{n}")));
            let l = Arc::clone(&log);
            txn.on_abort(move |_| note(&l, format!("abort:{n}")));
        }
        txn.rollback();
        assert_eq!(
            entries(&log),
            ["abort:1", "abort:2", "hook:abort:rollback"],
            "{level:?}"
        );
    });
}

#[test]
fn writes_made_by_a_before_commit_callback_are_validated() {
    each_config(Options::default, |fixture, level| {
        fixture.db().put(b"c", b"0").unwrap();
        let mut txn = fixture.begin(level);
        txn.before_commit(|txn| {
            let current = txn.get(b"c")?.unwrap();
            txn.put(b"c", &[current[0] + 1])
        });
        // A write around the transaction, after it began.
        fixture.db().put(b"c", b"5").unwrap();
        match txn.commit() {
            Err(TransactionError::Conflict(conflict)) => assert_eq!(conflict.key(), b"c"),
            other => panic!("{level:?}: the callback's write skipped validation: {other:?}"),
        }
        assert_eq!(fixture.db().get(b"c").unwrap().as_deref(), Some(&b"5"[..]));

        let mut txn = fixture.begin(level);
        txn.before_commit(|txn| {
            let current = txn.get(b"c")?.unwrap();
            txn.put(b"c", &[current[0] + 1])
        });
        txn.commit().unwrap();
        assert_eq!(fixture.db().get(b"c").unwrap().as_deref(), Some(&b"6"[..]));
    });
}

#[test]
fn a_callback_may_register_more_callbacks_that_run_in_the_same_pass() {
    each_config(Options::default, |fixture, level| {
        let log = log();
        let mut txn = fixture.begin(level);
        let l = Arc::clone(&log);
        txn.before_commit(move |txn| {
            note(&l, "first");
            let (inner, again, commit) = (Arc::clone(&l), Arc::clone(&l), Arc::clone(&l));
            txn.before_commit(move |txn| {
                note(&inner, "nested");
                let late = Arc::clone(&inner);
                txn.before_commit(move |_| {
                    note(&late, "nested:nested");
                    Ok(())
                });
                Ok(())
            });
            txn.on_commit(move |_| note(&commit, "registered:commit"));
            txn.on_abort(move |_| note(&again, "registered:abort"));
            Ok(())
        });
        let l = Arc::clone(&log);
        txn.before_commit(move |_| {
            note(&l, "second");
            Ok(())
        });
        txn.commit().unwrap();
        assert_eq!(
            entries(&log),
            [
                "first",
                "second",
                "nested",
                "nested:nested",
                "registered:commit"
            ],
            "{level:?}"
        );
    });
}

// ---- the database hook -----------------------------------------------------

#[test]
fn the_hooks_before_commit_writes_are_part_of_the_commit_and_validated() {
    let log = log();
    let hook = Recorder::with_before(&log, |txn| {
        let seen = txn.get(b"audit")?.map_or(0, |v| v[0]);
        txn.put(b"audit", &[seen + 1])
    });
    each_config(with_hook(hook), |fixture, level| {
        fixture.db().delete(b"audit").unwrap();
        let txn = fixture.begin(level);
        txn.put(b"k", b"v").unwrap();
        fixture.db().put(b"audit", &[9]).unwrap();
        assert!(
            matches!(txn.commit(), Err(TransactionError::Conflict(c)) if c.key() == b"audit"),
            "{level:?}: the hook's write skipped validation"
        );
        fixture.db().delete(b"audit").unwrap();
        let txn = fixture.begin(level);
        txn.put(b"k", b"v").unwrap();
        txn.commit().unwrap();
        assert_eq!(
            fixture.db().get(b"audit").unwrap().as_deref(),
            Some(&[1][..])
        );
    });
}

#[test]
fn a_hook_error_fails_the_commit_and_runs_the_abort_callbacks() {
    let log = log();
    let hook = Recorder::with_before(&log, |txn| {
        txn.put(b"hook-write", b"x")?;
        Err(TransactionError::NoSavepoint)
    });
    each_config(with_hook(hook), |fixture, level| {
        log.lock().unwrap().clear();
        let mut txn = fixture.begin(level);
        register_all(&mut txn, &log);
        txn.put(b"k", b"v").unwrap();
        assert!(matches!(txn.commit(), Err(TransactionError::NoSavepoint)));
        assert_eq!(
            entries(&log),
            ["before", "hook:before", "abort:error", "hook:abort:error"],
            "{level:?}"
        );
        assert_eq!(fixture.db().get(b"k").unwrap(), None);
        assert_eq!(fixture.db().get(b"hook-write").unwrap(), None);
    });
}

#[test]
fn the_hook_is_told_the_isolation_level_and_the_receipt() {
    struct Keep(Mutex<Vec<(IsolationLevel, u64)>>);
    impl TransactionHooks for Keep {
        fn on_commit(&self, info: &CommitInfo) {
            self.0
                .lock()
                .unwrap()
                .push((info.isolation(), info.receipt().seq()));
        }
    }
    let keep = Arc::new(Keep(Mutex::default()));
    let options = {
        let keep = keep.clone();
        move || Options::default().transaction_hooks(keep.clone())
    };
    each_config(options, |fixture, level| {
        let txn = fixture.begin(level);
        txn.put(b"k", b"v").unwrap();
        let receipt = txn.commit().unwrap();
        assert_eq!(
            keep.0.lock().unwrap().last().copied(),
            Some((level, receipt.seq()))
        );
    });
}

// ---- prepare ---------------------------------------------------------------

#[test]
fn prepare_runs_the_callbacks_once_and_commit_does_not_run_them_again() {
    let log = log();
    let hook = Recorder::new(&log);
    each_config(with_hook(hook), |fixture, level| {
        log.lock().unwrap().clear();
        let mut txn = fixture.begin(level);
        let l = Arc::clone(&log);
        txn.before_commit(move |_| {
            note(&l, "before");
            Ok(())
        });
        txn.prepare().unwrap();
        txn.prepare().unwrap();
        assert_eq!(entries(&log), ["before", "hook:before"], "{level:?}");
        txn.commit().unwrap();
        assert_eq!(
            entries(&log),
            [
                "before".to_string(),
                "hook:before".into(),
                format!("hook:commit:{level:?}")
            ],
            "{level:?}"
        );
    });
}

#[test]
fn a_failing_callback_is_rolled_back_and_the_later_ones_stay_queued() {
    each_config(Options::default, |fixture, level| {
        let log = log();
        let mut txn = fixture.begin(level);
        txn.put(b"kept", b"1").unwrap();
        txn.set_savepoint();
        let l = Arc::clone(&log);
        let mut failed = false;
        txn.before_commit(move |txn| {
            if std::mem::replace(&mut failed, true) {
                return Ok(());
            }
            txn.put(b"doomed", b"x")?;
            txn.set_savepoint();
            let (c, a) = (Arc::clone(&l), Arc::clone(&l));
            txn.on_commit(move |_| note(&c, "doomed:commit"));
            txn.on_abort(move |_| note(&a, "doomed:abort"));
            Err(TransactionError::NoSavepoint)
        });
        let l = Arc::clone(&log);
        txn.before_commit(move |txn| {
            note(&l, "later");
            txn.put(b"later", b"2")
        });
        assert!(matches!(txn.prepare(), Err(TransactionError::NoSavepoint)));
        assert_eq!(txn.get(b"doomed").unwrap(), None, "{level:?}");
        assert_eq!(txn.get(b"kept").unwrap().as_deref(), Some(&b"1"[..]));
        assert!(entries(&log).is_empty(), "the later callback ran early");

        // The savepoint the failed callback set is gone, the caller's is not.
        txn.rollback_to_savepoint().unwrap();
        assert!(matches!(
            txn.rollback_to_savepoint(),
            Err(TransactionError::NoSavepoint)
        ));
        assert_eq!(txn.get(b"kept").unwrap().as_deref(), Some(&b"1"[..]));

        txn.prepare().unwrap();
        assert_eq!(entries(&log), ["later"], "{level:?}");
        txn.commit().unwrap();
        // What the failed callback registered was taken back with it.
        assert_eq!(entries(&log), ["later"], "{level:?}");
        assert_eq!(
            fixture.db().get(b"later").unwrap().as_deref(),
            Some(&b"2"[..])
        );
        assert_eq!(fixture.db().get(b"doomed").unwrap(), None);
    });
}

#[test]
fn an_error_from_the_hook_leaves_the_hook_to_run_again() {
    let log = log();
    let failures = Arc::new(AtomicUsize::new(1));
    let hook = {
        let failures = Arc::clone(&failures);
        Recorder::with_before(&log, move |txn| {
            txn.put(b"hook", b"w")?;
            if failures.fetch_sub(1, Ordering::SeqCst) > 0 {
                return Err(TransactionError::NoSavepoint);
            }
            Ok(())
        })
    };
    each_config(with_hook(hook), |fixture, level| {
        log.lock().unwrap().clear();
        failures.store(1, Ordering::SeqCst);
        let mut txn = fixture.begin(level);
        assert!(txn.prepare().is_err());
        txn.prepare().unwrap();
        txn.commit().unwrap();
        assert_eq!(
            entries(&log),
            [
                "hook:before".to_string(),
                "hook:before".into(),
                format!("hook:commit:{level:?}")
            ],
            "{level:?}"
        );
        assert_eq!(
            fixture.db().get(b"hook").unwrap().as_deref(),
            Some(&b"w"[..])
        );
    });
}

/// Entries at `journal/<20 decimal digits>`, head at `journal-head`.
struct Journal;

impl regolith::LogLayout for Journal {
    fn head_key(&self) -> &[u8] {
        b"journal-head"
    }

    fn entry_key(&self, position: u64, out: &mut Vec<u8>) {
        out.extend_from_slice(format!("journal/{position:020}").as_bytes());
    }

    fn max_entry_key_len(&self) -> usize {
        28
    }
}

#[test]
fn a_failing_callbacks_appends_are_taken_back_with_its_writes() {
    let dir = TempDir::new().unwrap();
    let db = OptimisticTransactionDb::open(dir.path(), Options::default()).unwrap();
    let log: Arc<dyn regolith::LogLayout> = Arc::new(Journal);
    let mut txn = db.begin(&TxnOptions::new());
    let doomed = Arc::clone(&log);
    let mut failed = false;
    txn.before_commit(move |txn| {
        if std::mem::replace(&mut failed, true) {
            return Ok(());
        }
        txn.append(&doomed, b"doomed", None)?;
        Err(TransactionError::NoSavepoint)
    });
    let kept = Arc::clone(&log);
    txn.before_commit(move |txn| txn.append(&kept, b"kept", None));
    txn.append(&log, b"first", None).unwrap();
    assert!(txn.prepare().is_err());
    txn.commit().unwrap();
    let entry = |n: u64| db.db().get(format!("journal/{n:020}").as_bytes()).unwrap();
    assert_eq!(entry(1).as_deref(), Some(&b"first"[..]));
    assert_eq!(entry(2).as_deref(), Some(&b"kept"[..]));
    assert_eq!(
        entry(3),
        None,
        "the failed callback's append was taken back"
    );
}

// ---- panics ----------------------------------------------------------------

fn panicked_in(callback: &str, result: &TxResult<CommitReceipt>) -> bool {
    matches!(
        result,
        Err(TransactionError::Engine(Error::CallbackPanicked { callback: named, latched: false })) if *named == callback
    )
}

#[test]
fn a_panic_in_before_commit_fails_the_commit_and_aborts_the_attempt() {
    each_config(Options::default, |fixture, level| {
        let log = log();
        let mut txn = fixture.begin(level);
        register_all(&mut txn, &log);
        txn.before_commit(|txn| {
            txn.put(b"doomed", b"x").unwrap();
            panic!("injected before_commit panic");
        });
        txn.put(b"k", b"v").unwrap();
        let result = txn.commit();
        assert!(
            panicked_in("before_commit", &result),
            "{level:?}: {result:?}"
        );
        assert_eq!(entries(&log), ["before", "abort:panicked"], "{level:?}");
        assert_eq!(fixture.db().get(b"k").unwrap(), None);
        assert_eq!(fixture.db().get(b"doomed").unwrap(), None);
        // A panic before the commit step does not latch the database.
        fixture.db().put(b"after", b"v").unwrap();
    });
}

#[test]
fn a_panic_in_the_hooks_before_commit_fails_the_commit_and_aborts_the_attempt() {
    struct Panics(Log);
    impl TransactionHooks for Panics {
        fn before_commit(&self, _: &mut Transaction) -> TxResult<()> {
            panic!("injected hook panic");
        }
        fn on_abort(&self, reason: &AbortReason<'_>) {
            note(&self.0, format!("hook:abort:{}", reason_name(reason)));
        }
    }
    let log = log();
    let hooks = Arc::new(Panics(Arc::clone(&log)));
    each_config(
        {
            let hooks = hooks.clone();
            move || Options::default().transaction_hooks(hooks.clone())
        },
        |fixture, level| {
            log.lock().unwrap().clear();
            let mut txn = fixture.begin(level);
            register_all(&mut txn, &log);
            let result = txn.commit();
            assert!(panicked_in("TransactionHooks", &result), "{result:?}");
            assert_eq!(
                entries(&log),
                ["before", "abort:panicked", "hook:abort:panicked"],
                "{level:?}"
            );
        },
    );
}

#[test]
fn after_prepare_panics_the_transaction_can_only_fail() {
    each_config(Options::default, |fixture, level| {
        let mut txn = fixture.begin(level);
        txn.before_commit(|_| panic!("injected"));
        assert!(matches!(
            txn.prepare(),
            Err(TransactionError::Engine(Error::CallbackPanicked { .. }))
        ));
        txn.put(b"k", b"v").unwrap();
        let result = txn.commit();
        assert!(
            panicked_in("before_commit", &result),
            "{level:?}: {result:?}"
        );
        assert_eq!(fixture.db().get(b"k").unwrap(), None);
        let message = result.unwrap_err().to_string();
        assert!(
            !message.contains("read-only"),
            "a panic in before_commit does not latch the database: {message}"
        );
    });
}

/// Counts the panics reported to it.
#[derive(Default)]
struct PanicCounter(Mutex<Vec<&'static str>>);

impl EventListener for PanicCounter {
    fn on_callback_panic(&self, callback: &'static str) {
        self.0.lock().unwrap().push(callback);
    }
}

#[test]
fn a_panic_after_the_outcome_is_reported_and_the_outcome_stands() {
    let listener = Arc::new(PanicCounter::default());
    struct Panics;
    impl TransactionHooks for Panics {
        fn on_commit(&self, _: &CommitInfo) {
            panic!("injected hook on_commit panic");
        }
        fn on_abort(&self, _: &AbortReason<'_>) {
            panic!("injected hook on_abort panic");
        }
    }
    let options = {
        let listener = listener.clone();
        move || {
            Options::default()
                .listeners(vec![listener.clone()])
                .transaction_hooks(Arc::new(Panics))
        }
    };
    each_config(options, |fixture, level| {
        listener.0.lock().unwrap().clear();
        let log = log();
        let mut txn = fixture.begin(level);
        txn.on_commit(|_| panic!("injected on_commit panic"));
        let l = Arc::clone(&log);
        txn.on_commit(move |_| note(&l, "second"));
        txn.put(b"k", b"v").unwrap();
        txn.commit().expect("the commit stands");
        assert_eq!(fixture.db().get(b"k").unwrap().as_deref(), Some(&b"v"[..]));
        assert_eq!(entries(&log), ["second"], "the other callbacks still run");
        assert_eq!(
            *listener.0.lock().unwrap(),
            ["on_commit", "TransactionHooks::on_commit"],
            "{level:?}"
        );

        listener.0.lock().unwrap().clear();
        let mut txn = fixture.begin(level);
        txn.on_abort(|_| panic!("injected on_abort panic"));
        let l = Arc::clone(&log);
        txn.on_abort(move |_| note(&l, "abort second"));
        txn.rollback();
        assert!(entries(&log).contains(&"abort second".to_string()));
        assert_eq!(
            *listener.0.lock().unwrap(),
            ["on_abort", "TransactionHooks::on_abort"],
            "{level:?}"
        );
    });
}

#[test]
fn a_callback_panic_while_unwinding_does_not_abort_the_process() {
    let fixture = Fixture::optimistic(Options::default());
    let ran = Arc::new(AtomicUsize::new(0));
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut txn = fixture.begin(IsolationLevel::default());
        let ran = Arc::clone(&ran);
        txn.on_abort(move |_| {
            ran.fetch_add(1, Ordering::SeqCst);
            panic!("injected on_abort panic during unwinding");
        });
        panic!("the caller panics holding a transaction");
    }));
    assert!(outcome.is_err());
    assert_eq!(ran.load(Ordering::SeqCst), 1);
}

// ---- transact --------------------------------------------------------------

#[test]
fn each_attempt_has_its_own_callbacks_and_a_conflict_runs_only_its_abort() {
    let log = log();
    let hook = Recorder::new(&log);
    for fixture in [
        Fixture::optimistic(with_hook(hook.clone())()),
        Fixture::pessimistic(with_hook(hook)()),
    ] {
        log.lock().unwrap().clear();
        fixture.db().put(b"k", b"0").unwrap();
        let mut attempt = 0;
        let (value, _) = fixture
            .transact(5, |txn, previous| -> Result<usize, TransactionError> {
                attempt += 1;
                assert_eq!(previous.is_some(), attempt > 1);
                let n = attempt;
                let l = Arc::clone(&log);
                txn.on_commit(move |_| note(&l, format!("commit:{n}")));
                let l = Arc::clone(&log);
                txn.on_abort(move |reason| note(&l, format!("abort:{n}:{}", reason_name(reason))));
                txn.get_for_update(b"k")?;
                if attempt < 3 {
                    fixture.db().put(b"k", &[attempt as u8]).unwrap();
                }
                txn.put(b"k", b"mine")?;
                Ok(attempt)
            })
            .expect("the third attempt commits");
        assert_eq!(value, 3);
        assert_eq!(
            entries(&log),
            [
                "hook:before",
                "abort:1:conflict",
                "hook:abort:conflict",
                "hook:before",
                "abort:2:conflict",
                "hook:abort:conflict",
                "hook:before",
                "commit:3",
                "hook:commit:SnapshotIsolation",
            ]
        );
    }
}

#[test]
fn every_attempt_of_an_exhausted_transact_aborts_and_none_commits() {
    let log = log();
    each_config(Options::default, |fixture, _| {
        log.lock().unwrap().clear();
        fixture.db().put(b"k", b"0").unwrap();
        let mut attempt = 0;
        let result = fixture.transact(3, |txn, _| -> Result<(), TransactionError> {
            attempt += 1;
            let n = attempt;
            let l = Arc::clone(&log);
            txn.on_commit(move |_| note(&l, format!("commit:{n}")));
            let l = Arc::clone(&log);
            txn.on_abort(move |reason| note(&l, format!("abort:{n}:{}", reason_name(reason))));
            txn.get_for_update(b"k")?;
            fixture.db().put(b"k", b"x").unwrap();
            txn.put(b"k", b"mine")?;
            Ok(())
        });
        assert!(matches!(result, Err(TransactError::Exhausted(_))));
        assert_eq!(
            entries(&log),
            ["abort:1:conflict", "abort:2:conflict", "abort:3:conflict"]
        );
    });
}

#[test]
fn a_closure_error_rolls_the_attempt_back() {
    each_config(Options::default, |fixture, _| {
        let log = log();
        let result = fixture.transact(3, |txn, _| -> Result<(), &'static str> {
            let l = Arc::clone(&log);
            txn.on_abort(move |reason| note(&l, format!("abort:{}", reason_name(reason))));
            let l = Arc::clone(&log);
            txn.on_commit(move |_| note(&l, "commit"));
            Err("no")
        });
        assert!(matches!(result, Err(TransactError::Closure("no"))));
        assert_eq!(entries(&log), ["abort:rollback"]);
    });
}

// ---- close -----------------------------------------------------------------

#[test]
fn close_aborts_an_open_transaction_once_and_its_commit_is_refused() {
    let log = log();
    let hook = Recorder::new(&log);
    for fixture in [
        Fixture::optimistic(with_hook(hook.clone())()),
        Fixture::pessimistic(with_hook(hook)()),
    ] {
        log.lock().unwrap().clear();
        let mut txn = fixture.begin(IsolationLevel::default());
        let closer = std::thread::current().id();
        let (l, ran_on_closer) = (Arc::clone(&log), Arc::new(AtomicUsize::new(0)));
        let flag = Arc::clone(&ran_on_closer);
        txn.on_abort(move |reason| {
            flag.fetch_add(
                usize::from(std::thread::current().id() == closer),
                Ordering::SeqCst,
            );
            note(&l, format!("abort:{}", reason_name(reason)));
        });
        let l = Arc::clone(&log);
        txn.on_commit(move |_| note(&l, "commit"));
        txn.put(b"k", b"v").unwrap();

        fixture.db().close().unwrap();
        assert_eq!(
            entries(&log),
            ["abort:closed", "hook:abort:closed"],
            "close runs the callbacks, on its own thread"
        );
        assert_eq!(ran_on_closer.load(Ordering::SeqCst), 1);

        let result = txn.commit();
        assert!(
            matches!(result, Err(TransactionError::Engine(Error::Closed))),
            "{result:?}"
        );
        assert_eq!(entries(&log).len(), 2, "nothing runs a second time");
    }
}

#[test]
fn a_callback_registered_after_close_aborted_the_transaction_runs_once_at_its_end() {
    each_config(Options::default, |fixture, level| {
        // `each_config` reuses one database per flavor, so close a copy.
        let own = match fixture {
            Fixture::Optimistic { .. } => Fixture::optimistic(Options::default()),
            Fixture::Pessimistic { .. } => Fixture::pessimistic(Options::default()),
        };
        let log = log();
        let mut txn = own.begin(level);
        let l = Arc::clone(&log);
        txn.on_abort(move |reason| note(&l, format!("first:{}", reason_name(reason))));
        own.db().close().unwrap();
        let l = Arc::clone(&log);
        txn.on_abort(move |reason| note(&l, format!("late:{}", reason_name(reason))));
        drop(txn);
        assert_eq!(entries(&log), ["first:closed", "late:closed"], "{level:?}");
    });
}

#[test]
fn a_transaction_begun_after_close_registers_nothing_and_fails_with_closed() {
    let log = log();
    let hook = Recorder::new(&log);
    for fixture in [
        Fixture::optimistic(with_hook(hook.clone())()),
        Fixture::pessimistic(with_hook(hook)()),
    ] {
        log.lock().unwrap().clear();
        fixture.db().close().unwrap();
        let mut txn = fixture.begin(IsolationLevel::default());
        register_all(&mut txn, &log);
        txn.put(b"k", b"v").unwrap();
        let result = txn.commit();
        assert!(
            matches!(result, Err(TransactionError::Engine(Error::Closed))),
            "{result:?}"
        );
        assert_eq!(
            entries(&log),
            ["before", "hook:before", "abort:closed", "hook:abort:closed"]
        );
    }
}

#[test]
fn a_write_free_commit_after_close_is_refused_like_any_other() {
    each_config(Options::default, |_, level| {
        let fixture = Fixture::optimistic(Options::default());
        let log = log();
        fixture.db().close().unwrap();
        let mut txn = fixture.begin(level);
        register_all(&mut txn, &log);
        let result = txn.commit();
        assert!(
            matches!(result, Err(TransactionError::Engine(Error::Closed))),
            "{level:?}: {result:?}"
        );
        assert_eq!(entries(&log), ["before", "abort:closed"], "{level:?}");
    });
}

#[test]
fn a_dropped_transaction_of_a_closed_database_reports_closed() {
    each_config(Options::default, |_, level| {
        let fixture = Fixture::optimistic(Options::default());
        let log = log();
        let mut txn = fixture.begin(level);
        register_all(&mut txn, &log);
        fixture.db().close().unwrap();
        drop(txn);
        assert_eq!(entries(&log), ["abort:closed"], "{level:?}");
    });
}

/// `close` and `commit` race on many transactions: whichever wins a
/// transaction, every callback of it runs exactly once, and only those of the
/// outcome it reached.
#[test]
fn close_and_commit_racing_run_every_callback_exactly_once() {
    for round in 0..40 {
        let fixture = Fixture::optimistic(Options::default());
        let mut counters = Vec::new();
        let mut txns = Vec::new();
        for n in 0..16u8 {
            let (commits, aborts) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
            let mut txn = fixture.begin(IsolationLevel::default());
            let c = Arc::clone(&commits);
            txn.on_commit(move |_| {
                c.fetch_add(1, Ordering::SeqCst);
            });
            let a = Arc::clone(&aborts);
            txn.on_abort(move |_| {
                a.fetch_add(1, Ordering::SeqCst);
            });
            txn.put(&[n], b"v").unwrap();
            counters.push((commits, aborts));
            txns.push(txn);
        }
        let results = std::thread::scope(|scope| {
            let committer = scope.spawn(|| {
                txns.into_iter()
                    .map(|txn| match txn.commit() {
                        Ok(_) => true,
                        // Close won this transaction.
                        Err(TransactionError::Engine(Error::Closed)) => false,
                        Err(other) => {
                            panic!("round {round}: a commit failed, and not on close: {other}")
                        }
                    })
                    .collect::<Vec<_>>()
            });
            if round % 2 == 0 {
                std::thread::yield_now();
            }
            fixture.db().close().unwrap();
            committer.join().unwrap()
        });
        for (committed, (commits, aborts)) in results.iter().zip(&counters) {
            let (commits, aborts) = (
                commits.load(Ordering::SeqCst),
                aborts.load(Ordering::SeqCst),
            );
            assert_eq!(
                (commits, aborts),
                if *committed { (1, 0) } else { (0, 1) },
                "round {round}"
            );
        }
    }
}

// ---- the exactly-once law --------------------------------------------------

#[derive(Debug, Clone)]
enum Op {
    Begin(usize, u8, u8),
    Poke(usize),
    Commit(usize),
    Rollback(usize),
    Drop(usize),
}

fn ops() -> impl Strategy<Value = Vec<Op>> {
    prop::collection::vec(
        prop_oneof![
            4 => (0usize..3, 0u8..4, 0u8..4).prop_map(|(s, c, a)| Op::Begin(s, c, a)),
            2 => (0usize..3).prop_map(Op::Poke),
            3 => (0usize..3).prop_map(Op::Commit),
            1 => (0usize..3).prop_map(Op::Rollback),
            1 => (0usize..3).prop_map(Op::Drop),
        ],
        0..40,
    )
}

/// What one transaction's callbacks counted, and how it ended.
struct Record {
    commits: Vec<Arc<AtomicUsize>>,
    aborts: Vec<Arc<AtomicUsize>>,
    committed: Option<bool>,
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// Over any mix of commits, rollbacks, drops and forced conflicts, in
    /// either flavor and at any level, every transaction runs the callbacks of
    /// the outcome it reached once each, none of the other outcome's, and the
    /// database hook runs once per transaction for the same outcome.
    #[test]
    fn every_callback_of_every_transaction_runs_exactly_once(
        ops in ops(),
        pessimistic in any::<bool>(),
        level in 0usize..5,
        close_at_end in any::<bool>(),
    ) {
        let hook_commits = Arc::new(AtomicUsize::new(0));
        let hook_aborts = Arc::new(AtomicUsize::new(0));
        struct Counts(Arc<AtomicUsize>, Arc<AtomicUsize>);
        impl TransactionHooks for Counts {
            fn on_commit(&self, _: &CommitInfo) { self.0.fetch_add(1, Ordering::SeqCst); }
            fn on_abort(&self, _: &AbortReason<'_>) { self.1.fetch_add(1, Ordering::SeqCst); }
        }
        let options = Options::default()
            .transaction_hooks(Arc::new(Counts(hook_commits.clone(), hook_aborts.clone())));
        let fixture = if pessimistic {
            Fixture::pessimistic(options)
        } else {
            Fixture::optimistic(options)
        };
        let level = LEVELS[level];
        let key = |slot: usize| format!("slot-{slot}").into_bytes();

        let mut live: [Option<(Transaction, Record)>; 3] = [None, None, None];
        let mut done: Vec<Record> = Vec::new();
        for op in ops {
            match op {
                Op::Begin(slot, commits, aborts) => {
                    // A transaction still in the slot is dropped first.
                    finish_dropped(&mut live[slot], &mut done);
                    let mut txn = fixture.begin(level);
                    let mut record = Record { commits: vec![], aborts: vec![], committed: None };
                    for _ in 0..commits {
                        let counter = Arc::new(AtomicUsize::new(0));
                        let c = Arc::clone(&counter);
                        txn.on_commit(move |_| { c.fetch_add(1, Ordering::SeqCst); });
                        record.commits.push(counter);
                    }
                    for _ in 0..aborts {
                        let counter = Arc::new(AtomicUsize::new(0));
                        let c = Arc::clone(&counter);
                        txn.on_abort(move |_| { c.fetch_add(1, Ordering::SeqCst); });
                        record.aborts.push(counter);
                    }
                    txn.get_for_update(&key(slot)).unwrap();
                    txn.put(&key(slot), b"mine").unwrap();
                    live[slot] = Some((txn, record));
                }
                Op::Poke(slot) => fixture.db().put(&key(slot), b"poke").unwrap(),
                Op::Commit(slot) => {
                    if let Some((txn, record)) = live[slot].take() {
                        let committed = common::committed(&txn.commit());
                        let mut record = record;
                        record.committed = Some(committed);
                        done.push(record);
                    }
                }
                Op::Rollback(slot) => {
                    if let Some((txn, mut record)) = live[slot].take() {
                        txn.rollback();
                        record.committed = Some(false);
                        done.push(record);
                    }
                }
                Op::Drop(slot) => finish_dropped(&mut live[slot], &mut done),
            }
        }
        if close_at_end {
            fixture.db().close().unwrap();
        }
        for slot in &mut live {
            finish_dropped(slot, &mut done);
        }

        let (mut commits, mut aborts) = (0, 0);
        for record in &done {
            let committed = record.committed.expect("every transaction ended");
            commits += usize::from(committed);
            aborts += usize::from(!committed);
            for counter in &record.commits {
                prop_assert_eq!(counter.load(Ordering::SeqCst), usize::from(committed));
            }
            for counter in &record.aborts {
                prop_assert_eq!(counter.load(Ordering::SeqCst), usize::from(!committed));
            }
        }
        prop_assert_eq!(hook_commits.load(Ordering::SeqCst), commits);
        prop_assert_eq!(hook_aborts.load(Ordering::SeqCst), aborts);
    }
}

/// Drop what is left in `slot`, ending it without a commit.
fn finish_dropped(slot: &mut Option<(Transaction, Record)>, done: &mut Vec<Record>) {
    if let Some((txn, mut record)) = slot.take() {
        drop(txn);
        record.committed = Some(false);
        done.push(record);
    }
}
