//! The heap an open's directory walks, and a backup repository's, need does
//! not grow with the directories they walk.
//!
//! A counting global allocator tracks the bytes live on the heap and their
//! peak. Each test measures the peak an operation reaches over a quiet
//! directory, crowds the directory with thousands of entries the operation
//! must walk past or remove, and measures again: the second peak may not
//! pass the first by more than [`SLACK`]. A walk that collected its
//! directory, or kept every log or every suspect table it met, would hold
//! a copy of each entry's path at once, hundreds of kilobytes here.
//!
//! The databases run on an `Env` that starts no thread (no compaction
//! worker, no disk check), so the only allocations in a measurement are the
//! operation's own, in the same order each time. One test function per
//! measurement would let the harness run them side by side and mix their
//! allocations, so every measurement is a step of one test. This counts
//! bytes, it does not time anything: no benchmark.
#![cfg(not(target_arch = "wasm32"))]

use std::alloc::{GlobalAlloc, Layout, System};
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use regolith::env::{
    Capabilities, Env, FileLock, FileMeta, JoinHandle, ReadDir, ReadFile, StdEnv, WriteFile,
    WriteMode,
};
use regolith::{BackupEngine, BackupId, Db, Options, SstFileWriter};
use tempfile::TempDir;

/// How far a crowded run's peak may pass a quiet one's: well above the few
/// hundred bytes one entry costs while it is handled, and well below what
/// holding a crowded directory would cost.
const SLACK: usize = 64 * 1024;

/// Entries each crowded directory gains.
const MANY: usize = 6_000;

struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

// SAFETY: every call goes to the system allocator unchanged; the counters
// only observe the sizes it was asked for.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller's contract for `alloc` is passed through.
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            let live = LIVE.fetch_add(layout.size(), Ordering::SeqCst) + layout.size();
            PEAK.fetch_max(live, Ordering::SeqCst);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: the caller's contract for `dealloc` is passed through.
        unsafe { System.dealloc(ptr, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::SeqCst);
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// The most heap `f` held at once beyond what was live when it started.
fn peak_during(f: impl FnOnce()) -> usize {
    let base = LIVE.load(Ordering::SeqCst);
    PEAK.store(base, Ordering::SeqCst);
    f();
    PEAK.load(Ordering::SeqCst).saturating_sub(base)
}

/// `StdEnv` without threads, so nothing allocates beside a measurement.
#[derive(Debug)]
struct NoThreads(StdEnv);

impl Env for NoThreads {
    fn spawn(
        &self,
        _name: &str,
        _body: Box<dyn FnOnce() + Send + 'static>,
    ) -> io::Result<Box<dyn JoinHandle>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "no threads here",
        ))
    }
    fn capabilities(&self) -> Capabilities {
        self.0.capabilities().with_threads(false)
    }
    fn read_dir(&self, path: &Path) -> io::Result<ReadDir<'_>> {
        self.0.read_dir(path)
    }
    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        self.0.create_dir_all(path)
    }
    fn open_read(&self, path: &Path) -> io::Result<Box<dyn ReadFile>> {
        self.0.open_read(path)
    }
    fn open_log(
        &self,
        path: &Path,
        mode: WriteMode,
    ) -> io::Result<Box<dyn regolith::env::LogFile>> {
        self.0.open_log(path, mode)
    }
    fn open_write(&self, path: &Path, mode: WriteMode) -> io::Result<Box<dyn WriteFile>> {
        self.0.open_write(path, mode)
    }
    fn metadata(&self, path: &Path) -> io::Result<FileMeta> {
        self.0.metadata(path)
    }
    fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.0.remove_file(path)
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.0.rename(from, to)
    }
    fn hard_link(&self, src: &Path, dst: &Path) -> io::Result<()> {
        self.0.hard_link(src, dst)
    }
    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        self.0.sync_dir(path)
    }
    fn lock_file(&self, path: &Path, exclusive: bool) -> io::Result<Box<dyn FileLock>> {
        self.0.lock_file(path, exclusive)
    }
    fn now_micros(&self) -> Option<u64> {
        self.0.now_micros()
    }
    fn unix_secs(&self) -> Option<u64> {
        self.0.unix_secs()
    }
}

fn options(env: &Arc<dyn Env>) -> Options {
    Options::default()
        .write_buffer_size(16 * 1024)
        .max_background_compactions(0)
        .env(Arc::clone(env))
}

fn put_some(db: &Db) {
    for i in 0..100 {
        regolith::through_stalls(|| db.put(format!("key_{i:06}").as_bytes(), &[7u8; 64])).unwrap();
    }
}

/// The peak one open and close of the database at `dir` reaches.
fn open_peak(env: &Arc<dyn Env>, dir: &Path) -> usize {
    peak_during(|| {
        let db = Db::open(dir, options(env)).unwrap();
        db.close().unwrap();
    })
}

/// The peak an open of the database at `dir` reaches before it refuses.
fn refused_open_peak(env: &Arc<dyn Env>, dir: &Path) -> usize {
    peak_during(|| {
        assert!(Db::open(dir, options(env)).is_err());
    })
}

fn crowd_the_open(dir: &Path) {
    let sst = dir.join("sst");
    let wal = dir.join("wal");
    for k in 0..MANY {
        // Tables renamed aside, which the sweep removes; staging files,
        // which the open removes; and names no log has.
        std::fs::write(sst.join(format!("000001.sst.removed-{k}")), b"x").unwrap();
        std::fs::write(wal.join(format!("wal_{k:06}.tmp")), b"x").unwrap();
        std::fs::write(wal.join(format!("note-{k}")), b"x").unwrap();
    }
    // Log 0, older than any open's minimum, under every zero padding a
    // name has room for: retired logs recovery passes over and the open
    // removes.
    for zeros in 1..=240 {
        for ext in ["log", "wal"] {
            std::fs::write(wal.join(format!("wal_{}.{ext}", "0".repeat(zeros))), b"x").unwrap();
        }
    }
}

/// The peak a delete of a backup that is not there reaches: the
/// collection alone, sweeping `shared/`.
fn sweep_peak(engine: &mut BackupEngine) -> usize {
    peak_during(|| engine.delete_backup(BackupId(999_999)).unwrap())
}

/// A table that carries data, written through the public writer.
fn table_bytes(dir: &Path) -> Vec<u8> {
    let path = dir.join("fixture.sst");
    let mut writer = SstFileWriter::create(&path, &Options::default()).unwrap();
    writer.put(b"a", b"b").unwrap();
    writer.finish().unwrap();
    std::fs::read(&path).unwrap()
}

#[test]
fn walks_need_no_more_heap_for_a_crowded_directory() {
    let env: Arc<dyn Env> = Arc::new(NoThreads(StdEnv::new()));

    // An open: the staged-log removal, the recovery scan, the orphan sweep
    // and the retired-log removal each walk a directory.
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), options(&env)).unwrap();
    put_some(&db);
    db.flush().unwrap();
    db.close().unwrap();
    drop(db);
    // Two quiet opens first, so the one measured finds the state every
    // later open finds: one log, nothing to retire twice.
    open_peak(&env, dir.path());
    let quiet = open_peak(&env, dir.path());
    crowd_the_open(dir.path());
    let crowded = open_peak(&env, dir.path());
    assert!(
        crowded <= quiet + SLACK,
        "an open over a crowded directory peaked at {crowded} bytes, a quiet one at {quiet}"
    );
    let left = std::fs::read_dir(dir.path().join("wal")).unwrap().count();
    assert_eq!(
        left,
        MANY + 1,
        "the open left more than the notes and its log"
    );

    // The discarded-table guard, over a headerless manifest: a few
    // suspects, then thousands.
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), options(&env)).unwrap();
    db.close().unwrap();
    drop(db);
    std::fs::write(dir.path().join("MANIFEST"), b"").unwrap();
    let table = table_bytes(dir.path());
    let sst = dir.path().join("sst");
    for k in 0..9 {
        std::fs::write(sst.join(format!("{:06}.sst", 1_000 + k)), &table).unwrap();
    }
    refused_open_peak(&env, dir.path());
    let quiet = refused_open_peak(&env, dir.path());
    for k in 9..3_000 {
        std::fs::write(sst.join(format!("{:06}.sst", 1_000 + k)), &table).unwrap();
    }
    let crowded = refused_open_peak(&env, dir.path());
    assert!(
        crowded <= quiet + SLACK,
        "the guard over 3000 suspects peaked at {crowded} bytes, over 9 at {quiet}"
    );

    // A backup listing and a delete's collection: a page of ids and one
    // listing at a time, so a repository past one page needs what a
    // repository of many pages needs. The backups past the first are
    // copies of its plaintext metadata, which binds no id.
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path().join("db"), options(&env)).unwrap();
    put_some(&db);
    db.flush().unwrap();
    let mut engine =
        BackupEngine::open_with_env(dir.path().join("backups"), Arc::clone(&env)).unwrap();
    engine.create_backup(&db).unwrap();
    drop(db);
    let meta = dir.path().join("backups").join("meta");
    let copy_up_to = |last: u64| {
        for id in 2..=last {
            let to = meta.join(format!("{id:06}.backup"));
            if !to.exists() {
                std::fs::copy(meta.join("000001.backup"), to).unwrap();
            }
        }
    };
    let list_peak = |engine: &BackupEngine| {
        peak_during(|| assert!(engine.list_backups().all(|b| matches!(b, Ok(Ok(_))))))
    };
    copy_up_to(1_100);
    list_peak(&engine);
    let quiet = list_peak(&engine);
    let quiet_delete = peak_during(|| engine.delete_backup(BackupId(1_100)).unwrap());
    copy_up_to(MANY as u64);
    let crowded = list_peak(&engine);
    assert!(
        crowded <= quiet + SLACK,
        "listing {MANY} backups peaked at {crowded} bytes, 1100 at {quiet}"
    );
    let crowded_delete = peak_during(|| engine.delete_backup(BackupId(MANY as u64)).unwrap());
    assert!(
        crowded_delete <= quiet_delete + SLACK,
        "a delete beside {MANY} backups peaked at {crowded_delete} bytes, beside 1100 at {quiet_delete}"
    );

    // A collection's sweep of `shared/`: one batch of candidate tables and
    // one listing at a time, so thousands of tables no backup lists, many
    // batches of them, need what a few need. Holding them all would cost a
    // quarter of a megabyte of content ids here.
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path().join("db"), options(&env)).unwrap();
    put_some(&db);
    db.flush().unwrap();
    let mut engine =
        BackupEngine::open_with_env(dir.path().join("backups"), Arc::clone(&env)).unwrap();
    engine.create_backup(&db).unwrap();
    drop(db);
    let shared = dir.path().join("backups").join("shared");
    let listed = std::fs::read_dir(&shared).unwrap().count();
    assert!(listed > 0);
    // Tables no backup lists, under the names the engine gives shared
    // tables, as backups cut short leave them.
    let orphans = |n: u128| {
        for k in 0..n {
            std::fs::write(shared.join(format!("{:032x}.sst", 0xF000_0000 + k)), b"x").unwrap();
        }
    };
    orphans(16);
    sweep_peak(&mut engine);
    orphans(16);
    let quiet = sweep_peak(&mut engine);
    orphans(8 * 1024);
    let crowded = sweep_peak(&mut engine);
    assert!(
        crowded <= quiet + SLACK,
        "a sweep of {} unlisted tables peaked at {crowded} bytes, of 16 at {quiet}",
        8 * 1024
    );
    assert_eq!(
        std::fs::read_dir(&shared).unwrap().count(),
        listed,
        "the sweep left a table no backup lists, or removed a listed one"
    );
}
