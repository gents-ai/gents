//! A database torn down on its own compaction worker keeps its directory
//! until that worker has stopped.
//!
//! A caller that drops its last handle while a job it did not wait for runs
//! on the worker leaves the job holding the last reference: the engine is
//! torn down there, on the worker, when the job ends. The worker then still
//! has the rest of its round in front of it (the flush of frozen memtables
//! and a compaction pass), and either one writes tables and the manifest.
//! Another open of the directory must not be let in while it could, or two
//! engines write one directory. These tests leave such work behind the job,
//! and check, at every table the old worker writes and every compaction it
//! begins once the database is dropped, that the directory is still locked,
//! and that the worker does none of that work: it returns as soon as the job
//! that tore the engine down ends, and only then lets the directory go.
#![cfg(not(target_arch = "wasm32"))]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use regolith::{
    CompactionJobInfo, Db, Env, EventListener, Options, StdEnv, TableFileCreationInfo,
    TableFileCreationReason,
};
use tempfile::TempDir;

/// What the worker has left to do in its round once the job ends.
#[derive(Clone, Copy, Debug)]
enum LeftOver {
    /// A memtable sealed while the job ran, which the worker's round flushes.
    FrozenMemtable,
    /// Level 0 tables outside the job's range, past the compaction trigger,
    /// which the worker's round compacts.
    TablesOutsideTheRange,
}

/// Holds the first compaction, the job's, at its start until told to go on,
/// and says when it began. Once armed (the database dropped), checks every
/// table written and every compaction begun.
struct Watch {
    dir: PathBuf,
    held: AtomicBool,
    began: std::sync::mpsc::SyncSender<()>,
    go_on: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    armed: AtomicBool,
    /// A write found the directory unlocked.
    written_unlocked: AtomicBool,
    /// The worker went on with its round after the job: a flush, or a
    /// compaction of tables flushed while the job was held.
    went_on: AtomicBool,
    /// The tables flushed while the job was held: outside its range, so no
    /// compaction of the job's reads them.
    rest: std::sync::Mutex<Vec<u64>>,
}

impl Watch {
    /// The database is gone: only an engine that holds the directory's lock
    /// may write in it. `after_job` says the write is not the job's own.
    fn check(&self, after_job: bool) {
        if !self.armed.load(Ordering::Acquire) {
            return;
        }
        if after_job {
            self.went_on.store(true, Ordering::Release);
        }
        if StdEnv.lock_file(&self.dir, true).is_ok() {
            self.written_unlocked.store(true, Ordering::Release);
        }
    }
}

impl EventListener for Watch {
    fn on_compaction_begin(&self, info: &CompactionJobInfo) {
        let rest = self.rest.lock().unwrap();
        let after_job = info
            .input_files_input_level
            .iter()
            .any(|id| rest.contains(id));
        drop(rest);
        self.check(after_job);
        // Only the first compaction, the job's, is held.
        if !self.held.swap(true, Ordering::AcqRel) && self.began.try_send(()).is_ok() {
            // Bounded: a test that never says go on fails on its own wait.
            let _ = self
                .go_on
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(30));
        }
    }

    fn on_table_file_created(&self, info: &TableFileCreationInfo) {
        let flushed = info.reason == TableFileCreationReason::Flush;
        if flushed && self.held.load(Ordering::Acquire) && !self.armed.load(Ordering::Acquire) {
            self.rest.lock().unwrap().push(info.file_id);
        }
        self.check(flushed);
    }
}

fn key(prefix: &str, i: u64) -> Vec<u8> {
    format!("{prefix}/{i:05}").into_bytes()
}

/// Run `done` until it says so, with a deadline and a bounded backoff.
fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut backoff = Duration::from_micros(100);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        std::thread::sleep(backoff);
        backoff = (backoff * 2).min(Duration::from_millis(10));
    }
}

fn a_worker_keeps_the_directory_until_it_stops(left: LeftOver) {
    let dir = TempDir::new().unwrap();
    let (began_tx, began) = std::sync::mpsc::sync_channel(1);
    let (go_on, go_on_rx) = std::sync::mpsc::channel();
    let watch = Arc::new(Watch {
        dir: dir.path().to_path_buf(),
        held: AtomicBool::new(false),
        began: began_tx,
        go_on: std::sync::Mutex::new(go_on_rx),
        armed: AtomicBool::new(false),
        written_unlocked: AtomicBool::new(false),
        went_on: AtomicBool::new(false),
        rest: std::sync::Mutex::new(Vec::new()),
    });
    // One worker, so the work left behind the job is the tearing-down
    // worker's own. Small memtables, so the writes below seal one. No stall.
    let trigger = match left {
        LeftOver::FrozenMemtable => 1_000,
        LeftOver::TablesOutsideTheRange => 3,
    };
    let options = || {
        Options::default()
            .write_buffer_size(64 << 10)
            .max_background_compactions(1)
            .l0_compaction_trigger(trigger)
            .level0_slowdown_writes_trigger(0)
            .level0_stop_writes_trigger(0)
    };
    let db = Db::open(
        dir.path(),
        options().listeners(vec![Arc::clone(&watch) as Arc<dyn EventListener>]),
    )
    .unwrap();
    // Two overlapping level 0 tables in the job's range, below the trigger,
    // so the job's merge is the first compaction.
    for _ in 0..2 {
        for i in 0..100u64 {
            regolith::through_stalls(|| db.put(&key("job", i), &[7u8; 256])).unwrap();
        }
        db.flush().unwrap();
    }
    // The ticket is dropped: the job runs on, on the worker.
    drop(db.compact_range(Some(b"job/"), Some(b"job0")));
    began
        .recv_timeout(Duration::from_secs(30))
        .expect("the job's compaction began");

    // While the job is held, leave the worker more to do after it.
    for i in 0..300u64 {
        regolith::through_stalls(|| db.put(&key("rest", i), &[9u8; 256])).unwrap();
        if matches!(left, LeftOver::TablesOutsideTheRange) && i % 100 == 99 {
            db.flush().unwrap();
        }
    }
    match left {
        LeftOver::FrozenMemtable => {
            let all = db.get_int_property("regolith.cur-size-all-mem-tables");
            let active = db.get_int_property("regolith.cur-size-active-mem-table");
            assert!(all > active, "no sealed memtable waits for the worker");
        }
        LeftOver::TablesOutsideTheRange => {
            let l0 = db.get_int_property("regolith.num-files-at-level0").unwrap();
            assert!(l0 >= 2 + trigger as u64, "level 0 holds {l0} tables");
        }
    }

    drop(db);
    watch.armed.store(true, Ordering::Release);
    go_on.send(()).unwrap();

    let mut reopened = None;
    wait_until("the dropped database to release its directory", || {
        reopened = Db::open(dir.path(), options()).ok();
        reopened.is_some()
    });
    assert!(
        !watch.written_unlocked.load(Ordering::Acquire),
        "the old worker wrote in the directory after its lock was released ({left:?})"
    );
    assert!(
        !watch.went_on.load(Ordering::Acquire),
        "the worker went on with its round after the job that tore the database down ({left:?})"
    );
    let db = reopened.unwrap();
    for i in 0..100u64 {
        assert!(db.get(&key("job", i)).unwrap().is_some(), "job key {i}");
    }
    for i in 0..300u64 {
        assert!(db.get(&key("rest", i)).unwrap().is_some(), "rest key {i}");
    }
}

#[test]
fn a_database_dropped_under_a_job_keeps_its_directory_while_its_worker_flushes() {
    a_worker_keeps_the_directory_until_it_stops(LeftOver::FrozenMemtable);
}

#[test]
fn a_database_dropped_under_a_job_keeps_its_directory_while_its_worker_compacts() {
    a_worker_keeps_the_directory_until_it_stops(LeftOver::TablesOutsideTheRange);
}
