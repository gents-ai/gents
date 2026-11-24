//! Foreground compaction: `max_background_compactions == 0`.
//!
//! Every test here runs a database with no background worker, which is
//! the only mode a single-threaded host such as `wasm32-wasip1` can
//! open in, with inline compaction on and no I/O queue. The invariant
//! under test throughout is that a writer never blocks on a signal that
//! nobody will send: it either does the compaction work on its own thread
//! or returns [`regolith::Error::Busy`].

// Native-only. wasm-pack builds every test target for wasm32, and these use
// threads, the filesystem or proptest, none of which exist there. The browser
// suite lives in tests/wasm_opfs*.rs.
#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use regolith::{
    CompactionJobInfo, CompactionOutcome, Db, Error, EventListener, FlushJobInfo, Options,
    WriteBatch, WriteOptions,
};

/// A small database whose memtable fills after a few writes, so tests
/// reach L0 and the stall thresholds in a bounded amount of work.
fn foreground_options() -> Options {
    Options::default()
        .write_buffer_size(32 * 1024)
        .block_size(4 * 1024)
        .block_cache_size(64 * 1024)
        .target_file_size(64 * 1024)
        .level_base_bytes(128 * 1024)
        .l0_compaction_trigger(2)
        .level0_slowdown_writes_trigger(4)
        .level0_stop_writes_trigger(8)
        .max_background_compactions(0)
        .inline_compaction(true)
}

fn value(i: usize) -> Vec<u8> {
    format!("{i:0>512}").into_bytes()
}

/// Counts the lifecycle callbacks the engine dispatches, so a test can
/// assert how much work a single write actually performed rather than
/// inferring it from timing.
#[derive(Default)]
struct JobCounter {
    flushes: AtomicUsize,
    compactions: AtomicUsize,
}

impl EventListener for JobCounter {
    fn on_flush_completed(&self, _info: &FlushJobInfo) {
        self.flushes.fetch_add(1, Ordering::Relaxed);
    }

    fn on_compaction_completed(&self, _info: &CompactionJobInfo) {
        self.compactions.fetch_add(1, Ordering::Relaxed);
    }
}

#[test]
fn open_succeeds_with_zero_background_compactions() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path(), foreground_options()).unwrap();
    regolith::through_stalls(|| db.put(b"k", b"v")).unwrap();
    assert_eq!(db.get(b"k").unwrap().as_deref(), Some(&b"v"[..]));
}

/// The wedge this PR exists to remove. Before foreground compaction,
/// zero workers meant L0 grew without bound until the stop trigger, at
/// which point the writer parked on a condvar that nothing would ever
/// signal: this test hung rather than failing.
#[test]
fn zero_workers_do_not_wedge_at_the_stop_trigger() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path(), foreground_options()).unwrap();

    // Enough writes to cycle the 32 KiB memtable many times over and
    // push L0 well past `level0_stop_writes_trigger`, with no
    // `compact_range` anywhere to rescue it.
    for i in 0..8_000usize {
        regolith::through_stalls(|| db.put(format!("key{i:06}").as_bytes(), &value(i))).unwrap();
    }

    for i in (0..8_000usize).step_by(97) {
        assert_eq!(
            db.get(format!("key{i:06}").as_bytes()).unwrap(),
            Some(value(i)),
            "key {i} lost"
        );
    }

    let l0: u64 = db.get_int_property("regolith.num-files-at-level0").unwrap();
    assert!(
        l0 < 8,
        "L0 grew to {l0} files, at or past the stop trigger: inline compaction is not keeping up"
    );
}

/// The stall path is bounded per call. A writer performs compaction
/// jobs itself, but a single `put` cannot turn into an unbounded
/// number of them.
#[test]
fn inline_compaction_bounds_the_work_one_write_performs() {
    let counter = Arc::new(JobCounter::default());
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(
        dir.path(),
        foreground_options().listeners(vec![counter.clone()]),
    )
    .unwrap();

    let mut worst = 0usize;
    for i in 0..4_000usize {
        let before = counter.compactions.load(Ordering::Relaxed);
        regolith::through_stalls(|| db.put(format!("key{i:06}").as_bytes(), &value(i))).unwrap();
        worst = worst.max(counter.compactions.load(Ordering::Relaxed) - before);
    }

    // `MAX_INLINE_PASSES` is 32 in the engine; a healthy run is far
    // below it because one L0 -> L1 pass drains the whole L0 pool.
    assert!(
        worst <= 32,
        "one write performed {worst} compaction jobs, past the inline cap"
    );
    assert!(
        counter.compactions.load(Ordering::Relaxed) > 0,
        "no compaction ran at all, so the bound proves nothing"
    );
    assert!(
        counter.flushes.load(Ordering::Relaxed) > 0,
        "no memtable was flushed, so the writes never reached L0"
    );
}

/// A snapshot pins every version, so compaction cannot free anything.
/// The writer must be told that, promptly, rather than waiting for a
/// worker that does not exist.
#[test]
fn a_stalled_write_reports_busy_instead_of_blocking() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(
        dir.path(),
        foreground_options()
            // Nothing can ever be compacted out of L0 into L1 while the
            // stop trigger is this low and a snapshot pins the data, so
            // the engine has to surface the dead end.
            .level0_stop_writes_trigger(2)
            .level0_slowdown_writes_trigger(2)
            .l0_compaction_trigger(64),
    )
    .unwrap();

    let _snapshot = db.snapshot();

    let deadline = Instant::now() + Duration::from_secs(60);
    let mut outcome = Ok(());
    for i in 0..4_000usize {
        outcome = db.put(format!("key{i:06}").as_bytes(), &value(i));
        if outcome.is_err() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "writes neither progressed nor failed within 60s: the writer is wedged"
        );
    }

    match outcome {
        Err(Error::Busy(_)) => {}
        Err(other) => panic!("expected Error::Busy, got {other:?}"),
        Ok(()) => panic!("expected the stall to surface as Error::Busy"),
    }
}

#[test]
fn no_slowdown_still_returns_busy_immediately() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(
        dir.path(),
        foreground_options()
            .level0_slowdown_writes_trigger(1)
            .level0_stop_writes_trigger(2)
            .l0_compaction_trigger(64),
    )
    .unwrap();

    let opts = WriteOptions {
        no_slowdown: true,
        ..WriteOptions::default()
    };

    let mut saw_busy = false;
    for i in 0..4_000usize {
        match db.put_opt(&opts, format!("key{i:06}").as_bytes(), &value(i)) {
            Ok(()) => {}
            Err(Error::Busy(_)) => {
                saw_busy = true;
                break;
            }
            Err(other) => panic!("expected Error::Busy, got {other:?}"),
        }
    }
    assert!(saw_busy, "no_slowdown never reported a stall");
}

/// A full memtable still becomes an L0 file with no worker running:
/// the rotating writer writes it inline.
#[test]
fn a_full_memtable_becomes_an_l0_file_with_no_worker() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(
        dir.path(),
        foreground_options()
            // High enough that nothing compacts L0 away underneath the
            // assertion below.
            .l0_compaction_trigger(64)
            .level0_slowdown_writes_trigger(0)
            .level0_stop_writes_trigger(0),
    )
    .unwrap();

    assert_eq!(db.get_int_property("regolith.num-files-at-level0"), Some(0));

    for i in 0..400usize {
        regolith::through_stalls(|| db.put(format!("key{i:06}").as_bytes(), &value(i))).unwrap();
    }

    assert!(
        db.get_int_property("regolith.num-files-at-level0").unwrap() > 0,
        "the memtable filled but no L0 file was written"
    );
}

#[test]
fn explicit_flush_writes_an_l0_file_and_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path(), foreground_options().l0_compaction_trigger(64)).unwrap();

    regolith::through_stalls(|| db.put(b"a", b"1")).unwrap();
    db.flush().unwrap();
    let after_first = db.get_int_property("regolith.num-files-at-level0").unwrap();
    assert_eq!(after_first, 1);

    // Nothing left in the memtable, so a second flush writes nothing.
    db.flush().unwrap();
    assert_eq!(
        db.get_int_property("regolith.num-files-at-level0").unwrap(),
        after_first
    );

    assert_eq!(db.get(b"a").unwrap().as_deref(), Some(&b"1"[..]));
}

#[test]
fn compact_step_reports_idle_did_work_and_contended_apart() {
    // Two 32 KiB memtables' worth of 512-byte values: one sealed by the
    // writes, one by the flush, and under the slowdown trigger of 4.
    const KEYS: usize = 100;
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path(), foreground_options()).unwrap();

    assert_eq!(
        db.compact_step().unwrap(),
        CompactionOutcome::Idle,
        "an empty database has nothing to compact, and must say Idle rather than Contended"
    );

    // With no worker the writes themselves compact once L0 reaches its
    // trigger (E16), so the tables are written with a trigger they never
    // reach, and the database is reopened at `l0_compaction_trigger` 2 to
    // give the picker two of them to merge with no write in between.
    drop(db);
    let db = Db::open(
        dir.path(),
        foreground_options().l0_compaction_trigger(1_000),
    )
    .unwrap();
    for i in 0..KEYS {
        regolith::through_stalls(|| db.put(format!("key{i:06}").as_bytes(), &value(i))).unwrap();
    }
    db.flush().unwrap();
    assert!(
        db.get_int_property("regolith.num-files-at-level0").unwrap() >= 2,
        "the writes left fewer than two tables to merge"
    );
    drop(db);
    let db = Db::open(dir.path(), foreground_options()).unwrap();

    assert_eq!(
        db.compact_step().unwrap(),
        CompactionOutcome::DidWork,
        "L0 is over the compaction trigger but compact_step found no work"
    );

    let mut steps = 0;
    while db.compact_step().unwrap() == CompactionOutcome::DidWork {
        steps += 1;
        assert!(steps < 1_000, "compact_step never drained");
    }

    // The reason this call returns an outcome rather than a bool. With no
    // background worker running there is nothing to contend with, so a
    // settled tree must report Idle exactly. A `bool` collapsed Idle and
    // Contended into one `false`, and a caller draining compaction could
    // not tell "the tree is settled" from "someone else holds the level".
    assert_eq!(
        db.compact_step().unwrap(),
        CompactionOutcome::Idle,
        "a drained tree with no worker running must report Idle, not Contended"
    );

    for i in (0..KEYS).step_by(37) {
        assert_eq!(
            db.get(format!("key{i:06}").as_bytes()).unwrap(),
            Some(value(i))
        );
    }
}

#[test]
fn compact_step_works_with_a_background_worker_running() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(
        dir.path(),
        foreground_options().max_background_compactions(2),
    )
    .unwrap();

    for i in 0..2_000usize {
        // Through stalls: the workers may fall behind these writes.
        regolith::through_stalls(|| db.put(format!("key{i:06}").as_bytes(), &value(i))).unwrap();
        if i % 250 == 0 {
            // Returns false when the workers got there first; either
            // answer is correct, and neither may corrupt the version.
            db.compact_step().unwrap();
        }
    }

    for i in (0..2_000usize).step_by(53) {
        assert_eq!(
            db.get(format!("key{i:06}").as_bytes()).unwrap(),
            Some(value(i))
        );
    }
}

/// The whole lifecycle the portability goal names, with no background
/// thread anywhere: open, put, get, delete, scan, snapshot, iterate,
/// flush, compact, close, reopen, read back.
#[test]
fn full_lifecycle_with_zero_workers() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path(), foreground_options()).unwrap();

    for i in 0..2_000usize {
        regolith::through_stalls(|| db.put(format!("key{i:06}").as_bytes(), &value(i))).unwrap();
    }

    let snapshot = db.snapshot();

    regolith::through_stalls(|| db.delete(b"key000005")).unwrap();
    assert_eq!(db.get(b"key000005").unwrap(), None);
    assert_eq!(
        snapshot.get(b"key000005").unwrap(),
        Some(value(5)),
        "the snapshot lost its point-in-time view"
    );

    let mut batch = WriteBatch::new();
    batch.put(b"batched", b"yes");
    batch.delete(b"key000006");
    regolith::through_stalls(|| db.write(batch.clone())).unwrap();
    assert_eq!(db.get(b"batched").unwrap().as_deref(), Some(&b"yes"[..]));
    assert_eq!(db.get(b"key000006").unwrap(), None);

    let scanned = db.scan(Some(b"key000100"), Some(b"key000110")).unwrap();
    assert_eq!(scanned.len(), 10);
    assert_eq!(scanned[0].0, b"key000100".to_vec());

    let mut iter = db.iter();
    iter.seek(b"key001000");
    let mut walked = 0;
    while iter.valid() && walked < 50 {
        assert!(iter.key().unwrap() >= b"key001000".as_slice());
        iter.next();
        walked += 1;
    }
    assert_eq!(walked, 50);
    drop(iter);
    drop(snapshot);

    db.flush().unwrap();
    db.compact_range(None, None).wait().unwrap();
    while db.compact_step().unwrap() == CompactionOutcome::DidWork {}

    db.close().unwrap();
    // The directory lock lives on the handle, not on the close, so the
    // reopen has to wait for the drop.
    drop(db);

    let db = Db::open(dir.path(), foreground_options()).unwrap();
    assert_eq!(db.get(b"key000000").unwrap(), Some(value(0)));
    assert_eq!(db.get(b"key001999").unwrap(), Some(value(1999)));
    assert_eq!(db.get(b"key000005").unwrap(), None);
    assert_eq!(db.get(b"key000006").unwrap(), None);
    assert_eq!(db.get(b"batched").unwrap().as_deref(), Some(&b"yes"[..]));
}

fn l0_tables(db: &Db) -> u64 {
    db.get_int_property("regolith.num-files-at-level0").unwrap()
}

/// A slowdown delays no write. What a slowdown threshold still decides, for
/// both of them (the count of level-0 tables and their bytes):
///
/// - a write that asked not to be slowed is refused with the reason;
/// - with no worker and inline compaction on, a write pays one step of the
///   compaction that is due, and with inline compaction off it does not.
///
/// A threshold that is off decides neither.
#[test]
fn a_slowdown_threshold_refuses_no_slowdown_writes_and_makes_a_write_pay_a_step() {
    let dir = tempfile::tempdir().unwrap();
    // Three level-0 tables, written with thresholds they never reach.
    let quiet = || {
        foreground_options()
            .l0_compaction_trigger(1_000)
            .level0_slowdown_writes_trigger(0)
            .level0_stop_writes_trigger(0)
            .soft_pending_compaction_bytes_limit(0)
            .hard_pending_compaction_bytes_limit(0)
    };
    {
        let db = Db::open(dir.path(), quiet()).unwrap();
        for table in 0..3usize {
            db.put(format!("key{table}").as_bytes(), &value(table))
                .unwrap();
            db.flush().unwrap();
        }
        assert_eq!(l0_tables(&db), 3);
    }
    let no_slowdown = WriteOptions {
        no_slowdown: true,
        ..WriteOptions::default()
    };
    let by_count = |options: Options| options.level0_slowdown_writes_trigger(3);
    let by_bytes = |options: Options| options.soft_pending_compaction_bytes_limit(1);
    let cases: [(&dyn Fn(Options) -> Options, &str); 2] = [
        (&by_count, "slowdown: L0 files over trigger"),
        (
            &by_bytes,
            "slowdown: pending compaction bytes over soft limit",
        ),
    ];

    for (threshold, reason) in cases {
        // Inline compaction off: nothing is compacted, a plain write goes
        // on at once, and a write that refuses to be slowed is told why.
        let db = Db::open(dir.path(), threshold(quiet()).inline_compaction(false)).unwrap();
        db.put(b"plain", b"v").unwrap();
        assert_eq!(l0_tables(&db), 3);
        match db.put_opt(&no_slowdown, b"refused", b"v") {
            Err(Error::Busy(told)) => assert_eq!(told, reason),
            other => panic!("expected the slowdown's reason, got {other:?}"),
        }
        assert_eq!(db.get(b"refused").unwrap(), None);
        drop(db);
    }

    // The threshold off, inline compaction on, a compaction due: a write
    // owes nothing, so the tables stay.
    let due = || quiet().l0_compaction_trigger(2).inline_compaction(true);
    let db = Db::open(dir.path(), due()).unwrap();
    db.put(b"plain", b"v").unwrap();
    db.put_opt(&no_slowdown, b"not-refused", b"v").unwrap();
    assert_eq!(l0_tables(&db), 3);
    drop(db);

    // The threshold on: the same write pays the step, which merges them.
    let db = Db::open(dir.path(), by_count(due())).unwrap();
    db.put(b"plain", b"w").unwrap();
    assert_eq!(
        l0_tables(&db),
        0,
        "the slowed write ran the pass that was due"
    );
    assert_eq!(db.get(b"key0").unwrap(), Some(value(0)));
    assert_eq!(db.get(b"plain").unwrap(), Some(b"w".to_vec()));
}
