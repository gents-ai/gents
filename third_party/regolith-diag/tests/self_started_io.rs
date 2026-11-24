//! I/O regolith starts itself (D53, `NonBlocking.tla` SelfStart and
//! SelfIoNowhere): the bounded step a write leaves owing with no worker, and
//! the disk check, run on the queue of the thread whose call started them,
//! or inline, bounded, on a thread with no queue; never on a queue nobody
//! polls for them, never on a thread of their own, never on the open path.
#![cfg(not(target_arch = "wasm32"))]

use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use regolith::env::{
    Capabilities, DiskSpace, Env, FileLock, FileMeta, JoinHandle, ReadDir, ReadFile, StdEnv,
    WriteFile, WriteMode,
};
use regolith::{Db, IoBudget, Options, Statistics, Ticker};
use tempfile::TempDir;

/// A standard environment that counts how often the disk space is asked.
#[derive(Debug, Default)]
struct CountsDiskChecks {
    inner: StdEnv,
    checks: AtomicUsize,
}

impl Env for CountsDiskChecks {
    fn open_read(&self, p: &Path) -> io::Result<Box<dyn ReadFile>> {
        self.inner.open_read(p)
    }
    fn open_log(&self, p: &Path, mode: WriteMode) -> io::Result<Box<dyn regolith::env::LogFile>> {
        self.inner.open_log(p, mode)
    }
    fn open_write(&self, p: &Path, mode: WriteMode) -> io::Result<Box<dyn WriteFile>> {
        self.inner.open_write(p, mode)
    }
    fn create_dir_all(&self, p: &Path) -> io::Result<()> {
        self.inner.create_dir_all(p)
    }
    fn read_dir(&self, p: &Path) -> io::Result<ReadDir<'_>> {
        self.inner.read_dir(p)
    }
    fn metadata(&self, p: &Path) -> io::Result<FileMeta> {
        self.inner.metadata(p)
    }
    fn remove_file(&self, p: &Path) -> io::Result<()> {
        self.inner.remove_file(p)
    }
    fn rename(&self, a: &Path, b: &Path) -> io::Result<()> {
        self.inner.rename(a, b)
    }
    fn hard_link(&self, a: &Path, b: &Path) -> io::Result<()> {
        self.inner.hard_link(a, b)
    }
    fn sync_dir(&self, p: &Path) -> io::Result<()> {
        self.inner.sync_dir(p)
    }
    fn lock_file(&self, p: &Path, ex: bool) -> io::Result<Box<dyn FileLock>> {
        self.inner.lock_file(p, ex)
    }
    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }
    fn now_micros(&self) -> Option<u64> {
        self.inner.now_micros()
    }
    fn unix_secs(&self) -> Option<u64> {
        self.inner.unix_secs()
    }
    fn spawn(
        &self,
        name: &str,
        f: Box<dyn FnOnce() + Send + 'static>,
    ) -> io::Result<Box<dyn JoinHandle>> {
        self.inner.spawn(name, f)
    }
    fn disk_space(&self, p: &Path) -> io::Result<Option<DiskSpace>> {
        self.checks.fetch_add(1, Ordering::SeqCst);
        self.inner.disk_space(p)
    }
}

/// No worker, a 4 KiB buffer that two 1 KiB writes fill.
fn no_worker(stats: &Arc<Statistics>) -> Options {
    Options::default()
        .max_background_compactions(0)
        .write_buffer_size(4 * 1024)
        .statistics(Some(Arc::clone(stats)))
}

fn frozen(db: &Db) -> u64 {
    db.get_int_property("regolith.num-entries-imm-mem-tables")
        .unwrap()
}

/// Write 1 KiB values until a write seals the active memtable.
fn write_until_sealed(db: &Db, stats: &Statistics) {
    let flushes = stats.get_ticker(Ticker::FlushCount);
    for i in 0..64u32 {
        regolith::through_stalls(|| db.put(format!("k{i:04}").as_bytes(), &[7u8; 1024])).unwrap();
        if frozen(db) > 0 || stats.get_ticker(Ticker::FlushCount) > flushes {
            return;
        }
    }
    panic!("64 writes of 1 KiB never sealed a 4 KiB memtable");
}

/// A thread with a queue: the flush its write left owing waits on that
/// queue and runs at its poll, not inside the write.
#[test]
fn an_owed_flush_runs_at_the_writers_poll() {
    let dir = TempDir::new().unwrap();
    let stats = Arc::new(Statistics::new());
    let db = Db::open(dir.path(), no_worker(&stats)).unwrap();
    let mut queue = db.io_queue();
    write_until_sealed(&db, &stats);
    // The write that sealed returned; any write after it only queues the
    // owed step again, never runs it.
    regolith::through_stalls(|| db.put(b"after", b"v")).unwrap();
    assert!(frozen(&db) > 0, "the owed flush waits for the poll");
    assert_eq!(stats.get_ticker(Ticker::FlushCount), 0);
    queue.poll(IoBudget::ALL);
    assert_eq!(frozen(&db), 0, "the poll ran the owed flush");
    assert_eq!(stats.get_ticker(Ticker::FlushCount), 1);
}

/// A thread with no queue runs the owed step inline, once its write
/// returned from the pipeline, as before (E16).
#[test]
fn an_owed_flush_runs_inline_with_no_queue() {
    let dir = TempDir::new().unwrap();
    let stats = Arc::new(Statistics::new());
    let db = Db::open(dir.path(), no_worker(&stats)).unwrap();
    write_until_sealed(&db, &stats);
    regolith::through_stalls(|| db.put(b"after", b"v")).unwrap();
    assert_eq!(frozen(&db), 0, "the write after the seal flushed inline");
    assert_eq!(stats.get_ticker(Ticker::FlushCount), 1);
}

/// The owed step lands on the queue of the thread whose write started it:
/// another thread's poll does nothing for it.
#[test]
fn an_owed_step_lands_on_the_starting_threads_queue_only() {
    let dir = TempDir::new().unwrap();
    let stats = Arc::new(Statistics::new());
    let db = Db::open(dir.path(), no_worker(&stats)).unwrap();
    let mut mine = db.io_queue();
    write_until_sealed(&db, &stats);
    regolith::through_stalls(|| db.put(b"after", b"v")).unwrap();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let mut theirs = db.io_queue();
            assert_eq!(theirs.poll(IoBudget::ALL).completed, 0);
        });
    });
    assert!(frozen(&db) > 0);
    assert_eq!(mine.poll(IoBudget::ALL).completed, 1);
    assert_eq!(frozen(&db), 0);
}

/// A queue dropped before it ran the owed step leaves the work owed: the
/// next write takes it.
#[test]
fn a_dropped_queue_leaves_the_step_owed() {
    let dir = TempDir::new().unwrap();
    let stats = Arc::new(Statistics::new());
    let db = Db::open(dir.path(), no_worker(&stats)).unwrap();
    let queue = db.io_queue();
    write_until_sealed(&db, &stats);
    regolith::through_stalls(|| db.put(b"after", b"v")).unwrap();
    assert!(frozen(&db) > 0);
    drop(queue);
    regolith::through_stalls(|| db.put(b"next", b"v")).unwrap();
    assert_eq!(frozen(&db), 0, "the next write took the owed flush inline");
}

/// The disk check never runs on the open path. With no worker the first
/// write owes it, run on its thread's queue at its poll.
#[test]
fn the_disk_check_runs_at_the_first_writers_poll_with_no_worker() {
    let dir = TempDir::new().unwrap();
    let env = Arc::new(CountsDiskChecks::default());
    let db = Db::open(
        dir.path(),
        Options::default()
            .max_background_compactions(0)
            .env(Arc::clone(&env) as Arc<dyn Env>),
    )
    .unwrap();
    assert_eq!(env.checks.load(Ordering::SeqCst), 0, "not on the open path");
    let mut queue = db.io_queue();
    regolith::through_stalls(|| db.put(b"k", b"v")).unwrap();
    assert_eq!(env.checks.load(Ordering::SeqCst), 0, "not inside the write");
    queue.poll(IoBudget::ALL);
    assert_eq!(env.checks.load(Ordering::SeqCst), 1);
    regolith::through_stalls(|| db.put(b"k2", b"v")).unwrap();
    queue.poll(IoBudget::ALL);
    assert_eq!(env.checks.load(Ordering::SeqCst), 1, "once");
}

/// With a worker the disk check is the worker's.
#[test]
fn the_disk_check_runs_on_the_worker() {
    let dir = TempDir::new().unwrap();
    let env = Arc::new(CountsDiskChecks::default());
    let db = Db::open(
        dir.path(),
        Options::default().env(Arc::clone(&env) as Arc<dyn Env>),
    )
    .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let mut pause = std::time::Duration::from_millis(1);
    while env.checks.load(Ordering::SeqCst) == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "the worker never checked"
        );
        std::thread::sleep(pause);
        pause = (pause * 2).min(std::time::Duration::from_millis(50));
    }
    assert_eq!(env.checks.load(Ordering::SeqCst), 1);
    drop(db);
}
