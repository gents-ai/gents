//! Who relieves a write stall on a database with no compaction worker.
//!
//! - Nothing said about inline compaction: writers do. A database opened
//!   with `max_background_compactions(0)` and nothing else can relieve its
//!   own stops, so writes and commits go through them.
//! - Inline compaction turned off by hand: the caller does, with
//!   `compact_step`. A plain write a stop holds returns the stall's wait,
//!   whose message says what to call; a transaction's blocking commit, which
//!   cannot be made again, still relieves the stop itself, as far as the
//!   stop needs.
//! - With a worker, and nothing said: the worker does, and no writer's
//!   thread ever runs a compaction.
#![cfg(not(target_arch = "wasm32"))]

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread::ThreadId;

use regolith::{
    CompactionJobInfo, Db, Error, EventListener, OptimisticTransactionDb, Options, TxnOptions,
    WouldBlock,
};
use tempfile::TempDir;

/// On top of `options`: writes stop once L0 holds two tables, which is also
/// where a compaction of L0 is due, and a memtable holds a handful of
/// writes. No worker.
fn stops_at_two(options: Options) -> Options {
    options
        .max_background_compactions(0)
        .write_buffer_size(4 * 1024)
        .l0_compaction_trigger(2)
        .level0_slowdown_writes_trigger(0)
        .level0_stop_writes_trigger(2)
        .max_write_buffer_number(0)
}

fn l0(db: &Db) -> u64 {
    db.get_int_property("regolith.num-files-at-level0").unwrap()
}

/// Bring L0 to the stop with explicit flushes: the second flush is the last
/// thing done, so nothing has compacted the two tables yet.
fn stop(db: &Db) {
    for round in 0..2 {
        db.put(format!("fill{round}").as_bytes(), b"v").unwrap();
        db.flush().unwrap();
    }
    assert_eq!(l0(db), 2);
}

/// Records the thread each compaction began on.
#[derive(Default)]
struct WhoCompacts(Mutex<Vec<ThreadId>>);

impl WhoCompacts {
    fn threads(&self) -> Vec<ThreadId> {
        self.0.lock().unwrap().clone()
    }
}

impl EventListener for WhoCompacts {
    fn on_compaction_begin(&self, _: &CompactionJobInfo) {
        self.0.lock().unwrap().push(std::thread::current().id());
    }
}

fn listening(options: Options) -> (Arc<WhoCompacts>, Options) {
    let who = Arc::new(WhoCompacts::default());
    let options = options.listeners(vec![Arc::clone(&who) as Arc<dyn EventListener>]);
    (who, options)
}

/// No worker and nothing said: the stop holds when the first commit
/// arrives, and four hundred commits and four hundred plain writes against
/// a threshold they reach again and again all go through, on one thread,
/// with nobody else to compact.
#[test]
fn with_no_worker_and_nothing_said_writes_and_commits_go_through_a_stop() {
    let dir = TempDir::new().unwrap();
    let (who, options) = listening(stops_at_two(Options::default()));
    let db = OptimisticTransactionDb::open(dir.path(), options).unwrap();
    stop(db.db());
    let value = vec![7u8; 600];
    for i in 0..400u32 {
        let tx = db.begin(&TxnOptions::new());
        tx.put(format!("txn{i:05}").as_bytes(), &value).unwrap();
        tx.commit()
            .unwrap_or_else(|err| panic!("commit {i} of 400 failed: {err}"));
        db.db()
            .put(format!("put{i:05}").as_bytes(), &value)
            .unwrap_or_else(|err| panic!("put {i} of 400 failed: {err}"));
    }
    for i in [0u32, 199, 399] {
        assert_eq!(
            db.db().get(format!("txn{i:05}").as_bytes()).unwrap(),
            Some(value.clone())
        );
        assert_eq!(
            db.db().get(format!("put{i:05}").as_bytes()).unwrap(),
            Some(value.clone())
        );
    }
    let compacted = who.threads();
    assert!(
        compacted.len() > 1,
        "the writes met the stop more than once"
    );
    let me = std::thread::current().id();
    assert!(
        compacted.iter().all(|thread| *thread == me),
        "the writer relieved every stop itself"
    );
}

/// No worker, inline compaction turned off by hand: a plain write a stop
/// holds returns the stall's wait, the wait says what to call, and a
/// `compact_step` loop relieves the stop.
#[test]
fn with_inline_compaction_off_a_plain_write_gets_the_wait_and_compact_step_relieves_it() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(
        dir.path(),
        stops_at_two(Options::default()).inline_compaction(false),
    )
    .unwrap();
    stop(&db);
    let stopped = db.put(b"k", b"v").unwrap_err();
    assert_eq!(
        stopped.to_string(),
        "writes are stalled behind background work (stop: too many L0 files) that this \
         database leaves to its caller; call compact_step until the stall clears, or turn \
         inline_compaction on, then run the write again"
    );
    let wait = match stopped {
        Error::WouldBlock(WouldBlock::Stall(wait)) => wait,
        other => panic!("a stopped write returns its wait, got {other:?}"),
    };
    assert!(!wait.is_ready());
    assert_eq!(db.get(b"k").unwrap(), None, "the write applied nothing");
    let mut steps = 0;
    while !wait.is_ready() {
        db.compact_step().unwrap();
        steps += 1;
        assert!(steps <= 64, "compact_step never relieved the stop");
    }
    db.put(b"k", b"v").unwrap();
}

/// The single-threaded databases the next test runs on: the file system
/// with no worker, and an Env that starts no thread at all. Inline
/// compaction is off on both.
fn single_threaded(dir: &TempDir) -> Vec<(&'static str, PathBuf, Options)> {
    vec![
        (
            "the file system",
            dir.path().to_path_buf(),
            stops_at_two(Options::default()).inline_compaction(false),
        ),
        (
            "an Env with no threads",
            PathBuf::from("no-worker-stall-relief"),
            stops_at_two(Options::memory()).inline_compaction(false),
        ),
    ]
}

/// One thread, no worker, inline compaction off: a transaction's blocking
/// commit is not handed the wait, which nobody could complete for it. It
/// relieves the stop itself and commits; it runs the one compaction the
/// stop needs and no more; and the stop is gone afterwards. A plain write in
/// the state the commit found still returns the wait: the option governs
/// it.
#[test]
fn with_inline_compaction_off_a_blocking_commit_relieves_the_stop_as_far_as_it_needs() {
    let dir = TempDir::new().unwrap();
    for (on, path, options) in single_threaded(&dir) {
        let (who, options) = listening(options);
        let db = OptimisticTransactionDb::open(&path, options).unwrap();
        stop(db.db());
        assert!(
            matches!(
                db.db().put(b"plain", b"v"),
                Err(Error::WouldBlock(WouldBlock::Stall(_)))
            ),
            "on {on}: a plain write is handed the wait"
        );
        assert!(who.threads().is_empty(), "on {on}: nothing compacted yet");

        let tx = db.begin(&TxnOptions::new());
        tx.put(b"txn", b"v").unwrap();
        tx.commit()
            .unwrap_or_else(|err| panic!("on {on}: the commit did not go through the stop: {err}"));
        assert_eq!(db.db().get(b"txn").unwrap(), Some(b"v".to_vec()));
        assert_eq!(
            who.threads(),
            vec![std::thread::current().id()],
            "on {on}: one compaction, on the committing thread"
        );
        assert!(l0(db.db()) < 2, "on {on}: the stop is gone");
        db.db()
            .put(b"plain", b"v")
            .unwrap_or_else(|err| panic!("on {on}: a plain write is still stopped: {err}"));
    }
}

/// With a worker and nothing said about inline compaction, the worker
/// relieves every stop: of the compactions that ran, none began on a
/// thread that writes.
#[test]
fn with_a_worker_and_nothing_said_no_writer_thread_compacts() {
    let dir = TempDir::new().unwrap();
    let (who, options) = listening(
        Options::default()
            .write_buffer_size(4 * 1024)
            .l0_compaction_trigger(2)
            .level0_slowdown_writes_trigger(3)
            .level0_stop_writes_trigger(4),
    );
    let db = Arc::new(OptimisticTransactionDb::open(dir.path(), options).unwrap());
    let value = vec![7u8; 600];
    let writers: Vec<_> = (0..2)
        .map(|t| {
            let (db, value) = (Arc::clone(&db), value.clone());
            std::thread::spawn(move || {
                for i in 0..300u32 {
                    let key = format!("w{t}-{i:05}");
                    if i % 2 == 0 {
                        regolith::through_stalls(|| db.db().put(key.as_bytes(), &value)).unwrap();
                    } else {
                        let tx = db.begin(&TxnOptions::new());
                        tx.put(key.as_bytes(), &value).unwrap();
                        tx.commit().unwrap();
                    }
                }
                std::thread::current().id()
            })
        })
        .collect();
    let writers: HashSet<ThreadId> = writers
        .into_iter()
        .map(|writer| writer.join().unwrap())
        .collect();
    let compacted = who.threads();
    assert!(
        !compacted.is_empty(),
        "setup: the writes were to make compactions due"
    );
    let on_a_writer = compacted
        .iter()
        .filter(|thread| writers.contains(thread))
        .count();
    assert_eq!(
        on_a_writer,
        0,
        "{on_a_writer} of {} compactions began on a writer's thread",
        compacted.len()
    );
}
