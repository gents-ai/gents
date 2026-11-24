//! A database dropped while one of its own workers is still inside a job.
//!
//! A job's caller may drop its ticket and its database handle without
//! waiting. The worker running the job then holds the last reference to the
//! engine, so the engine is torn down on that worker's thread. The teardown
//! joins every worker, and a thread cannot join itself.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use regolith::{Db, Options};
use tempfile::TempDir;

fn numbered(i: u64) -> Vec<u8> {
    format!("key/{i:05}").into_bytes()
}

/// Poll `done` with a deadline and bounded backoff.
fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut backoff = Duration::from_micros(100);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        std::thread::sleep(backoff);
        backoff = (backoff * 2).min(Duration::from_millis(10));
    }
}

/// Holds a compaction at its start until told to go on, and says when one
/// began.
struct HoldCompaction {
    held: AtomicBool,
    began: std::sync::mpsc::SyncSender<()>,
    go_on: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
}

impl regolith::EventListener for HoldCompaction {
    fn on_compaction_begin(&self, _: &regolith::CompactionJobInfo) {
        // Only the first compaction is held.
        if !self.held.swap(true, Ordering::AcqRel) && self.began.try_send(()).is_ok() {
            // Bounded: a test that never says go on fails on its own wait.
            let _ = self
                .go_on
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(30));
        }
    }
}

/// A database dropped while its compaction worker is inside a job the caller
/// did not wait for. The worker holds the engine until the job ends, so the
/// engine is torn down on the worker's own thread: that must stop the other
/// workers, join no thread with itself, and release the directory, which
/// then opens again with every write.
#[test]
fn a_database_dropped_under_a_running_job_releases_its_directory_and_reopens() {
    // A panic on one of the database's own threads, which no join reports.
    static WORKER_PANICKED: AtomicBool = AtomicBool::new(false);
    let earlier = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if std::thread::current()
            .name()
            .is_some_and(|name| name.starts_with("regolith-"))
        {
            WORKER_PANICKED.store(true, Ordering::Release);
        }
        earlier(info);
    }));

    let dir = TempDir::new().unwrap();
    let (began_tx, began) = std::sync::mpsc::sync_channel(1);
    let (go_on, go_on_rx) = std::sync::mpsc::channel();
    let hold = Arc::new(HoldCompaction {
        held: AtomicBool::new(false),
        began: began_tx,
        go_on: std::sync::Mutex::new(go_on_rx),
    });
    // No pass starts by itself: the only compaction is the job's.
    let options = || {
        Options::default()
            .write_buffer_size(64 << 20)
            .max_background_compactions(2)
            .l0_compaction_trigger(1_000)
            .level0_slowdown_writes_trigger(0)
            .level0_stop_writes_trigger(0)
    };
    let db = Db::open(
        dir.path(),
        options().listeners(vec![hold as Arc<dyn regolith::EventListener>]),
    )
    .unwrap();
    for i in 0..400u64 {
        db.put(&numbered(i), &[7u8; 256]).unwrap();
        // Several level 0 tables, so the job has a compaction to run.
        if i % 100 == 99 {
            db.flush().unwrap();
        }
    }
    // The ticket is dropped: the job runs on, on a worker.
    drop(db.compact_range(None, None));
    began
        .recv_timeout(Duration::from_secs(30))
        .expect("the compaction began");
    drop(db);
    go_on.send(()).unwrap();

    let mut reopened = None;
    wait_until("the dropped database to release its directory", || {
        reopened = Db::open(dir.path(), options()).ok();
        reopened.is_some()
    });
    let db = reopened.unwrap();
    assert!(
        !WORKER_PANICKED.load(Ordering::Acquire),
        "a worker panicked tearing the database down"
    );
    for i in 0..400u64 {
        assert!(db.get(&numbered(i)).unwrap().is_some(), "key {i}");
    }
}
