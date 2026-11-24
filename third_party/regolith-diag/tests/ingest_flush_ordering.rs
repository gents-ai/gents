//! Deterministic proof that an ingest orders itself like a flush.
//!
//! `Db::ingest_external_files` copies its file with no lock held, then,
//! under the commit pipeline, flushes every memtable up to the newest one
//! holding a key of the file's range, allocates its sequence number and
//! installs the file before it releases the pipeline. So a flush of a
//! memtable holding one of its keys always installs before the ingested
//! file, a write issued while the ingest copies its file commits at once,
//! below the ingest's sequence, and a write issued while the ingest holds
//! the pipeline commits, rotates and flushes only after the file is
//! installed. Each test forces one interleaving, named in its doc, with a
//! seam that pauses or fails a real background write mid-flight, then
//! checks the read that interleaving gets wrong and the reopened sequence
//! counter that the manifest's raised-maximum rule protects on its own.
#![cfg(not(target_arch = "wasm32"))]

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle as StdJoinHandle;
use std::time::{Duration, Instant};

use regolith::env::{
    Capabilities, Env, FileLock, FileMeta, JoinHandle, ReadDir, ReadFile, StdEnv, WriteFile,
    WriteMode,
};
use regolith::{Db, IngestOptions, Options, SstFileWriter, WriteBatch};
use tempfile::TempDir;

/// How long a test lets the fixed engine prove the losing side of the
/// race did not finish on its own before the paused party is released.
/// After the fix the paused party is what the other thread is blocked
/// on, so nothing can finish first regardless of this value; the
/// deadline only bounds how long a broken engine gets to hide the bug.
const RELEASE_DEADLINE: Duration = Duration::from_secs(3);

/// Polls `done` with a bounded backoff (1 ms doubling to a 50 ms cap,
/// never a fixed sleep in a loop) until it returns true or `deadline`
/// passes. Returns whether it did.
fn wait_until(deadline: Duration, mut done: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    let mut backoff = Duration::from_millis(1);
    while !done() {
        if start.elapsed() >= deadline {
            return false;
        }
        std::thread::sleep(backoff);
        backoff = (backoff * 2).min(Duration::from_millis(50));
    }
    true
}

/// Whether `handle`'s thread finished within `deadline`.
fn wait_finished<T>(handle: &StdJoinHandle<T>, deadline: Duration) -> bool {
    wait_until(deadline, || handle.is_finished())
}

/// Waits for `handle` as [`wait_finished`] does, then calls `release`.
/// Every assertion in the tests that use it holds regardless of which
/// way the wait resolves.
fn release_when_finished_or_after<T>(
    handle: &StdJoinHandle<T>,
    deadline: Duration,
    release: impl FnOnce(),
) {
    wait_finished(handle, deadline);
    release();
}

/// Bytes held by frozen memtables: sealed, and not yet flushed.
fn frozen_bytes(db: &Db) -> u64 {
    let all = db
        .get_int_property("regolith.cur-size-all-mem-tables")
        .unwrap();
    let active = db
        .get_int_property("regolith.cur-size-active-mem-table")
        .unwrap();
    all - active
}

/// The bytes still frozen once the background has had time to write every
/// frozen memtable out: a rotation seals on the commit path and the
/// compaction worker flushes (E9). Waits with a deadline and a bounded
/// backoff, so a fast machine returns at once and a loaded one still does.
fn frozen_bytes_once_flushed(db: &Db) -> u64 {
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut pause = Duration::from_millis(1);
    loop {
        let frozen = frozen_bytes(db);
        if frozen == 0 || Instant::now() >= deadline {
            return frozen;
        }
        std::thread::sleep(pause);
        pause = (pause * 2).min(Duration::from_millis(50));
    }
}

/// Writes eight 1 KiB values under `prefix`. With a 4 KiB write buffer
/// that fills the active memtable and rotates it at least once, whatever
/// it already held.
fn write_past_one_rotation(db: &Db, prefix: &str) -> regolith::Result<()> {
    for i in 0..8 {
        regolith::through_stalls(|| db.put(format!("{prefix}{i}").as_bytes(), &[0u8; 1024]))?;
    }
    Ok(())
}

/// Wraps [`StdEnv`] and counts the calls to `open_write` whose parent
/// directory is the database's `sst` directory (SST file writes; WAL
/// and MANIFEST writes live elsewhere and are not counted). The
/// `pause_nth` call blocks until [`release`](Self::release) is called,
/// and the `fail_nth` call returns an injected error instead of opening
/// the file, after its pause when both name the same call. `0` matches
/// no call.
#[derive(Debug)]
struct NthSstOpen {
    inner: StdEnv,
    sst_dir: PathBuf,
    pause_nth: usize,
    fail_nth: usize,
    count: AtomicUsize,
    // (paused, released)
    state: Mutex<(bool, bool)>,
    cv: Condvar,
}

impl NthSstOpen {
    fn new(db_dir: &Path, pause_nth: usize, fail_nth: usize) -> Arc<Self> {
        Arc::new(Self {
            inner: StdEnv,
            sst_dir: db_dir.join("sst"),
            pause_nth,
            fail_nth,
            count: AtomicUsize::new(0),
            state: Mutex::new((false, false)),
            cv: Condvar::new(),
        })
    }

    /// Blocks until the `pause_nth` open has paused. Panics with a plain
    /// message if `deadline` passes first.
    fn wait_paused(&self, deadline: Duration) {
        let state = self.state.lock().unwrap();
        let (_state, result) = self
            .cv
            .wait_timeout_while(state, deadline, |s| !s.0)
            .unwrap();
        if result.timed_out() {
            panic!(
                "sst open never reached call {} within {deadline:?}",
                self.pause_nth
            );
        }
    }

    fn release(&self) {
        self.state.lock().unwrap().1 = true;
        self.cv.notify_all();
    }
}

impl Env for NthSstOpen {
    fn open_log(
        &self,
        path: &Path,
        mode: WriteMode,
    ) -> io::Result<Box<dyn regolith::env::LogFile>> {
        self.inner.open_log(path, mode)
    }
    fn open_write(&self, path: &Path, mode: WriteMode) -> io::Result<Box<dyn WriteFile>> {
        if path.parent() == Some(self.sst_dir.as_path()) {
            let n = self.count.fetch_add(1, Ordering::AcqRel) + 1;
            if n == self.pause_nth {
                let mut state = self.state.lock().unwrap();
                state.0 = true;
                self.cv.notify_all();
                while !state.1 {
                    state = self.cv.wait(state).unwrap();
                }
            }
            if n == self.fail_nth {
                return Err(io::Error::other("injected table open failure"));
            }
        }
        self.inner.open_write(path, mode)
    }
    fn create_dir_all(&self, p: &Path) -> io::Result<()> {
        self.inner.create_dir_all(p)
    }
    fn read_dir(&self, p: &Path) -> io::Result<ReadDir<'_>> {
        self.inner.read_dir(p)
    }
    fn open_read(&self, p: &Path) -> io::Result<Box<dyn ReadFile>> {
        self.inner.open_read(p)
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
}

/// Build a one-entry external SSTable ready for `ingest_external_files`.
fn build_ingest_file(dir: &Path, opts: &Options, key: &[u8], value: &[u8]) -> PathBuf {
    let path = dir.join("ingest.sst");
    let mut writer = SstFileWriter::create(&path, opts).unwrap();
    writer.put(key, value).unwrap();
    writer.finish().unwrap();
    path
}

/// Interleaving (a): a flush seals a memtable, releases the pipeline,
/// and is slow writing its table while an ingest for the same key
/// allocates a higher sequence and installs first.
#[test]
fn an_ingest_waits_for_a_flush_of_a_memtable_sealed_before_it() {
    let dir = TempDir::new().unwrap();
    let staging = TempDir::new().unwrap();
    // The flush's table is the first SST the database opens: it pauses
    // there, sealed and not installed.
    let env = NthSstOpen::new(dir.path(), 1, 0);
    let opts = Options::default().env(env.clone());
    let db = Arc::new(Db::open(dir.path(), opts.clone()).unwrap());

    regolith::through_stalls(|| db.put(b"k", b"old")).unwrap();

    let a = {
        let db = Arc::clone(&db);
        std::thread::spawn(move || db.flush())
    };
    env.wait_paused(Duration::from_secs(60));

    let ingest_path = build_ingest_file(staging.path(), &Options::default(), b"k", b"new");
    let b = {
        let db = Arc::clone(&db);
        std::thread::spawn(move || {
            db.ingest_external_files(&[ingest_path], IngestOptions::default())
                .wait()
        })
    };

    release_when_finished_or_after(&b, RELEASE_DEADLINE, || env.release());
    a.join().unwrap().unwrap();
    b.join().unwrap().unwrap();

    // The ingested version is the newest and must win. Before the fix,
    // L0 holds the flush's file above the ingested one (or the
    // ingested file sits at the deepest level under it) and this reads
    // "old".
    assert_eq!(db.get(b"k").unwrap(), Some(b"new".to_vec()));

    let before = db.latest_sequence();
    drop(db);

    let db = Db::open(dir.path(), Options::default()).unwrap();

    // Before the fix, `last_seq` was lowered to the flush's sealed
    // sequence and the flushed WAL had already been removed, so the
    // reopened counter restarted below the ingested sequence.
    let mut batch = WriteBatch::new();
    batch.put(b"z", b"v");
    let seq = regolith::through_stalls(|| db.write_sequenced(batch.clone())).unwrap();
    assert!(
        seq > before,
        "reopened sequence {seq} did not exceed {before}"
    );

    // Before the fix the horizon restarted below the ingested
    // sequence and the ingested entries were hidden after reopen.
    assert_eq!(db.get(b"k").unwrap(), Some(b"new".to_vec()));
}

/// Interleaving (b): an ingest holds the pipeline and is slow flushing
/// the memtable that holds a key it carries, while a writer commits a
/// newer version of that key and a flush of that write races the install.
/// The write cannot commit before the table is installed, so it, and the
/// flush of it, land above the ingested file.
#[test]
fn a_write_issued_during_an_ingest_lands_above_the_ingested_file() {
    let dir = TempDir::new().unwrap();
    let staging = TempDir::new().unwrap();
    // The first SST open is the ingest's copy of its file; the second is
    // its flush of the memtable holding `k`, under the pipeline.
    let env = NthSstOpen::new(dir.path(), 2, 0);
    let opts = Options::default().env(env.clone());
    let db = Arc::new(Db::open(dir.path(), opts.clone()).unwrap());

    regolith::through_stalls(|| db.put(b"k", b"old")).unwrap();

    let ingest_path = build_ingest_file(staging.path(), &opts, b"k", b"new");
    let b = {
        let db = Arc::clone(&db);
        std::thread::spawn(move || {
            db.ingest_external_files(&[ingest_path], IngestOptions::default())
        })
    };
    env.wait_paused(Duration::from_secs(60));

    let put = {
        let db = Arc::clone(&db);
        std::thread::spawn(move || regolith::through_stalls(|| db.put(b"k", b"v2")))
    };
    let a = {
        let db = Arc::clone(&db);
        std::thread::spawn(move || db.flush())
    };

    env.release();
    b.join().unwrap().wait().unwrap();
    put.join().unwrap().unwrap();
    a.join().unwrap().unwrap();

    // The write issued during the ingest is the newest. Had it committed
    // and been flushed before the ingested file (overlapping, so at L0)
    // was installed above it, this would read "new".
    assert_eq!(db.get(b"k").unwrap(), Some(b"v2".to_vec()));

    let before = db.latest_sequence();
    drop(db);

    let reopen_env = NthSstOpen::new(dir.path(), 0, 0);
    let reopen_opts = opts.env(reopen_env);
    let db = Db::open(dir.path(), reopen_opts).unwrap();

    // Before the fix the ingest's stamp was applied after the flush's
    // higher one, so the reopened counter restarted below `before`.
    let mut batch = WriteBatch::new();
    batch.put(b"z", b"v");
    let seq = regolith::through_stalls(|| db.write_sequenced(batch.clone())).unwrap();
    assert!(
        seq > before,
        "reopened sequence {seq} did not exceed {before}"
    );

    assert_eq!(db.get(b"k").unwrap(), Some(b"v2".to_vec()));
    assert_eq!(db.get(b"z").unwrap(), Some(b"v".to_vec()));
}

/// Interleaving (a) with no flush in progress: a flush that failed
/// leaves its memtable frozen, and nothing is writing it when the
/// ingest allocates. The ingest must install that memtable before its
/// own file, or the older value, still in the frozen list, answers the
/// read.
///
/// Deterministic: it runs on one thread, on a database with no worker.
/// (With one, the worker's first round, the one that runs the disk check
/// of the open, may reach the sealed memtable before this thread's flush
/// does and take the injected failure in its place, and the flush here
/// then goes through.) `Db::open` opens no SST, so the first SST open is
/// M1's flush, which fails. The ingest copies its file (the second open),
/// then retries that flush (the third) before it allocates: M1 lands at
/// L0, the ingest overlaps it and lands above it, and the read returns
/// "new". Without the flush the ingest lands at the deepest level while M1
/// stays frozen, and the read returns "old" from the frozen list.
#[test]
fn an_ingest_drains_a_memtable_left_frozen_by_a_failed_flush() {
    let dir = TempDir::new().unwrap();
    let staging = TempDir::new().unwrap();
    let opts = Options::default()
        .max_background_compactions(0)
        .env(NthSstOpen::new(dir.path(), 0, 1));
    let db = Db::open(dir.path(), opts.clone()).unwrap();

    regolith::through_stalls(|| db.put(b"k", b"old")).unwrap();
    assert!(
        db.flush().is_err(),
        "the injected table open failure must fail the flush"
    );

    let ingest_path = build_ingest_file(staging.path(), &opts, b"k", b"new");
    db.ingest_external_files(&[ingest_path], IngestOptions::default())
        .wait()
        .unwrap();

    // Without the drain, the ingest lands at the deepest level (the
    // version does not hold M1) and M1, still frozen, answers the
    // read: "old".
    assert_eq!(db.get(b"k").unwrap(), Some(b"new".to_vec()));

    let before = db.latest_sequence();
    drop(db);

    let db = Db::open(dir.path(), opts.env(NthSstOpen::new(dir.path(), 0, 0))).unwrap();

    // Without the maximum rule, M1's later stamp would lower
    // `last_seq` below the ingest's, and the reopened counter would
    // restart below `before`.
    let mut batch = WriteBatch::new();
    batch.put(b"z", b"v");
    let seq = regolith::through_stalls(|| db.write_sequenced(batch.clone())).unwrap();
    assert!(
        seq > before,
        "reopened sequence {seq} did not exceed {before}"
    );
    assert_eq!(db.get(b"k").unwrap(), Some(b"new".to_vec()));
}

/// A write that fills the memtable while an ingest copies its file is not
/// held back by the copy: it commits and rotates while the copy is paused.
/// The ingest draws its sequence afterwards, so its version of the key is
/// the newer one, and the memtable holding the write, sealed by the
/// rotation and written out off the commit path, is flushed before the
/// ingested file installs, so nothing is left frozen.
#[test]
fn a_write_that_fills_the_memtable_during_an_ingest_copy_commits_below_it() {
    let dir = TempDir::new().unwrap();
    let staging = TempDir::new().unwrap();
    // The first SST open is the flush of `old` below; the second is the
    // ingest's copy of its file.
    let env = NthSstOpen::new(dir.path(), 2, 0);
    let opts = Options::default()
        .env(env.clone())
        .write_buffer_size(4 * 1024);
    let db = Arc::new(Db::open(dir.path(), opts.clone()).unwrap());

    regolith::through_stalls(|| db.put(b"k", b"old")).unwrap();
    db.flush().unwrap();

    let ingest_path = build_ingest_file(staging.path(), &opts, b"k", b"new");
    let ingest = {
        let db = Arc::clone(&db);
        std::thread::spawn(move || {
            db.ingest_external_files(&[ingest_path], IngestOptions::default())
        })
    };
    env.wait_paused(Duration::from_secs(60));

    // `k = v2` goes into the memtable that the 1 KiB values then fill
    // and rotate.
    let writer = {
        let db = Arc::clone(&db);
        std::thread::spawn(move || {
            regolith::through_stalls(|| db.put(b"k", b"v2"))?;
            write_past_one_rotation(&db, "f")
        })
    };
    let committed_during_copy = wait_finished(&writer, Duration::from_secs(60));
    env.release();
    ingest.join().unwrap().wait().unwrap();
    writer.join().unwrap().unwrap();
    assert!(
        committed_during_copy,
        "a write waited for an ingest to copy its file"
    );

    assert_eq!(
        frozen_bytes_once_flushed(&db),
        0,
        "a rotation during an ingest left its memtable unflushed"
    );
    assert_eq!(db.get(b"k").unwrap(), Some(b"new".to_vec()));

    let before = db.latest_sequence();
    drop(db);

    let db = Db::open(dir.path(), opts.env(NthSstOpen::new(dir.path(), 0, 0))).unwrap();
    let mut batch = WriteBatch::new();
    batch.put(b"z", b"v");
    let seq = regolith::through_stalls(|| db.write_sequenced(batch.clone())).unwrap();
    assert!(
        seq > before,
        "reopened sequence {seq} did not exceed {before}"
    );
    assert_eq!(db.get(b"k").unwrap(), Some(b"new".to_vec()));
    assert_eq!(db.get(b"f0").unwrap(), Some(vec![0u8; 1024]));
}

/// An ingest that fails copying its file holds nothing the writes
/// issued while it ran wait for: they land, the memtable a rotation among
/// them seals is flushed as usual, and nothing is stranded in memory.
#[test]
fn writes_issued_during_an_ingest_that_fails_commit_once_it_gives_up() {
    let dir = TempDir::new().unwrap();
    let staging = TempDir::new().unwrap();
    // The ingest's copy is the first SST open: it pauses, then fails.
    let env = NthSstOpen::new(dir.path(), 1, 1);
    let opts = Options::default()
        .env(env.clone())
        .write_buffer_size(4 * 1024);
    let db = Arc::new(Db::open(dir.path(), opts.clone()).unwrap());

    let ingest_path = build_ingest_file(staging.path(), &opts, b"k", b"new");
    let ingest = {
        let db = Arc::clone(&db);
        std::thread::spawn(move || {
            db.ingest_external_files(&[ingest_path], IngestOptions::default())
        })
    };
    env.wait_paused(Duration::from_secs(60));

    let writer = {
        let db = Arc::clone(&db);
        std::thread::spawn(move || {
            regolith::through_stalls(|| db.put(b"k", b"v2"))?;
            write_past_one_rotation(&db, "f")
        })
    };
    env.release();
    let ingested = ingest.join().unwrap();
    writer.join().unwrap().unwrap();

    let err = ingested.wait().unwrap_err();
    assert!(
        err.to_string().contains("injected table open failure"),
        "unexpected ingest error: {err}"
    );
    assert_eq!(
        frozen_bytes_once_flushed(&db),
        0,
        "a rotation during a failed ingest left its memtable unflushed"
    );
    assert_eq!(db.get(b"k").unwrap(), Some(b"v2".to_vec()));

    drop(db);
    let db = Db::open(dir.path(), opts.env(NthSstOpen::new(dir.path(), 0, 0))).unwrap();
    assert_eq!(db.get(b"k").unwrap(), Some(b"v2".to_vec()));
    assert_eq!(db.get(b"f0").unwrap(), Some(vec![0u8; 1024]));
}
