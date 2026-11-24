//! How the engine behaves when background work keeps failing.
//!
//! A failed compaction must not leave its outputs on disk: nothing
//! references them, so a compaction that kept failing (out of file
//! descriptors, say) used to leak a full set on every retry until the disk
//! filled. A background worker must also back off between failed passes
//! and report them through `regolith.background-errors`. A writer
//! stopped behind failing work must get that failure back rather than
//! wait forever, one that was already waiting when the work failed
//! included, and writes must resume once the fault clears.
#![cfg(not(target_arch = "wasm32"))]

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use regolith::env::{
    Capabilities, Env, FileLock, FileMeta, JoinHandle, ReadDir, ReadFile, StdEnv, WriteFile,
    WriteMode,
};
use regolith::{Db, Options};

/// Delegates to [`StdEnv`], but once armed lets only `budget` more
/// SSTables be created and fails the rest the way an exhausted file
/// descriptor table does.
#[derive(Debug)]
struct SstBudgetEnv {
    inner: StdEnv,
    remaining: AtomicUsize,
    refused: AtomicUsize,
    deny_sst_reads: AtomicBool,
    /// Set by [`Self::hold_next_sst`]: where the next table creation says it
    /// arrived, and what it waits on before it is answered.
    hold: Mutex<Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>>,
}

const UNLIMITED: usize = usize::MAX;

/// How long a test waits for another thread before it calls the wait a hang.
const DEADLINE: Duration = Duration::from_secs(60);

impl SstBudgetEnv {
    fn new() -> Self {
        Self {
            inner: StdEnv::new(),
            remaining: AtomicUsize::new(UNLIMITED),
            refused: AtomicUsize::new(0),
            deny_sst_reads: AtomicBool::new(false),
            hold: Mutex::new(None),
        }
    }

    fn allow_ssts(&self, budget: usize) {
        self.remaining.store(budget, Ordering::SeqCst);
    }

    /// Hold the next table creation before it is answered: the channel that
    /// says it arrived, and the one that lets it go.
    fn hold_next_sst(&self) -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
        let (arrived, when_arrived) = mpsc::channel();
        let (let_go, when_let_go) = mpsc::channel();
        *self.hold.lock().unwrap() = Some((arrived, when_let_go));
        (when_arrived, let_go)
    }

    fn take_sst(&self) -> bool {
        let mut n = self.remaining.load(Ordering::SeqCst);
        loop {
            let next = match n {
                UNLIMITED => return true,
                0 => return false,
                n => n - 1,
            };
            match self
                .remaining
                .compare_exchange(n, next, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => return true,
                Err(current) => n = current,
            }
        }
    }
}

#[cfg(unix)]
fn emfile() -> io::Error {
    io::Error::from_raw_os_error(24)
}

#[cfg(not(unix))]
fn emfile() -> io::Error {
    io::Error::other("Too many open files")
}

fn is_sst(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext == "sst")
}

impl Env for SstBudgetEnv {
    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        self.inner.create_dir_all(path)
    }
    fn read_dir(&self, path: &Path) -> io::Result<ReadDir<'_>> {
        self.inner.read_dir(path)
    }
    fn open_read(&self, path: &Path) -> io::Result<Box<dyn ReadFile>> {
        if is_sst(path) && self.deny_sst_reads.load(Ordering::SeqCst) {
            return Err(io::Error::other("Too many open files (os error 24)"));
        }
        self.inner.open_read(path)
    }
    fn open_log(
        &self,
        path: &Path,
        mode: WriteMode,
    ) -> io::Result<Box<dyn regolith::env::LogFile>> {
        self.inner.open_log(path, mode)
    }
    fn open_write(&self, path: &Path, mode: WriteMode) -> io::Result<Box<dyn WriteFile>> {
        if is_sst(path) {
            let held = self.hold.lock().unwrap().take();
            if let Some((arrived, when_let_go)) = held {
                let _ = arrived.send(());
                let _ = when_let_go.recv_timeout(DEADLINE);
            }
            if !self.take_sst() {
                self.refused.fetch_add(1, Ordering::SeqCst);
                return Err(emfile());
            }
        }
        self.inner.open_write(path, mode)
    }
    fn metadata(&self, path: &Path) -> io::Result<FileMeta> {
        self.inner.metadata(path)
    }
    fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.inner.remove_file(path)
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.inner.rename(from, to)
    }
    fn hard_link(&self, src: &Path, dst: &Path) -> io::Result<()> {
        self.inner.hard_link(src, dst)
    }
    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        self.inner.sync_dir(path)
    }
    fn lock_file(&self, path: &Path, exclusive: bool) -> io::Result<Box<dyn FileLock>> {
        self.inner.lock_file(path, exclusive)
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
        body: Box<dyn FnOnce() + Send + 'static>,
    ) -> io::Result<Box<dyn JoinHandle>> {
        self.inner.spawn(name, body)
    }
}

fn sst_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir.join("sst"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| is_sst(p))
        .collect();
    files.sort();
    files
}

fn key(i: u32) -> Vec<u8> {
    format!("key{i:08}").into_bytes()
}

#[test]
fn a_compaction_that_fails_midway_leaves_no_outputs_behind() {
    let dir = tempfile::tempdir().unwrap();
    let env = Arc::new(SstBudgetEnv::new());
    let opts = Options::default()
        .env(Arc::clone(&env) as Arc<dyn Env>)
        .max_background_compactions(0)
        .target_file_size(16 * 1024);
    let db = Db::open(dir.path(), opts).unwrap();

    for batch in 0..4u32 {
        for i in 0..2_000u32 {
            regolith::through_stalls(|| db.put(&key(batch * 2_000 + i), &[batch as u8; 64]))
                .unwrap();
        }
        db.flush().unwrap();
    }
    let live_before = sst_files(dir.path());
    assert!(live_before.len() >= 4, "setup must leave several L0 tables");

    // Room for one output, so the failure lands after a finished file.
    env.allow_ssts(1);
    for _ in 0..5 {
        assert!(db.compact_range(None, None).wait().is_err());
        assert_eq!(
            sst_files(dir.path()),
            live_before,
            "a failed compaction must leave exactly the live tables on disk"
        );
    }

    env.allow_ssts(UNLIMITED);
    db.compact_range(None, None).wait().unwrap();
    for i in 0..8_000u32 {
        assert!(db.get(&key(i)).unwrap().is_some(), "key {i} lost");
    }
}

#[test]
fn a_failing_background_worker_backs_off_and_counts_its_failures() {
    let dir = tempfile::tempdir().unwrap();
    let env = Arc::new(SstBudgetEnv::new());
    let opts = |l0_compaction_trigger| {
        Options::default()
            .env(Arc::clone(&env) as Arc<dyn Env>)
            .l0_compaction_trigger(l0_compaction_trigger)
    };

    let db = Db::open(dir.path(), opts(1_000)).unwrap();
    for batch in 0..6u32 {
        for i in 0..500u32 {
            regolith::through_stalls(|| db.put(&key(batch * 500 + i), &[batch as u8; 64])).unwrap();
        }
        db.flush().unwrap();
    }
    db.close().unwrap();
    drop(db);

    env.allow_ssts(0);
    let db = Db::open(dir.path(), opts(2)).unwrap();
    std::thread::sleep(Duration::from_secs(7));

    let failures = db.get_int_property("regolith.background-errors").unwrap();
    let attempts = env.refused.load(Ordering::SeqCst);
    assert!(failures >= 2, "expected repeated failures, saw {failures}");
    assert_eq!(failures as usize, attempts, "every failed pass is counted");
    // Unpaced, the worker retries on every one-second poll: about 7
    // attempts here. Backing off 1 s, 2 s, 4 s allows at most 4.
    assert!(attempts <= 4, "worker retried {attempts} times in 7 s");

    env.allow_ssts(UNLIMITED);
    for i in 0..3_000u32 {
        assert!(db.get(&key(i)).unwrap().is_some(), "key {i} lost");
    }
}

/// A flush writes its table, then opens a reader on it. When that open
/// fails (the descriptor table is full) the flush errors with the file
/// already on disk, and the next attempt allocates a fresh id.
#[test]
fn a_flush_that_fails_after_writing_its_table_leaves_no_file_behind() {
    let dir = tempfile::tempdir().unwrap();
    let env = Arc::new(SstBudgetEnv::new());
    let opts = Options::default()
        .env(Arc::clone(&env) as Arc<dyn Env>)
        .max_background_compactions(0);
    let db = Db::open(dir.path(), opts).unwrap();
    for i in 0..1_000u32 {
        regolith::through_stalls(|| db.put(&key(i), &[7u8; 64])).unwrap();
    }
    db.flush().unwrap();
    for i in 1_000..2_000u32 {
        regolith::through_stalls(|| db.put(&key(i), &[8u8; 64])).unwrap();
    }
    let live_before = sst_files(dir.path());

    env.deny_sst_reads.store(true, Ordering::SeqCst);
    for _ in 0..3 {
        assert!(db.flush().is_err());
        assert_eq!(
            sst_files(dir.path()),
            live_before,
            "a failed flush must leave exactly the live tables on disk"
        );
    }

    env.deny_sst_reads.store(false, Ordering::SeqCst);
    db.flush().unwrap();
    for i in 0..2_000u32 {
        assert!(db.get(&key(i)).unwrap().is_some(), "key {i} lost");
    }
}

/// Run `op` on another thread and fail the test if it has not returned
/// within `limit`: the regression these tests guard against is a hang.
fn within<T: Send + 'static>(limit: Duration, op: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(op());
    });
    rx.recv_timeout(limit)
        .expect("the write hung instead of returning")
}

fn wait_until(limit: Duration, mut ready: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + limit;
    while !ready() {
        assert!(std::time::Instant::now() < deadline, "condition never held");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_writer_stopped_behind_failing_compaction_gets_the_failure_instead_of_waiting() {
    let dir = tempfile::tempdir().unwrap();
    let env = Arc::new(SstBudgetEnv::new());
    let opts = |l0_compaction_trigger| {
        Options::default()
            .env(Arc::clone(&env) as Arc<dyn Env>)
            .l0_compaction_trigger(l0_compaction_trigger)
            .level0_slowdown_writes_trigger(0)
            .level0_stop_writes_trigger(4)
    };

    let db = Db::open(dir.path(), opts(1_000)).unwrap();
    for batch in 0..4u32 {
        for i in 0..200u32 {
            regolith::through_stalls(|| db.put(&key(batch * 200 + i), b"value")).unwrap();
        }
        db.flush().unwrap();
    }
    db.close().unwrap();
    drop(db);

    env.allow_ssts(0);
    let db = Arc::new(Db::open(dir.path(), opts(2)).unwrap());
    wait_until(Duration::from_secs(10), || {
        db.get_int_property("regolith.background-errors").unwrap() > 0
    });

    let writer = Arc::clone(&db);
    let result = within(Duration::from_secs(10), move || {
        writer.put(b"stalled", b"v")
    });
    match result {
        Err(regolith::Error::BackgroundFailed { job, source, .. }) => {
            assert_eq!(job, "compaction");
            #[cfg(unix)]
            assert_eq!(source.raw_os_error(), Some(24), "the cause must survive");
            #[cfg(not(unix))]
            let _ = source;
        }
        other => panic!("expected BackgroundFailed, got {other:?}"),
    }

    env.allow_ssts(UNLIMITED);
    let writer = Arc::clone(&db);
    within(Duration::from_secs(90), move || {
        wait_until(Duration::from_secs(85), || {
            writer.put(b"recovered", b"v").is_ok()
        })
    });
    assert!(db.get(b"recovered").unwrap().is_some());
    for i in 0..800u32 {
        assert!(db.get(&key(i)).unwrap().is_some(), "key {i} lost");
    }
}

/// A writer already waiting when the compaction behind its stop fails is
/// told then, and gets the failure a writer that arrives next gets. The
/// compaction is the worker's own and fails in the environment: it is held at
/// the table it is about to create until the writer waits, and that creation
/// is refused.
#[test]
fn a_writer_already_waiting_is_told_when_the_compaction_behind_its_stop_fails() {
    let dir = tempfile::tempdir().unwrap();
    let env = Arc::new(SstBudgetEnv::new());
    let opts = |l0_compaction_trigger| {
        Options::default()
            .env(Arc::clone(&env) as Arc<dyn Env>)
            .l0_compaction_trigger(l0_compaction_trigger)
            .level0_slowdown_writes_trigger(0)
            .level0_stop_writes_trigger(4)
    };

    let db = Db::open(dir.path(), opts(1_000)).unwrap();
    for batch in 0..4u32 {
        for i in 0..200u32 {
            regolith::through_stalls(|| db.put(&key(batch * 200 + i), b"value")).unwrap();
        }
        db.flush().unwrap();
    }
    db.close().unwrap();
    drop(db);

    // Reopened at the stop, with a worker whose only work is the compaction
    // that would relieve it.
    let (when_held, let_go) = env.hold_next_sst();
    env.allow_ssts(0);
    let db = Db::open(dir.path(), opts(2)).unwrap();
    when_held
        .recv_timeout(DEADLINE)
        .expect("the worker never started the compaction");
    assert_eq!(
        db.get_int_property("regolith.background-errors").unwrap(),
        0,
        "setup: nothing has failed yet"
    );

    let wait = match db.put(b"stopped", b"v") {
        Err(regolith::Error::WouldBlock(regolith::WouldBlock::Stall(wait))) => wait,
        other => panic!("the write was not stopped: {other:?}"),
    };
    assert!(!wait.is_ready());

    let (told, when_told) = mpsc::channel();
    std::thread::scope(|scope| {
        // What `through_stalls` does with the wait the write was handed.
        scope.spawn(|| {
            wait.wait();
            let outcome = regolith::through_stalls(|| db.put(b"stopped", b"v"));
            told.send(outcome).unwrap();
        });
        let_go.send(()).unwrap();
        let outcome = when_told.recv_timeout(DEADLINE);
        if outcome.is_err() {
            // Closing completes every wait, so the thread ends and this
            // test fails instead of hanging with it.
            let _ = db.close();
        }
        let told = outcome
            .expect("the writer was still waiting after the compaction behind its stop failed")
            .expect_err("the stop is on and its relief is failing");
        match &told {
            regolith::Error::BackgroundFailed { job, source, .. } => {
                assert_eq!(*job, "compaction");
                #[cfg(unix)]
                assert_eq!(source.raw_os_error(), Some(24), "the cause must survive");
                #[cfg(not(unix))]
                let _ = source;
            }
            other => panic!("expected BackgroundFailed, got {other:?}"),
        }
        assert_eq!(
            told.to_string(),
            db.put(b"fresh", b"v").unwrap_err().to_string()
        );
    });
    assert!(env.refused.load(Ordering::SeqCst) >= 1);
}

#[test]
fn a_writer_stopped_behind_failing_flushes_retries_them_and_recovers() {
    let dir = tempfile::tempdir().unwrap();
    let env = Arc::new(SstBudgetEnv::new());
    let db = Arc::new(
        Db::open(
            dir.path(),
            Options::default()
                .env(Arc::clone(&env) as Arc<dyn Env>)
                .write_buffer_size(64 * 1024)
                .max_write_buffer_number(1),
        )
        .unwrap(),
    );

    env.allow_ssts(0);
    let writer = Arc::clone(&db);
    let failure = within(Duration::from_secs(20), move || {
        for i in 0..100_000u32 {
            if let Err(e) = writer.put(&key(i), &[1u8; 256])
                && matches!(e, regolith::Error::BackgroundFailed { job: "flush", .. })
            {
                return Some(e);
            }
        }
        None
    });
    assert!(
        failure.is_some(),
        "writes behind failing flushes must hit BackgroundFailed"
    );

    env.allow_ssts(UNLIMITED);
    let writer = Arc::clone(&db);
    within(Duration::from_secs(20), move || {
        wait_until(Duration::from_secs(15), || {
            writer.put(b"recovered", b"v").is_ok()
        })
    });
    db.flush().unwrap();
    assert!(db.get(b"recovered").unwrap().is_some());
}

/// A blocking writer waits out a stop through `through_stalls`. When the
/// flushes behind it fail, the writer returns the failure: from the flush
/// it ran itself on the commit path, or as the background's. It does not
/// wait for a flush that keeps failing.
#[test]
fn a_writer_waiting_out_a_stop_is_told_when_the_flush_behind_it_fails() {
    let dir = tempfile::tempdir().unwrap();
    let env = Arc::new(SstBudgetEnv::new());
    let db = Arc::new(
        Db::open(
            dir.path(),
            Options::default()
                .env(Arc::clone(&env) as Arc<dyn Env>)
                .write_buffer_size(16 * 1024)
                .max_write_buffer_number(2),
        )
        .unwrap(),
    );

    env.allow_ssts(0);
    let writer = Arc::clone(&db);
    let failure = within(Duration::from_secs(30), move || {
        (0..100_000u32)
            .find_map(|i| regolith::through_stalls(|| writer.put(&key(i), &[1u8; 256])).err())
    });
    match failure {
        Some(regolith::Error::BackgroundFailed { job, .. }) => assert_eq!(job, "flush"),
        Some(regolith::Error::Io(_)) => {}
        other => panic!("expected the failing flush, got {other:?}"),
    }
}
