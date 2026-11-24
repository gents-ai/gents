//! What a caller reads back from the handles it holds: the statistics a
//! database was opened with, and the options it set.

// Native-only. wasm-pack builds every test target for wasm32, and the browser
// suite lives in tests/wasm_opfs*.rs.
#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;

use std::sync::atomic::{AtomicUsize, Ordering};

use regolith::{Db, IsolationLevel, OptimisticTransactionDb, Options, Statistics, Ticker};
use regolith::{EventListener, FlushJobInfo, TransactionDb, TxnOptions};

struct Quiet;

impl EventListener for Quiet {}

/// Counts the flushes it is told of.
#[derive(Default)]
struct Flushes(AtomicUsize);

impl EventListener for Flushes {
    fn on_flush_completed(&self, _info: &FlushJobInfo) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

/// A caller handed options that already carry a listener adds its own without
/// reading the list back, and the database tells both.
#[test]
fn a_listener_added_to_options_is_told_beside_the_ones_already_there() {
    let (theirs, mine) = (Arc::new(Flushes::default()), Arc::new(Flushes::default()));
    let given = Options::memory().listeners(vec![theirs.clone()]);
    let db = Db::open("surface-add-listener", given.add_listener(mine.clone())).unwrap();
    db.put(b"k", b"v").unwrap();
    db.flush().unwrap();
    assert_eq!(theirs.0.load(Ordering::Relaxed), 1);
    assert_eq!(mine.0.load(Ordering::Relaxed), 1);
}

#[test]
fn options_hand_back_the_key_bound_and_the_inline_compaction_switch() {
    let defaults = Options::default();
    assert_eq!(defaults.get_max_key_size(), regolith::DEFAULT_MAX_KEY_SIZE);
    assert!(!defaults.get_inline_compaction());

    let set = Options::default().max_key_size(512).inline_compaction(true);
    assert_eq!(set.get_max_key_size(), 512);
    assert!(set.get_inline_compaction());

    // A preset's choices are read the same way, so a caller that copies some
    // of a preset onto its own options can copy these two as well.
    let preset = Options::memory();
    assert_eq!(preset.get_max_key_size(), 16 * 1024);
    assert!(preset.get_inline_compaction());
}

/// The inline-compaction getter reads the switch as the database takes it.
/// Left unset, that follows whether the database has a compaction worker.
#[test]
fn the_inline_compaction_getter_reads_what_the_database_takes() {
    // A default database has a worker: off.
    assert_eq!(Options::default().get_max_background_compactions(), 1);
    assert!(!Options::default().get_inline_compaction());
    // No worker and nothing said: on.
    let no_worker = || Options::default().max_background_compactions(0);
    assert!(no_worker().get_inline_compaction());
    // Said explicitly, it is what was said.
    assert!(!no_worker().inline_compaction(false).get_inline_compaction());
    assert!(
        Options::default()
            .inline_compaction(true)
            .get_inline_compaction()
    );
}

#[test]
fn options_hand_back_the_listeners_in_the_order_they_were_set() {
    assert!(Options::default().get_listeners().is_empty());

    let first: Arc<dyn EventListener> = Arc::new(Quiet);
    let options = Options::default().listeners(vec![Arc::clone(&first)]);
    assert_eq!(options.get_listeners().len(), 1);
    assert!(Arc::ptr_eq(&options.get_listeners()[0], &first));

    // The builder replaces the list, so adding one listener to the ones
    // already set is: read them, add, set.
    let second: Arc<dyn EventListener> = Arc::new(Quiet);
    let mut listeners = options.get_listeners().to_vec();
    listeners.push(Arc::clone(&second));
    let options = options.listeners(listeners);
    let set = options.get_listeners();
    assert_eq!(set.len(), 2);
    assert!(Arc::ptr_eq(&set[0], &first));
    assert!(Arc::ptr_eq(&set[1], &second));
}

#[test]
fn a_database_hands_back_the_statistics_it_was_opened_with() {
    let stats = Arc::new(Statistics::new());
    let options = Options::memory().statistics(Some(Arc::clone(&stats)));
    let db = Db::open("surface-statistics", options).unwrap();

    let handed_back = db.statistics().expect("the database was opened with one");
    assert!(
        Arc::ptr_eq(handed_back, &stats),
        "the caller's own object, not a copy"
    );
    db.put(b"k", b"v").unwrap();
    assert_eq!(handed_back.get_ticker(Ticker::KeysWritten), 1);
    assert_eq!(stats.get_ticker(Ticker::KeysWritten), 1);
}

#[test]
fn a_database_opened_without_statistics_hands_back_none() {
    let db = Db::open("surface-no-statistics", Options::memory()).unwrap();
    assert!(db.statistics().is_none());
    db.put(b"k", b"v").unwrap();
    assert!(db.statistics().is_none(), "none is made on demand");
}

/// A caller that holds only a transaction database, and never kept the
/// `Arc` it opened it with, still reads the commit tickers.
#[test]
fn both_transaction_databases_reach_their_statistics() {
    let options = || Options::memory().statistics(Some(Arc::new(Statistics::new())));

    let optimistic = OptimisticTransactionDb::open("surface-optimistic", options()).unwrap();
    let txn = optimistic.begin(&TxnOptions::new().isolation(IsolationLevel::RepeatableRead));
    txn.put(b"k", b"v").unwrap();
    txn.commit().unwrap();
    let stats = optimistic.db().statistics().unwrap();
    assert_eq!(stats.get_ticker(Ticker::CommitCount), 1);

    let pessimistic = TransactionDb::open("surface-pessimistic", options()).unwrap();
    let txn = pessimistic.begin(&TxnOptions::new());
    txn.put(b"k", b"v").unwrap();
    txn.commit().unwrap();
    let stats = pessimistic.db().statistics().unwrap();
    assert_eq!(stats.get_ticker(Ticker::CommitCount), 1);
}

/// Everything here comes from `regolith::prelude` and nothing else: this
/// module sees none of the imports above.
mod from_the_prelude_alone {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use regolith::prelude::*;

    /// Counts the conflicts it hears about.
    #[derive(Default)]
    struct Conflicts(AtomicUsize);

    impl EventListener for Conflicts {
        fn on_conflict(&self, _conflict: &Conflict) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Removes every value that starts with `x`.
    struct DropX;

    impl CompactionFilter for DropX {
        fn filter(&self, _level: usize, _key: &[u8], value: &[u8]) -> CompactionDecision {
            if value.starts_with(b"x") {
                CompactionDecision::Remove
            } else {
                CompactionDecision::Keep
            }
        }

        fn name(&self) -> &'static str {
            "drop-x"
        }
    }

    /// Every trait a caller implements is named by the prelude.
    #[test]
    fn the_prelude_names_every_trait_a_caller_implements() {
        fn named<T: ?Sized>() {}
        named::<dyn KeyClassifier>();
        named::<dyn MergeOperator>();
        named::<dyn LogLayout>();
        named::<dyn TransactionHooks>();
        named::<dyn EventListener>();
        named::<dyn KeyProvider>();
        named::<dyn CompactionFilter>();
        named::<dyn Env>();
    }

    /// A listener, a compaction filter and an environment, written and
    /// installed with the prelude alone, are the ones the database runs.
    #[test]
    fn a_listener_a_filter_and_an_environment_work_from_the_prelude_alone() {
        let heard = Arc::new(Conflicts::default());
        let listener: Arc<dyn EventListener> = heard.clone();
        let filter: Arc<dyn CompactionFilter> = Arc::new(DropX);
        let env: Arc<dyn Env> = Arc::clone(Options::memory().get_env());
        let options = Options::memory()
            .env(env)
            .listeners(vec![listener])
            .compaction_filter(Some(filter));
        let db = OptimisticTransactionDb::open("surface-prelude", options).unwrap();

        db.db().put(b"gone", b"x-rated").unwrap();
        db.db().put(b"kept", b"plain").unwrap();
        db.db().flush().unwrap();
        db.db().compact_range(None, None).wait().unwrap();
        assert_eq!(db.db().get(b"gone").unwrap(), None);
        assert_eq!(db.db().get(b"kept").unwrap(), Some(b"plain".to_vec()));

        let txn = db.begin(&TxnOptions::new().isolation(IsolationLevel::RepeatableRead));
        txn.get(b"kept").unwrap();
        db.db().put(b"kept", b"changed").unwrap();
        txn.put(b"other", b"v").unwrap();
        assert!(matches!(txn.commit(), Err(TransactionError::Conflict(_))));
        assert_eq!(heard.0.load(Ordering::Relaxed), 1);
    }
}
